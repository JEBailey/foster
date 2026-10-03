// Shared verbatim by the VM and native runtime; keep this module std-only.
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicI64, Ordering};

#[path = "callbacks.rs"]
pub mod callbacks;

pub const ABI: u64 = 1;
pub const LIMIT: usize = 16 * 1024 * 1024;
type Describe = unsafe extern "C" fn(u32) -> u64;
type Call =
    unsafe extern "C" fn(u32, *mut u8, *const u8, u64, *mut u8, u64, *mut u64, *mut *mut u8) -> i32;
type Destroy = unsafe extern "C" fn(u32, *mut u8);
type Close = unsafe extern "C" fn(u32, *mut u8, *mut u8) -> i64;
type Forget = unsafe extern "C" fn(u32, *mut u8);

struct Library {
    _module: Module,
    schema: String,
    describe: Describe,
    call: Call,
    destroy: Destroy,
    close: Close,
    forget: Option<Forget>,
}
struct Resource {
    library: Rc<Library>,
    kind: u32,
    pointer: *mut u8,
    leased: bool,
    _parents: Vec<Rc<RefCell<Resource>>>,
}

struct ResourceLease(Rc<RefCell<Resource>>);
impl ResourceLease {
    fn acquire(resource: Rc<RefCell<Resource>>) -> Result<Self, String> {
        {
            let mut value = resource
                .try_borrow_mut()
                .map_err(|_| "C resource is already in use")?;
            if value.leased || value.pointer.is_null() {
                return Err("C resource is closed or already in use".into());
            }
            value.leased = true;
        }
        Ok(Self(resource))
    }
}
impl Drop for ResourceLease {
    fn drop(&mut self) {
        self.0.borrow_mut().leased = false;
    }
}

struct ResourceFrame {
    library: Rc<Library>,
    pointers: HashMap<i64, (u32, *mut u8)>,
    leases: Vec<ResourceLease>,
    parents: HashMap<i64, Rc<RefCell<Resource>>>,
    transfers: HashSet<i64>,
}
thread_local! {
    static RESOURCE_FRAMES: RefCell<Vec<ResourceFrame>> = const { RefCell::new(Vec::new()) };
    static RESOURCE_ERROR: RefCell<Option<String>> = const { RefCell::new(None) };
}
struct ResourceFrameGuard;
impl ResourceFrameGuard {
    fn new(
        library: Rc<Library>,
        token: i64,
        resource: Option<Rc<RefCell<Resource>>>,
    ) -> Result<Self, String> {
        let mut frame = ResourceFrame {
            library,
            pointers: HashMap::new(),
            leases: Vec::new(),
            parents: HashMap::new(),
            transfers: HashSet::new(),
        };
        if let Some(resource) = resource {
            let lease = ResourceLease::acquire(resource)?;
            {
                let value = lease.0.borrow();
                frame.pointers.insert(token, (value.kind, value.pointer));
            }
            frame.leases.push(lease);
        }
        RESOURCE_FRAMES.with(|frames| frames.borrow_mut().push(frame));
        Ok(Self)
    }
    fn parents(&self) -> Vec<Rc<RefCell<Resource>>> {
        RESOURCE_FRAMES.with(|frames| {
            frames
                .borrow()
                .last()
                .unwrap()
                .parents
                .values()
                .cloned()
                .collect()
        })
    }
}
impl Drop for ResourceFrameGuard {
    fn drop(&mut self) {
        // Drop leases outside the frame stack borrow, allowing native destructors
        // to make independent bridge calls while releasing retained parents.
        let frame = RESOURCE_FRAMES.with(|frames| frames.borrow_mut().pop());
        drop(frame);
    }
}

