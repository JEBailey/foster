//! Stable native runtime, compiled and linted as an ordinary Rust crate.
use std::alloc::{Layout, alloc_zeroed, dealloc, handle_alloc_error};
use std::ffi::OsString;
use std::sync::OnceLock;

include!("version.rs");
pub const FOSTER_RUNTIME_ABI_VERSION: u16 = ABI_VERSION;

#[allow(dead_code)]
#[path = "../../src/remote.rs"]
mod remote_lifecycle;
pub use foster_host as services;

static FOSTER_CONSTANTS: OnceLock<&'static [&'static str]> = OnceLock::new();

#[inline(never)]
pub fn foster_runtime_initialize(constants: &'static [&'static str]) {
    FOSTER_CONSTANTS
        .set(constants)
        .expect("runtime already initialized");
    foster_rt_v4_host_initialize();
}

pub fn foster_runtime_check_execution() {
    if let Some(message) = FOSTER_EXECUTION.with(|execution| execution.borrow_mut().take()) {
        eprintln!("error: {message}");
        std::process::exit(2);
    }
}

fn constants() -> &'static [&'static str] {
    FOSTER_CONSTANTS.get().expect("runtime not initialized")
}

pub fn unicode_argument(value: OsString) -> String {
    value.into_string().unwrap_or_else(|_| {
        eprintln!("error: command arguments must be valid Unicode");
        std::process::exit(2);
    })
}

