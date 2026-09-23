// Shared verbatim by the VM and native runtime; keep this module std-only.
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicI64, Ordering};

pub const ABI: u64 = 1;
pub const LIMIT: usize = 16 * 1024 * 1024;
type Describe = unsafe extern "C" fn(u32) -> u64;
type Call =
    unsafe extern "C" fn(u32, *mut u8, *const u8, u64, *mut u8, u64, *mut u64, *mut *mut u8) -> i32;
type Destroy = unsafe extern "C" fn(u32, *mut u8);
type Close = unsafe extern "C" fn(u32, *mut u8, *mut u8) -> i64;

struct Library {
    _module: Module,
    schema: String,
    describe: Describe,
    call: Call,
    destroy: Destroy,
    close: Close,
}
struct Resource {
    library: Rc<Library>,
    kind: u32,
    pointer: *mut u8,
}
impl Drop for Resource {
    fn drop(&mut self) {
        if !self.pointer.is_null() {
            unsafe { (self.library.destroy)(self.kind, self.pointer) };
        }
    }
}
#[derive(Default)]
struct State {
    libraries: HashMap<String, Rc<Library>>,
    resources: HashMap<i64, Resource>,
}
thread_local! { static STATE: RefCell<State> = RefCell::new(State::default()); }
// Tokens are never reused, even between threads or successive VM executions.
static NEXT_TOKEN: AtomicI64 = AtomicI64::new(1);

pub fn encode(bytes: &[u8]) -> String {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 15) as usize] as char);
    }
    result
}
pub fn decode(text: &str) -> Result<Vec<u8>, String> {
    if text.len() > LIMIT * 2 || text.len() % 2 != 0 {
        return Err("invalid C wire payload length".into());
    }
    fn digit(x: u8) -> Result<u8, String> {
        match x {
            b'0'..=b'9' => Ok(x - b'0'),
            b'a'..=b'f' => Ok(x - b'a' + 10),
            _ => Err("invalid C wire hexadecimal digit".into()),
        }
    }
    text.as_bytes()
        .chunks_exact(2)
        .map(|c| Ok(digit(c[0])? * 16 + digit(c[1])?))
        .collect()
}
pub fn int(text: &str) -> Result<i64, String> {
    let bytes = decode(text)?;
    Ok(i64::from_le_bytes(bytes.try_into().map_err(
        |_| "C scalar must contain exactly eight bytes",
    )?))
}
fn response(result: Result<Vec<u8>, String>) -> String {
    match result {
        Ok(bytes) => format!("00{}", encode(&bytes)),
        Err(error) => format!("01{}", encode(error.as_bytes())),
    }
}
pub fn remote_error() -> String {
    response(Err(
        "C bridge calls are not supported inside remote tasks".into()
    ))
}

/// `path` must be an absolute, explicitly built bridge. Operation metadata is
/// checked before passing a resource pointer to C. No pointer comes from Foster.
pub fn exchange(
    path: &str,
    schema: &str,
    operation: i64,
    token: i64,
    create: bool,
    payload: &str,
) -> String {
    response((|| {
        let input = decode(payload)?;
        let operation = u32::try_from(operation).map_err(|_| "C operation is outside u32")?;
        STATE.with(|state| {
            let mut state = state
                .try_borrow_mut()
                .map_err(|_| "C callbacks/reentrant calls are unsupported")?;
            let library = if token == 0 {
                if !Path::new(path).is_absolute() {
                    return Err("C bridge path must be absolute".into());
                }
                if let Some(library) = state.libraries.get(path) {
                    library.clone()
                } else {
                    let library = Rc::new(Library::load(path)?);
                    state.libraries.insert(path.to_owned(), library.clone());
                    library
                }
            } else {
                state
                    .resources
                    .get(&token)
                    .ok_or("C resource is closed or belongs to another thread")?
                    .library
                    .clone()
            };
            if token == 0 && schema != library.schema {
                return Err(
                    "C bridge binding schema mismatch; rebuild the bindings and program together"
                        .into(),
                );
            }
            let metadata = unsafe { (library.describe)(operation) };
            let mode = metadata as u32 & 255;
            let kind = (metadata >> 32) as u32;
            if (mode == 2) != create {
                return Err("C constructor requires an owning resource result".into());
            }
            let pointer = match (mode, token) {
                (1 | 2, 0) => std::ptr::null_mut(),
                (3, _) if token != 0 => {
                    let resource = &state.resources[&token];
                    if resource.kind != kind {
                        return Err("C operation requires a different resource type".into());
                    }
                    resource.pointer
                }
                _ => return Err("unknown C operation or incompatible resource receiver".into()),
            };
            let mut output = vec![0u8; if metadata & 256 != 0 { LIMIT } else { 8 }];
            let mut length = 0;
            let mut created = std::ptr::null_mut();
            let status = unsafe {
                (library.call)(
                    operation,
                    pointer,
                    input.as_ptr(),
                    input.len() as u64,
                    output.as_mut_ptr(),
                    output.len() as u64,
                    &mut length,
                    &mut created,
                )
            };
            // Adopt immediately so every subsequent failure destroys the result.
            let resource = if mode == 2 && !created.is_null() {
                Some(Resource {
                    library: library.clone(),
                    kind,
                    pointer: created,
                })
            } else {
                None
            };
            if status != 0 {
                return Err(format!("C bridge operation {operation} failed ({status})"));
            }
            if length > output.len() as u64 {
                return Err("C bridge returned an invalid output length".into());
            }
            if mode == 2 {
                let resource = resource.ok_or("C constructor returned null")?;
                let token = NEXT_TOKEN
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
                    .map_err(|_| "C resource token space exhausted")?;
                state.resources.insert(token, resource);
                Ok(token.to_le_bytes().to_vec())
            } else {
                output.truncate(length as usize);
                Ok(output)
            }
        })
    })())
}