unsafe extern "C" fn resolve_resource(
    schema: u64,
    token: i64,
    kind: u32,
    action: u32,
    output: *mut *mut u8,
) -> i32 {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if output.is_null() || action > 3 {
            return Err("invalid C resource resolver request");
        }
        RESOURCE_FRAMES.with(|frames| {
            let mut frames = frames
                .try_borrow_mut()
                .map_err(|_| "C resource resolver is already in use")?;
            let frame = frames
                .last_mut()
                .ok_or("C resource lookup requires an active call")?;
            if format!("{schema:016x}") != frame.library.schema {
                return Err("C resource binding schema mismatch");
            }
            let resource = STATE
                .with(|state| state.borrow().resources.get(&token).cloned())
                .ok_or("C resource is closed or belongs to another thread")?;
            {
                let value = resource
                    .try_borrow()
                    .map_err(|_| "C resource is already in use")?;
                if value.kind != kind
                    || !Rc::ptr_eq(&value.library, &frame.library)
                    || value.pointer.is_null()
                {
                    return Err("C resource has a different kind or bridge");
                }
            }
            if !frame.pointers.contains_key(&token) {
                let lease = ResourceLease::acquire(resource.clone())
                    .map_err(|_| "C resource is already in use")?;
                let value = resource.borrow();
                frame.pointers.insert(token, (value.kind, value.pointer));
                frame.leases.push(lease);
            }
            let pointer = frame.pointers[&token].1;
            if action == 1 {
                if frame.transfers.contains(&token) {
                    return Err("C resource cannot be both retained and transferred");
                }
                frame.parents.insert(token, resource.clone());
            } else if action == 3 {
                if Rc::strong_count(&resource) > 3
                    || frame.parents.contains_key(&token)
                    || !frame.transfers.insert(token)
                {
                    return Err("C resource is retained or transferred more than once");
                }
                if frame.library.forget.is_none() {
                    return Err("C bridge does not support ownership transfer");
                }
                // The consumed owner may itself retain borrowed native backing.
                // Keep its tombstone as a parent of the result so that backing
                // outlives the newly adopted native value as well.
                frame.parents.insert(token, resource.clone());
            } else if action == 2 {
                if !frame.transfers.remove(&token) {
                    return Err("C resource ownership transfer was not checked");
                }
                let forget = frame
                    .library
                    .forget
                    .ok_or("C bridge does not support ownership transfer")?;
                unsafe { forget(kind, pointer) };
                resource.borrow_mut().pointer = std::ptr::null_mut();
                // Keep a tombstone until the consumed Foster owner runs deinit.
                // Its ordinary release removes the entry without destroying the
                // native value now owned by the constructor's result.
            }
            unsafe { *output = pointer };
            Ok(())
        })
    }));
    match result {
        Ok(Ok(())) => 0,
        Ok(Err(message)) => {
            RESOURCE_ERROR.with(|error| *error.borrow_mut() = Some(message.to_owned()));
            2
        }
        Err(_) => 2,
    }
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
    paths: BridgePaths,
    resources: HashMap<i64, Rc<RefCell<Resource>>>,
}
#[derive(Default)]
struct BridgePaths {
    resolved: HashMap<String, String>,
}
impl BridgePaths {
    fn resolve(
        &mut self,
        name: &str,
        resolver: impl FnOnce(&str) -> Result<String, String>,
    ) -> Result<&str, String> {
        if !self.resolved.contains_key(name) {
            self.resolved.insert(name.to_owned(), resolver(name)?);
        }
        Ok(&self.resolved[name])
    }
}
thread_local! { static STATE: RefCell<State> = RefCell::new(State::default()); }
// Tokens are never reused, even between threads or successive VM executions.
static NEXT_TOKEN: AtomicI64 = AtomicI64::new(1);

pub fn int(bytes: &[u8]) -> Result<i64, String> {
    let bytes: [u8; 8] = bytes
        .try_into()
        .map_err(|_| "C scalar must contain exactly eight bytes")?;
    Ok(i64::from_le_bytes(bytes))
}
pub fn response(result: Result<Vec<u8>, String>) -> Vec<u8> {
    let (status, payload) = match result {
        Ok(bytes) => (0, bytes),
        Err(error) => (1, error.into_bytes()),
    };
    let mut packet = Vec::with_capacity(1 + payload.len());
    packet.push(status);
    packet.extend_from_slice(&payload);
    packet
}
pub fn remote_error() -> Vec<u8> {
    response(Err(
        "C bridge calls are not supported inside remote tasks".into()
    ))
}