fn bounds_error(kind: &str, index: i64, length: usize) -> ! {
    eprintln!("error: {kind} index {index} is outside 0..{length}");
    std::process::exit(2);
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v4_alloc(size: i64, align: i64) -> usize {
    let layout = Layout::from_size_align(size as usize, align as usize)
        .unwrap_or_else(|_| std::process::abort());
    let pointer = unsafe { alloc_zeroed(layout) };
    if pointer.is_null() {
        handle_alloc_error(layout);
    }
    pointer as usize
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v4_dealloc(pointer: usize, size: i64, align: i64) -> u8 {
    let layout = Layout::from_size_align(size as usize, align as usize)
        .unwrap_or_else(|_| std::process::abort());
    unsafe { dealloc(pointer as *mut u8, layout) };
    0
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v4_assert(condition: u8, message: usize) -> u8 {
    if condition == 0 {
        let message = if message == 0 {
            "assertion failed".to_owned()
        } else {
            format!("assertion failed: {}", unsafe { string_value(message) })
        };
        foster_execution_failure(message);
    }
    0
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v4_fail(kind: i64, detail: i64, limit: i64) -> u8 {
    foster_execution_failure(match kind {
        1 => "integer overflow".into(),
        2 => "invalid integer division".into(),
        3 => format!("invalid shift count {detail}; expected 0..={limit}"),
        4 => format!("index {detail} is outside 0..{limit}"),
        5 => format!("{detail} is not a valid Unicode scalar value"),
        6 => format!("{detail} is not a valid Byte; expected 0..={limit}"),
        7 => format!("value has no implementation for contract dispatch slot {detail}"),
        _ => format!("native runtime failure {kind}"),
    });
    0
}

unsafe extern "C" {
    fn foster_native_string(data: usize, length: i64) -> usize;
    fn foster_native_string_data(value: usize) -> usize;
    fn foster_native_string_length(value: usize) -> i64;
}

pub fn owned_string(text: &str) -> usize {
    unsafe { foster_native_string(text.as_ptr() as usize, text.len() as i64) }
}

unsafe fn string_value<'a>(value: usize) -> &'a str {
    let data = unsafe { foster_native_string_data(value) };
    let length = unsafe { foster_native_string_length(value) } as usize;
    unsafe { std::str::from_utf8_unchecked(std::slice::from_raw_parts(data as *const u8, length)) }
}

#[derive(Clone, Copy)]
struct RuntimeValueLayout {
    size: usize,
    semantic: u8,
}

struct RuntimeField {
    offset: usize,
    value: RuntimeValueLayout,
    name: String,
}

unsafe fn descriptor_u8(descriptor: usize, offset: &mut usize) -> u8 {
    let value = unsafe { *((descriptor + *offset) as *const u8) };
    *offset += 1;
    value
}

unsafe fn descriptor_u16(descriptor: usize, offset: &mut usize) -> u16 {
    let pointer = (descriptor + *offset) as *const u16;
    let value = u16::from_le(unsafe { pointer.read_unaligned() });
    *offset += 2;
    value
}

unsafe fn descriptor_u32(descriptor: usize, offset: &mut usize) -> u32 {
    let pointer = (descriptor + *offset) as *const u32;
    let value = u32::from_le(unsafe { pointer.read_unaligned() });
    *offset += 4;
    value
}

unsafe fn descriptor_text(descriptor: usize, offset: &mut usize) -> String {
    let length = unsafe { descriptor_u32(descriptor, offset) } as usize;
    let bytes = unsafe { std::slice::from_raw_parts((descriptor + *offset) as *const u8, length) };
    *offset += length;
    String::from_utf8_lossy(bytes).into_owned()
}

unsafe fn descriptor_value(descriptor: usize, offset: &mut usize) -> RuntimeValueLayout {
    let size = unsafe { descriptor_u32(descriptor, offset) } as usize;
    let _align = unsafe { descriptor_u16(descriptor, offset) };
    let _representation = unsafe { descriptor_u8(descriptor, offset) };
    let semantic = unsafe { descriptor_u8(descriptor, offset) };
    let _pointee = unsafe { descriptor_u32(descriptor, offset) };
    RuntimeValueLayout { size, semantic }
}

unsafe fn descriptor_field(descriptor: usize, offset: &mut usize) -> RuntimeField {
    let _index = unsafe { descriptor_u32(descriptor, offset) };
    let field_offset = unsafe { descriptor_u32(descriptor, offset) } as usize;
    let value = unsafe { descriptor_value(descriptor, offset) };
    let _ownership = unsafe { descriptor_u8(descriptor, offset) };
    *offset += 3;
    let name = unsafe { descriptor_text(descriptor, offset) };
    RuntimeField {
        offset: field_offset,
        value,
        name,
    }
}

unsafe fn descriptor_fields(descriptor: usize, offset: &mut usize) -> Vec<RuntimeField> {
    let count = unsafe { descriptor_u32(descriptor, offset) } as usize;
    (0..count)
        .map(|_| unsafe { descriptor_field(descriptor, offset) })
        .collect()
}

unsafe fn runtime_word(address: usize) -> usize {
    unsafe { (address as *const usize).read_unaligned() }
}

unsafe fn render_slot(address: usize, semantic: u8) {
    match semantic {
        0 => print!("()"),
        1 => print!("{}", unsafe { *(address as *const u8) } != 0),
        2 => print!("{}", unsafe { (address as *const i64).read_unaligned() }),
        3 => print!("{}", unsafe { (address as *const f64).read_unaligned() }),
        4 => {
            let value = unsafe { (address as *const u32).read_unaligned() };
            print!(
                "{}",
                char::from_u32(value).unwrap_or(char::REPLACEMENT_CHARACTER)
            );
        }
        5 => print!("{}", unsafe { *(address as *const u8) }),
        6 => print!("{}", unsafe { string_value(runtime_word(address)) }),
        7 => print!(":{}", unsafe { string_value(runtime_word(address)) }),
        8 | 10 => unsafe { render_object(runtime_word(address)) },
        9 => print!("<reference>"),
        _ => print!("<invalid value>"),
    }
}

unsafe fn render_object(object: usize) {
    if object == 0 {
        print!("<null>");
        return;
    }
    let descriptor = unsafe { runtime_word(object) };
    let magic = unsafe { std::slice::from_raw_parts(descriptor as *const u8, 4) };
    let mut offset = 4;
    let version = unsafe { descriptor_u16(descriptor, &mut offset) };
    let kind = unsafe { descriptor_u16(descriptor, &mut offset) };
    if magic != b"FLYT" || version != 2 {
        print!("<invalid object>");
        return;
    }
    offset = 36;
    match kind {
        0 => {
            let name = unsafe { descriptor_text(descriptor, &mut offset) };
            let fields = unsafe { descriptor_fields(descriptor, &mut offset) };
            print!("{name} {{");
            for (index, field) in fields.iter().enumerate() {
                if index > 0 {
                    print!(", ");
                }
                print!("{}: ", field.name);
                unsafe { render_slot(object + field.offset, field.value.semantic) };
            }
            print!("}}");
        }
        1 => {
            let name = unsafe { descriptor_text(descriptor, &mut offset) };
            let tag_offset = unsafe { descriptor_u32(descriptor, &mut offset) } as usize;
            let _payload_offset = unsafe { descriptor_u32(descriptor, &mut offset) };
            let _payload_size = unsafe { descriptor_u32(descriptor, &mut offset) };
            let _payload_align = unsafe { descriptor_u16(descriptor, &mut offset) };
            let _reserved = unsafe { descriptor_u16(descriptor, &mut offset) };
            let alternatives = unsafe { descriptor_u32(descriptor, &mut offset) };
            let tag = unsafe { ((object + tag_offset) as *const u32).read_unaligned() };
            for _ in 0..alternatives {
                let alternative = unsafe { descriptor_text(descriptor, &mut offset) };
                let candidate = unsafe { descriptor_u32(descriptor, &mut offset) };
                let _payload_size = unsafe { descriptor_u32(descriptor, &mut offset) };
                let _payload_align = unsafe { descriptor_u16(descriptor, &mut offset) };
                let _reserved = unsafe { descriptor_u16(descriptor, &mut offset) };
                let fields = unsafe { descriptor_fields(descriptor, &mut offset) };
                if candidate == tag {
                    print!("{name}.{alternative}");
                    if !fields.is_empty() {
                        print!("(");
                        for (index, field) in fields.iter().enumerate() {
                            if index > 0 {
                                print!(", ");
                            }
                            unsafe { render_slot(object + field.offset, field.value.semantic) };
                        }
                        print!(")");
                    }
                    return;
                }
            }
            print!("{name}.<invalid>");
        }
        2 | 7 => print!("<closure>"),
        3 => print!("<reference>"),
        4 => {
            let data_offset = unsafe { descriptor_u32(descriptor, &mut offset) } as usize;
            let length_offset = unsafe { descriptor_u32(descriptor, &mut offset) } as usize;
            let data = unsafe { runtime_word(object + data_offset) };
            let length = unsafe { runtime_word(object + length_offset) };
            print!("Bytes {{value: 0x");
            for byte in unsafe { std::slice::from_raw_parts(data as *const u8, length) } {
                print!("{byte:02x}");
            }
            print!("}}");
        }
        5 => {
            let data_offset = unsafe { descriptor_u32(descriptor, &mut offset) } as usize;
            let length_offset = unsafe { descriptor_u32(descriptor, &mut offset) } as usize;
            let _capacity_offset = unsafe { descriptor_u32(descriptor, &mut offset) };
            let element = unsafe { descriptor_value(descriptor, &mut offset) };
            let mutable = unsafe { descriptor_u8(descriptor, &mut offset) } != 0;
            let data = unsafe { runtime_word(object + data_offset) };
            let length = unsafe { runtime_word(object + length_offset) };
            if mutable {
                print!("ByteBuffer(len={length})");
            } else {
                print!("[");
                for index in 0..length {
                    if index > 0 {
                        print!(", ");
                    }
                    unsafe { render_slot(data + index * element.size, element.semantic) };
                }
                print!("]");
            }
        }
        6 => print!("<handle>"),
        8 => {
            let value_offset = unsafe { descriptor_u32(descriptor, &mut offset) } as usize;
            let _release_offset = unsafe { descriptor_u32(descriptor, &mut offset) };
            let semantic_offset = unsafe { descriptor_u32(descriptor, &mut offset) } as usize;
            let semantic = unsafe { *((object + semantic_offset) as *const u8) };
            unsafe { render_slot(object + value_offset, semantic) };
        }
        _ => print!("<object>"),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn foster_rt_v4_write_unit() -> u8 {
    print!("()");
    0
}
#[unsafe(no_mangle)]
pub extern "C" fn foster_rt_v4_write_bool(value: u8) -> u8 {
    print!("{}", value != 0);
    0
}
#[unsafe(no_mangle)]
pub extern "C" fn foster_rt_v4_write_int(value: i64) -> u8 {
    print!("{value}");
    0
}
#[unsafe(no_mangle)]
pub extern "C" fn foster_rt_v4_write_float(value: f64) -> u8 {
    print!("{value}");
    0
}
#[unsafe(no_mangle)]
pub extern "C" fn foster_rt_v4_write_code_point(value: u32) -> u8 {
    print!(
        "{}",
        char::from_u32(value).unwrap_or(char::REPLACEMENT_CHARACTER)
    );
    0
}
#[unsafe(no_mangle)]
pub extern "C" fn foster_rt_v4_write_byte(value: u8) -> u8 {
    print!("{value}");
    0
}
#[unsafe(no_mangle)]
pub extern "C" fn foster_rt_v4_write_string(value: usize) -> u8 {
    print!("{}", unsafe { string_value(value) });
    0
}
#[unsafe(no_mangle)]
pub extern "C" fn foster_rt_v4_write_object(value: usize) -> u8 {
    unsafe { render_object(value) };
    0
}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v4_write_separator() -> u8 {
    print!(" ");
    0
}
#[unsafe(no_mangle)]
pub extern "C" fn foster_rt_v4_write_newline() -> u8 {
    println!();
    0
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v4_string_constant(index: i64) -> usize {
    let index = usize::try_from(index)
        .unwrap_or_else(|_| bounds_error("constant", index, constants().len()));
    constants()
        .get(index)
        .map(|value| owned_string(value))
        .unwrap_or_else(|| bounds_error("constant", index as i64, constants().len()))
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v4_string_empty(value: usize) -> u8 {
    u8::from(unsafe { string_value(value).is_empty() })
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v4_string_whitespace(value: usize) -> u8 {
    u8::from(unsafe { string_value(value).chars().all(char::is_whitespace) })
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v4_string_concat(left: usize, right: usize) -> usize {
    let mut result = unsafe { string_value(left).to_owned() };
    result.push_str(unsafe { string_value(right) });
    owned_string(&result)
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v4_copy_bytes(destination: usize, source: usize, length: i64) -> u8 {
    let length = usize::try_from(length).unwrap_or_else(|_| std::process::abort());
    unsafe { std::ptr::copy_nonoverlapping(source as *const u8, destination as *mut u8, length) };
    0
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v4_code_point_whitespace(value: u32) -> u8 {
    u8::from(char::from_u32(value).is_some_and(char::is_whitespace))
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v4_code_point_string(value: u32) -> usize {
    let value = char::from_u32(value).unwrap_or(char::REPLACEMENT_CHARACTER);
    owned_string(&value.to_string())
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v4_string_get(value: usize, index: i64) -> u32 {
    let text = unsafe { string_value(value) };
    usize::try_from(index)
        .ok()
        .and_then(|index| text.chars().nth(index))
        .map(|value| value as u32)
        .unwrap_or_else(|| {
            foster_execution_failure(format!(
                "string index {index} is outside 0..{}",
                text.chars().count()
            ));
            0
        })
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v4_string_equal(left: usize, right: usize) -> u8 {
    u8::from(unsafe { string_value(left) == string_value(right) })
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v4_parse_float(value: usize) -> f64 {
    unsafe { string_value(value) }
        .parse::<f64>()
        .unwrap_or_else(|_| {
            foster_execution_failure("invalid Float text".into());
            0.0
        })
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v4_format_float(value: f64) -> usize {
    owned_string(&value.to_string())
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v4_ref_load_i8(reference: usize) -> u8 {
    unsafe { *(reference as *const u8) }
}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v4_ref_load_i32(reference: usize) -> u32 {
    unsafe { *(reference as *const u32) }
}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v4_ref_load_i64(reference: usize) -> i64 {
    unsafe { *(reference as *const i64) }
}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v4_ref_load_f64(reference: usize) -> f64 {
    unsafe { *(reference as *const f64) }
}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v4_ref_load_ptr(reference: usize) -> usize {
    unsafe { *(reference as *const usize) }
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v4_ref_store_i8(reference: usize, value: u8) -> u8 {
    unsafe { *(reference as *mut u8) = value };
    0
}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v4_ref_store_i32(reference: usize, value: u32) -> u8 {
    unsafe { *(reference as *mut u32) = value };
    0
}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v4_ref_store_i64(reference: usize, value: i64) -> u8 {
    unsafe { *(reference as *mut i64) = value };
    0
}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v4_ref_store_f64(reference: usize, value: f64) -> u8 {
    unsafe { *(reference as *mut f64) = value };
    0
}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v4_ref_store_ptr(reference: usize, value: usize) -> u8 {
    unsafe { *(reference as *mut usize) = value };
    0
}

include!("host.rs");
include!("equality.rs");

#[cfg(test)]
mod tests;