/// Returns [consumed byte, signed little-endian status]. A failed close may
/// retain ownership; the descriptor's close contract decides this, not its sign.
pub fn close(token: i64) -> String {
    response(STATE.with(|state| {
        let mut state = state
            .try_borrow_mut()
            .map_err(|_| "C callbacks/reentrant calls are unsupported")?;
        let resource = state
            .resources
            .get_mut(&token)
            .ok_or("C resource is closed or belongs to another thread")?;
        let mut consumed = 0;
        let status =
            unsafe { (resource.library.close)(resource.kind, resource.pointer, &mut consumed) };
        if consumed != 0 {
            resource.pointer = std::ptr::null_mut();
            state.resources.remove(&token);
        }
        let mut result = vec![u8::from(consumed != 0)];
        result.extend_from_slice(&status.to_le_bytes());
        Ok(result)
    }))
}
pub fn release(token: i64) -> Result<(), String> {
    if token == 0 {
        return Ok(());
    }
    STATE.with(|state| {
        let resource = state
            .try_borrow_mut()
            .map_err(|_| "C callbacks/reentrant calls are unsupported")?
            .resources
            .remove(&token);
        drop(resource.ok_or("C resource is closed or belongs to another thread")?);
        Ok(())
    })
}

impl Library {
    fn load(path: &str) -> Result<Self, String> {
        let module = Module::load(path)?;
        unsafe {
            let version: unsafe extern "C" fn() -> u64 =
                std::mem::transmute(module.symbol(b"foster_c_abi\0")?);
            if version() != ABI {
                return Err("C bridge ABI version mismatch".into());
            }
            let schema: unsafe extern "C" fn() -> u64 =
                std::mem::transmute(module.symbol(b"foster_c_schema\0")?);
            Ok(Self {
                schema: format!("{:016x}", schema()),
                describe: std::mem::transmute(module.symbol(b"foster_c_describe\0")?),
                call: std::mem::transmute(module.symbol(b"foster_c_call\0")?),
                destroy: std::mem::transmute(module.symbol(b"foster_c_destroy\0")?),
                close: std::mem::transmute(module.symbol(b"foster_c_close\0")?),
                _module: module,
            })
        }
    }
}
#[cfg(all(windows, target_arch = "x86_64"))]
struct Module(*mut std::ffi::c_void);
#[cfg(all(windows, target_arch = "x86_64"))]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryExW(
        path: *const u16,
        file: *mut std::ffi::c_void,
        flags: u32,
    ) -> *mut std::ffi::c_void;
    fn GetProcAddress(module: *mut std::ffi::c_void, name: *const u8) -> *mut std::ffi::c_void;
    fn FreeLibrary(module: *mut std::ffi::c_void) -> i32;
}
#[cfg(all(windows, target_arch = "x86_64"))]
impl Module {
    fn load(path: &str) -> Result<Self, String> {
        if path.contains('\0') {
            return Err("C bridge path contains NUL".into());
        }
        let wide: Vec<_> = path.encode_utf16().chain(Some(0)).collect();
        // Search dependencies only beside the bridge and in System32, never cwd.
        let module = unsafe { LoadLibraryExW(wide.as_ptr(), std::ptr::null_mut(), 0x100 | 0x800) };
        if module.is_null() {
            Err(format!(
                "cannot load C bridge: {}",
                std::io::Error::last_os_error()
            ))
        } else {
            Ok(Self(module))
        }
    }
    fn symbol(&self, name: &[u8]) -> Result<*mut std::ffi::c_void, String> {
        let pointer = unsafe { GetProcAddress(self.0, name.as_ptr()) };
        if pointer.is_null() {
            Err(format!(
                "missing C bridge export {}",
                String::from_utf8_lossy(name)
            ))
        } else {
            Ok(pointer)
        }
    }
}
#[cfg(all(windows, target_arch = "x86_64"))]
impl Drop for Module {
    fn drop(&mut self) {
        unsafe {
            FreeLibrary(self.0);
        }
    }
}
#[cfg(not(all(windows, target_arch = "x86_64")))]
struct Module;
#[cfg(not(all(windows, target_arch = "x86_64")))]
impl Module {
    fn load(_: &str) -> Result<Self, String> {
        Err("C bridges currently require Windows x86-64".into())
    }
    fn symbol(&self, _: &[u8]) -> Result<*mut std::ffi::c_void, String> {
        unreachable!()
    }
}