/// `path` names an explicitly built bridge. Relative names resolve against
/// `FOSTER_BRIDGE_DIR`, the current directory, and the executable directory,
/// trying the full relative name before the bare file name; absolute names
/// pass through unchanged. Successful resolutions are pinned per thread;
/// failed resolutions are retried. Operation metadata is checked before passing a
/// resource pointer to C. No pointer comes from Foster.
pub fn exchange(
    path: &str,
    schema: &str,
    operation: i64,
    token: i64,
    create: bool,
    payload: &[u8],
) -> Vec<u8> {
    response((|| {
        if payload.len() > LIMIT {
            return Err("invalid C wire payload length".into());
        }
        let input = payload;
        let operation = u32::try_from(operation).map_err(|_| "C operation is outside u32")?;
        let (library, resource) = STATE.with(|state| {
            let mut state = state
                .try_borrow_mut()
                .map_err(|_| "C bridge registry is already in use")?;
            let library = if token == 0 {
                let State {
                    libraries, paths, ..
                } = &mut *state;
                let resolved = paths.resolve(path, resolve_bridge)?;
                if let Some(library) = libraries.get(resolved) {
                    library.clone()
                } else {
                    let library = Rc::new(Library::load(resolved)?);
                    libraries.insert(resolved.to_owned(), library.clone());
                    library
                }
            } else {
                state
                    .resources
                    .get(&token)
                    .ok_or("C resource is closed or belongs to another thread")?
                    .try_borrow()
                    .map_err(|_| "C resource is already in use")?
                    .library
                    .clone()
            };
            Ok::<_, String>((library, state.resources.get(&token).cloned()))
        })?;
        // Keep every native argument leased across callbacks without holding the
        // registry or a RefCell borrow across C. Nested calls get their own frame.
        let resource_frame = ResourceFrameGuard::new(library.clone(), token, resource.clone())?;
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
                let resource = resource.as_ref().ok_or("C resource is closed")?.borrow();
                if resource.kind != kind {
                    return Err("C operation requires a different resource type".into());
                }
                resource.pointer
            }
            _ => return Err("unknown C operation or incompatible resource receiver".into()),
        };
        // Bits 16..31 describe a fixed copied-record result; byte buffers use bit 8.
        let fixed_size = ((metadata >> 16) & 0xffff) as usize;
        let mut output = vec![
            0u8;
            if metadata & 256 != 0 {
                LIMIT
            } else {
                fixed_size.max(8)
            }
        ];
        let mut length = 0;
        let mut created = std::ptr::null_mut();
        RESOURCE_ERROR.with(|error| error.borrow_mut().take());
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
                leased: false,
                _parents: resource_frame.parents(),
            })
        } else {
            None
        };
        if status != 0 {
            if let Some(message) = RESOURCE_ERROR.with(|error| error.borrow_mut().take()) {
                return Err(message);
            }
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
            STATE.with(|state| {
                state
                    .borrow_mut()
                    .resources
                    .insert(token, Rc::new(RefCell::new(resource)))
            });
            Ok(token.to_le_bytes().to_vec())
        } else {
            output.truncate(length as usize);
            Ok(output)
        }
    })())
}

/// Returns [consumed byte, signed little-endian status]. A failed close may
/// retain ownership; the descriptor's close contract decides this, not its sign.
pub fn close(token: i64) -> Vec<u8> {
    response((|| {
        let resource = STATE
            .with(|state| state.borrow().resources.get(&token).cloned())
            .ok_or("C resource is closed or belongs to another thread")?;
        if Rc::strong_count(&resource) > 2 {
            return Err("C resource is in use or retained by another owner".into());
        }
        let mut resource = resource
            .try_borrow_mut()
            .map_err(|_| "C resource is already in use")?;
        if resource.leased {
            return Err("C resource is already in use".into());
        }
        if resource.pointer.is_null() {
            return Err("C resource ownership has been transferred".into());
        }
        let mut consumed = 0;
        let status =
            unsafe { (resource.library.close)(resource.kind, resource.pointer, &mut consumed) };
        if consumed != 0 {
            resource.pointer = std::ptr::null_mut();
            STATE.with(|state| state.borrow_mut().resources.remove(&token));
        }
        let mut result = vec![u8::from(consumed != 0)];
        result.extend_from_slice(&status.to_le_bytes());
        Ok(result)
    })())
}

/// Resolves a bridge name to an existing file. `bridge_dir` is
/// `FOSTER_BRIDGE_DIR`; all anchors are optional so tests stay hermetic.
fn resolve_bridge_with(
    name: &str,
    bridge_dir: Option<&Path>,
    cwd: Option<&Path>,
    exe_dir: Option<&Path>,
) -> Result<String, String> {
    use std::ffi::OsStr;
    if Path::new(name).is_absolute() {
        return Ok(name.to_owned());
    }
    let file_name = Path::new(name).file_name().map(|name| name.to_os_string());
    let anchors: Vec<&Path> = [bridge_dir, cwd, exe_dir]
        .into_iter()
        .flatten()
        .filter(|dir| !dir.as_os_str().is_empty())
        .collect();
    let mut tried: Vec<PathBuf> = Vec::new();
    let push = |base: &Path, suffix: &OsStr, tried: &mut Vec<PathBuf>| {
        let candidate = base.join(suffix);
        if !tried.contains(&candidate) {
            tried.push(candidate);
        }
    };
    // The full relative name wins over the bare file name at every anchor.
    let mut suffixes = vec![name.as_ref()];
    if let Some(file) = &file_name {
        suffixes.push(file.as_os_str());
    }
    for suffix in suffixes {
        for dir in &anchors {
            push(dir, suffix, &mut tried);
        }
    }
    for candidate in &tried {
        if candidate.is_file() {
            // The Windows dependency search requires a fully qualified path,
            // including when FOSTER_BRIDGE_DIR itself is relative.
            let resolved = std::path::absolute(candidate)
                .map_err(|error| format!("cannot make C bridge path absolute: {error}"))?;
            let resolved: PathBuf = resolved.components().collect();
            return Ok(resolved.to_string_lossy().into_owned());
        }
    }
    Err(format!(
        "cannot resolve C bridge '{}'; tried {}; set FOSTER_BRIDGE_DIR or use an absolute path",
        name,
        tried
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

fn resolve_bridge(name: &str) -> Result<String, String> {
    let bridge_dir = std::env::var("FOSTER_BRIDGE_DIR")
        .ok()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let cwd = std::env::current_dir().ok();
    let exe = std::env::current_exe().ok();
    resolve_bridge_with(
        name,
        bridge_dir.as_deref(),
        cwd.as_deref(),
        exe.as_ref().and_then(|exe| exe.parent()),
    )
}

pub fn release(token: i64) -> Result<(), String> {
    if token == 0 {
        return Ok(());
    }
    let resource = STATE
        .with(|state| state.borrow().resources.get(&token).cloned())
        .ok_or("C resource is closed or belongs to another thread")?;
    if resource
        .try_borrow_mut()
        .map_err(|_| "C resource is already in use")?
        .leased
    {
        return Err("C resource is already in use".into());
    }
    STATE.with(|state| state.borrow_mut().resources.remove(&token));
    drop(resource);
    Ok(())
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
            if let Ok(initialize) = module.symbol(b"foster_c_callbacks_init\0") {
                type Invoke =
                    unsafe extern "C" fn(i64, u64, *const u8, u64, *mut u8, u64, *mut u64) -> i32;
                let initialize: unsafe extern "C" fn(u64, Invoke) -> i32 =
                    std::mem::transmute(initialize);
                if initialize(1, callbacks::invoke) != 0 {
                    return Err("C callback ABI mismatch".into());
                }
            }
            if let Ok(initialize) = module.symbol(b"foster_c_resources_init\0") {
                type Resolve = unsafe extern "C" fn(u64, i64, u32, u32, *mut *mut u8) -> i32;
                let initialize: unsafe extern "C" fn(u64, Resolve) -> i32 =
                    std::mem::transmute(initialize);
                if initialize(1, resolve_resource) != 0 {
                    return Err("C resource argument ABI mismatch".into());
                }
            }
            let schema: unsafe extern "C" fn() -> u64 =
                std::mem::transmute(module.symbol(b"foster_c_schema\0")?);
            Ok(Self {
                schema: format!("{:016x}", schema()),
                describe: std::mem::transmute(module.symbol(b"foster_c_describe\0")?),
                call: std::mem::transmute(module.symbol(b"foster_c_call\0")?),
                destroy: std::mem::transmute(module.symbol(b"foster_c_destroy\0")?),
                close: std::mem::transmute(module.symbol(b"foster_c_close\0")?),
                forget: module
                    .symbol(b"foster_c_forget\0")
                    .ok()
                    .map(|symbol| std::mem::transmute(symbol)),
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
        // LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR needs native separators when deriving
        // the directory for dependencies. Generated Foster paths use forward slashes.
        let native_path = path.replace('/', "\\");
        let wide: Vec<_> = native_path.encode_utf16().chain(Some(0)).collect();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_paths_pin_successes_and_retry_failures() {
        let mut paths = BridgePaths::default();
        assert!(
            paths
                .resolve("bridge.dll", |_| Err("missing".into()))
                .is_err()
        );
        assert_eq!(
            paths
                .resolve("bridge.dll", |_| Ok("first/bridge.dll".into()))
                .unwrap(),
            "first/bridge.dll"
        );
        assert_eq!(
            paths
                .resolve("bridge.dll", |_| panic!(
                    "cached names must not search again"
                ))
                .unwrap(),
            "first/bridge.dll"
        );
        assert_eq!(
            paths
                .resolve("other.dll", |_| Ok("second/other.dll".into()))
                .unwrap(),
            "second/other.dll"
        );
        let mut other_registry = BridgePaths::default();
        assert_eq!(
            other_registry
                .resolve("bridge.dll", |_| Ok("second/bridge.dll".into()))
                .unwrap(),
            "second/bridge.dll"
        );
    }

    #[test]
    fn relative_bridge_directory_resolves_to_absolute_path() {
        let cwd = std::env::current_dir().unwrap();
        let name = "Cargo.toml";
        let resolved = resolve_bridge_with(name, Some(Path::new(".")), None, None).unwrap();
        assert!(Path::new(&resolved).is_absolute());
        assert_eq!(Path::new(&resolved), cwd.join(name));
    }

    #[test]
    fn scalar_decoder_preserves_exact_bits_and_rejects_wrong_lengths() {
        for value in [i64::MIN, -1, 0, 1, i64::MAX] {
            assert_eq!(int(&value.to_le_bytes()).unwrap(), value);
        }
        for bytes in [vec![], vec![0], vec![0; 7], vec![0; 9]] {
            assert_eq!(
                int(&bytes).unwrap_err(),
                "C scalar must contain exactly eight bytes"
            );
        }
        let oversized = vec![0; LIMIT + 1];
        assert_eq!(
            exchange("unused", "", 0, 0, false, &oversized),
            response(Err("invalid C wire payload length".into()))
        );
    }

    #[test]
    fn bridge_resolution_uses_documented_anchor_order() {
        let root =
            std::env::temp_dir().join(format!("foster-bridge-resolve-{}", std::process::id()));
        let bridge = root.join("bridges").join("sub").join("bridge.dll");
        let cwd = root.join("cwd");
        let exe_dir = root.join("exe");
        for directory in [bridge.parent().unwrap(), &cwd, &exe_dir] {
            std::fs::create_dir_all(directory).unwrap();
        }
        std::fs::write(&bridge, b"bridge").unwrap();
        let text = |path: &std::path::Path| path.to_string_lossy().into_owned();

        // Absolute names bypass the search entirely.
        let absolute = text(&root.join("never").join("built.dll"));
        assert_eq!(
            resolve_bridge_with(&absolute, None, Some(&cwd), Some(&exe_dir)).unwrap(),
            absolute
        );
        // The bridge directory wins over both the current and executable directories.
        assert_eq!(
            resolve_bridge_with(
                "sub/bridge.dll",
                Some(root.join("bridges").as_path()),
                Some(&cwd),
                Some(&exe_dir)
            )
            .unwrap(),
            text(&bridge)
        );
        // Full relative name: current directory before the executable directory.
        let cwd_hit = cwd.join("full").join("bridge.dll");
        std::fs::create_dir_all(cwd.join("full")).unwrap();
        std::fs::write(&cwd_hit, b"cwd").unwrap();
        assert_eq!(
            resolve_bridge_with("full/bridge.dll", None, Some(&cwd), Some(&exe_dir)).unwrap(),
            text(&cwd_hit)
        );
        // The bare file name is the native-exe case: the module carries a
        // repository-relative name, but the DLL sits beside the executable.
        let exe_hit = exe_dir.join("bridge.dll");
        std::fs::write(&exe_hit, b"exe").unwrap();
        assert_eq!(
            resolve_bridge_with(
                "full/bridge.dll",
                None,
                Some(cwd.join("empty").as_path()),
                Some(&exe_dir)
            )
            .unwrap(),
            text(&exe_hit)
        );
        // A missing bridge reports every candidate it tried.
        let error = resolve_bridge_with(
            "nope/bridge.dll",
            None,
            Some(cwd.join("empty").as_path()),
            Some(exe_dir.join("empty").as_path()),
        )
        .unwrap_err();
        assert!(error.starts_with("cannot resolve C bridge 'nope/bridge.dll'; tried "));
        assert!(error.contains("set FOSTER_BRIDGE_DIR or use an absolute path"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn response_envelopes_preserve_all_bytes_and_utf8_errors() {
        let bytes = (0..=255).collect::<Vec<u8>>();
        let packet = response(Ok(bytes.clone()));
        assert_eq!(packet[0], 0);
        assert_eq!(&packet[1..], bytes);
        assert_eq!(response(Ok(vec![])), vec![0]);
        let packet = response(Err("failure: λ".into()));
        assert_eq!(packet[0], 1);
        assert_eq!(&packet[1..], "failure: λ".as_bytes());
    }
}
