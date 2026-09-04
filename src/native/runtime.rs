//! Assemble the linked platform runtime and executable entry shim.
use super::*;

pub(super) fn entry_source(
    result: NativeType,
    accepts_arguments: bool,
    runtime_strings: &[String],
    releases_result: bool,
) -> String {
    let print = match result {
        NativeType::Unit => String::new(),
        NativeType::Bool => "foster_rt_v1_write_bool(value); foster_rt_v1_write_newline();".into(),
        NativeType::Int => "foster_rt_v1_write_int(value); foster_rt_v1_write_newline();".into(),
        NativeType::Float => {
            "foster_rt_v1_write_float(value); foster_rt_v1_write_newline();".into()
        }
        NativeType::CodePoint => {
            "foster_rt_v1_write_code_point(value); foster_rt_v1_write_newline();".into()
        }
        NativeType::Byte => "foster_rt_v1_write_byte(value); foster_rt_v1_write_newline();".into(),
        NativeType::String => {
            "foster_rt_v1_write_string(value); foster_rt_v1_write_newline();".into()
        }
        NativeType::Arguments => {
            "foster_rt_v1_write_arguments(value); foster_rt_v1_write_newline();".into()
        }
        NativeType::StringList => {
            "foster_rt_v1_write_string_list(value); foster_rt_v1_write_newline();".into()
        }
        NativeType::Opaque | NativeType::Object(_) => {
            "foster_rt_v1_write_object(value); foster_rt_v1_write_newline();".into()
        }
    };
    let constants = runtime_strings
        .iter()
        .map(|value| format!("{value:?}.to_owned()"))
        .collect::<Vec<_>>()
        .join(", ");
    let result_type = match result {
        NativeType::Unit | NativeType::Bool | NativeType::Byte => "u8",
        NativeType::CodePoint => "u32",
        NativeType::Int => "i64",
        NativeType::String => "usize",
        NativeType::Float => "f64",
        NativeType::Opaque
        | NativeType::Arguments
        | NativeType::StringList
        | NativeType::Object(_) => "usize",
    };
    let declaration = if accepts_arguments {
        format!(
            "unsafe extern \"C\" {{ fn foster_native_entry(arguments: usize) -> {result_type}; }}"
        )
    } else {
        format!("unsafe extern \"C\" {{ fn foster_native_entry() -> {result_type}; }}")
    };
    let invocation = if accepts_arguments {
        "let mut supplied = std::env::args_os();\n    let executable = supplied.next().map(unicode_argument).unwrap_or_default();\n    let arguments = FosterArguments { executable, values: supplied.map(unicode_argument).collect() };\n    let value = unsafe { foster_native_entry(&arguments as *const FosterArguments as usize) };"
    } else {
        "let value = unsafe { foster_native_entry() };"
    };
    let release_declaration = if releases_result {
        "unsafe extern \"C\" { fn foster_native_release_result(value: usize) -> u8; }"
    } else {
        ""
    };
    let release = if releases_result {
        "unsafe { foster_native_release_result(value); }"
    } else {
        ""
    };
    let runtime_abi_version = abi::VERSION;
    let host_runtime = host_runtime::SOURCE;
    let equality_runtime = equality_runtime::SOURCE;
    let runtime_assertions = abi::runtime_assertions();
    format!(
        r#"use std::alloc::{{Layout, alloc_zeroed, dealloc, handle_alloc_error}};
use std::ffi::OsString;
use std::sync::OnceLock;

const FOSTER_RUNTIME_ABI_VERSION: u16 = {runtime_abi_version};

struct FosterArguments {{
    executable: String,
    values: Vec<String>,
}}

static FOSTER_STRINGS: OnceLock<Vec<String>> = OnceLock::new();

fn constants() -> &'static Vec<String> {{
    FOSTER_STRINGS.get_or_init(|| vec![{constants}])
}}

fn unicode_argument(value: OsString) -> String {{
    value.into_string().unwrap_or_else(|_| {{
        eprintln!("error: command arguments must be valid Unicode");
        std::process::exit(2);
    }})
}}

fn bounds_error(kind: &str, index: i64, length: usize) -> ! {{
    eprintln!("error: {{kind}} index {{index}} is outside 0..{{length}}");
    std::process::exit(2);
}}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_alloc(size: i64, align: i64) -> usize {{
    let layout = Layout::from_size_align(size as usize, align as usize)
        .unwrap_or_else(|_| std::process::abort());
    let pointer = unsafe {{ alloc_zeroed(layout) }};
    if pointer.is_null() {{
        handle_alloc_error(layout);
    }}
    pointer as usize
}}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_dealloc(pointer: usize, size: i64, align: i64) -> u8 {{
    let layout = Layout::from_size_align(size as usize, align as usize)
        .unwrap_or_else(|_| std::process::abort());
    unsafe {{ dealloc(pointer as *mut u8, layout) }};
    0
}}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_assert(condition: u8, message: usize) -> u8 {{
    if condition == 0 {{
        if message == 0 {{
            eprintln!("error: assertion failed");
        }} else {{
            eprintln!("error: assertion failed: {{}}", unsafe {{ string_value(message) }});
        }}
        std::process::exit(2);
    }}
    0
}}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_fail(kind: i64, detail: i64, limit: i64) -> u8 {{
    match kind {{
        1 => eprintln!("error: integer overflow"),
        2 => eprintln!("error: invalid integer division"),
        3 => eprintln!("error: invalid shift count {{detail}}; expected 0..={{limit}}"),
        4 => eprintln!("error: index {{detail}} is outside 0..{{limit}}"),
        5 => eprintln!("error: {{detail}} is not a valid Unicode scalar value"),
        6 => eprintln!("error: {{detail}} is not a valid Byte; expected 0..={{limit}}"),
        7 => eprintln!("error: value has no implementation for contract dispatch slot {{detail}}"),
        _ => eprintln!("error: native runtime failure {{kind}}"),
    }}
    std::process::exit(2);
}}

unsafe fn command_arguments<'a>(value: usize) -> &'a FosterArguments {{
    unsafe {{ &*(value as *const FosterArguments) }}
}}

unsafe fn string_list<'a>(value: usize) -> &'a Vec<String> {{
    unsafe {{ &*(value as *const Vec<String>) }}
}}

unsafe fn string_value<'a>(value: usize) -> &'a String {{
    unsafe {{ &*(value as *const String) }}
}}

#[derive(Clone, Copy)]
struct RuntimeValueLayout {{
    size: usize,
    semantic: u8,
}}

struct RuntimeField {{
    offset: usize,
    value: RuntimeValueLayout,
    name: String,
}}

unsafe fn descriptor_u8(descriptor: usize, offset: &mut usize) -> u8 {{
    let value = unsafe {{ *((descriptor + *offset) as *const u8) }};
    *offset += 1;
    value
}}

unsafe fn descriptor_u16(descriptor: usize, offset: &mut usize) -> u16 {{
    let pointer = (descriptor + *offset) as *const u16;
    let value = u16::from_le(unsafe {{ pointer.read_unaligned() }});
    *offset += 2;
    value
}}

unsafe fn descriptor_u32(descriptor: usize, offset: &mut usize) -> u32 {{
    let pointer = (descriptor + *offset) as *const u32;
    let value = u32::from_le(unsafe {{ pointer.read_unaligned() }});
    *offset += 4;
    value
}}

unsafe fn descriptor_text(descriptor: usize, offset: &mut usize) -> String {{
    let length = unsafe {{ descriptor_u32(descriptor, offset) }} as usize;
    let bytes = unsafe {{ std::slice::from_raw_parts((descriptor + *offset) as *const u8, length) }};
    *offset += length;
    String::from_utf8_lossy(bytes).into_owned()
}}

unsafe fn descriptor_value(descriptor: usize, offset: &mut usize) -> RuntimeValueLayout {{
    let size = unsafe {{ descriptor_u32(descriptor, offset) }} as usize;
    let _align = unsafe {{ descriptor_u16(descriptor, offset) }};
    let _representation = unsafe {{ descriptor_u8(descriptor, offset) }};
    let semantic = unsafe {{ descriptor_u8(descriptor, offset) }};
    let _pointee = unsafe {{ descriptor_u32(descriptor, offset) }};
    RuntimeValueLayout {{ size, semantic }}
}}

unsafe fn descriptor_field(descriptor: usize, offset: &mut usize) -> RuntimeField {{
    let _index = unsafe {{ descriptor_u32(descriptor, offset) }};
    let field_offset = unsafe {{ descriptor_u32(descriptor, offset) }} as usize;
    let value = unsafe {{ descriptor_value(descriptor, offset) }};
    let _ownership = unsafe {{ descriptor_u8(descriptor, offset) }};
    *offset += 3;
    let name = unsafe {{ descriptor_text(descriptor, offset) }};
    RuntimeField {{ offset: field_offset, value, name }}
}}

unsafe fn descriptor_fields(descriptor: usize, offset: &mut usize) -> Vec<RuntimeField> {{
    let count = unsafe {{ descriptor_u32(descriptor, offset) }} as usize;
    (0..count)
        .map(|_| unsafe {{ descriptor_field(descriptor, offset) }})
        .collect()
}}

unsafe fn runtime_word(address: usize) -> usize {{
    unsafe {{ (address as *const usize).read_unaligned() }}
}}

unsafe fn render_slot(address: usize, semantic: u8) {{
    match semantic {{
        0 => print!("()"),
        1 => print!("{{}}", unsafe {{ *(address as *const u8) }} != 0),
        2 => print!("{{}}", unsafe {{ (address as *const i64).read_unaligned() }}),
        3 => print!("{{}}", unsafe {{ (address as *const f64).read_unaligned() }}),
        4 => {{
            let value = unsafe {{ (address as *const u32).read_unaligned() }};
            print!("{{}}", char::from_u32(value).unwrap_or(char::REPLACEMENT_CHARACTER));
        }}
        5 => print!("{{}}", unsafe {{ *(address as *const u8) }}),
        6 => print!("{{}}", unsafe {{ string_value(runtime_word(address)) }}),
        7 => print!(":{{}}", unsafe {{ string_value(runtime_word(address)) }}),
        8 | 10 => unsafe {{ render_object(runtime_word(address)) }},
        9 => print!("<reference>"),
        _ => print!("<invalid value>"),
    }}
}}

unsafe fn render_object(object: usize) {{
    if object == 0 {{
        print!("<null>");
        return;
    }}
    let descriptor = unsafe {{ runtime_word(object) }};
    let magic = unsafe {{ std::slice::from_raw_parts(descriptor as *const u8, 4) }};
    let mut offset = 4;
    let version = unsafe {{ descriptor_u16(descriptor, &mut offset) }};
    let kind = unsafe {{ descriptor_u16(descriptor, &mut offset) }};
    if magic != b"FLYT" || version != 2 {{
        print!("<invalid object>");
        return;
    }}
    offset = 36;
    match kind {{
        0 => {{
            let name = unsafe {{ descriptor_text(descriptor, &mut offset) }};
            let fields = unsafe {{ descriptor_fields(descriptor, &mut offset) }};
            print!("{{name}} {{{{");
            for (index, field) in fields.iter().enumerate() {{
                if index > 0 {{ print!(", "); }}
                print!("{{}}: ", field.name);
                unsafe {{ render_slot(object + field.offset, field.value.semantic) }};
            }}
            print!("}}}}");
        }}
        1 => {{
            let name = unsafe {{ descriptor_text(descriptor, &mut offset) }};
            let tag_offset = unsafe {{ descriptor_u32(descriptor, &mut offset) }} as usize;
            let _payload_offset = unsafe {{ descriptor_u32(descriptor, &mut offset) }};
            let _payload_size = unsafe {{ descriptor_u32(descriptor, &mut offset) }};
            let _payload_align = unsafe {{ descriptor_u16(descriptor, &mut offset) }};
            let _reserved = unsafe {{ descriptor_u16(descriptor, &mut offset) }};
            let alternatives = unsafe {{ descriptor_u32(descriptor, &mut offset) }};
            let tag = unsafe {{ ((object + tag_offset) as *const u32).read_unaligned() }};
            for _ in 0..alternatives {{
                let alternative = unsafe {{ descriptor_text(descriptor, &mut offset) }};
                let candidate = unsafe {{ descriptor_u32(descriptor, &mut offset) }};
                let _payload_size = unsafe {{ descriptor_u32(descriptor, &mut offset) }};
                let _payload_align = unsafe {{ descriptor_u16(descriptor, &mut offset) }};
                let _reserved = unsafe {{ descriptor_u16(descriptor, &mut offset) }};
                let fields = unsafe {{ descriptor_fields(descriptor, &mut offset) }};
                if candidate == tag {{
                    print!("{{name}}.{{alternative}}");
                    if !fields.is_empty() {{
                        print!("(");
                        for (index, field) in fields.iter().enumerate() {{
                            if index > 0 {{ print!(", "); }}
                            unsafe {{ render_slot(object + field.offset, field.value.semantic) }};
                        }}
                        print!(")");
                    }}
                    return;
                }}
            }}
            print!("{{name}}.<invalid>");
        }}
        2 | 7 => print!("<closure>"),
        3 => print!("<reference>"),
        4 => {{
            let data_offset = unsafe {{ descriptor_u32(descriptor, &mut offset) }} as usize;
            let length_offset = unsafe {{ descriptor_u32(descriptor, &mut offset) }} as usize;
            let data = unsafe {{ runtime_word(object + data_offset) }};
            let length = unsafe {{ runtime_word(object + length_offset) }};
            print!("Bytes {{{{value: 0x");
            for byte in unsafe {{ std::slice::from_raw_parts(data as *const u8, length) }} {{
                print!("{{byte:02x}}");
            }}
            print!("}}}}");
        }}
        5 => {{
            let data_offset = unsafe {{ descriptor_u32(descriptor, &mut offset) }} as usize;
            let length_offset = unsafe {{ descriptor_u32(descriptor, &mut offset) }} as usize;
            let _capacity_offset = unsafe {{ descriptor_u32(descriptor, &mut offset) }};
            let element = unsafe {{ descriptor_value(descriptor, &mut offset) }};
            let mutable = unsafe {{ descriptor_u8(descriptor, &mut offset) }} != 0;
            let data = unsafe {{ runtime_word(object + data_offset) }};
            let length = unsafe {{ runtime_word(object + length_offset) }};
            if mutable {{
                print!("ByteBuffer(len={{length}})");
            }} else {{
                print!("[");
                for index in 0..length {{
                    if index > 0 {{ print!(", "); }}
                    unsafe {{ render_slot(data + index * element.size, element.semantic) }};
                }}
                print!("]");
            }}
        }}
        6 => print!("<handle>"),
        8 => {{
            let value_offset = unsafe {{ descriptor_u32(descriptor, &mut offset) }} as usize;
            let _release_offset = unsafe {{ descriptor_u32(descriptor, &mut offset) }};
            let semantic_offset = unsafe {{ descriptor_u32(descriptor, &mut offset) }} as usize;
            let semantic = unsafe {{ *((object + semantic_offset) as *const u8) }};
            unsafe {{ render_slot(object + value_offset, semantic) }};
        }}
        _ => print!("<object>"),
    }}
}}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_write_unit() -> u8 {{ print!("()"); 0 }}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_write_bool(value: u8) -> u8 {{ print!("{{}}", value != 0); 0 }}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_write_int(value: i64) -> u8 {{ print!("{{value}}"); 0 }}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_write_float(value: f64) -> u8 {{ print!("{{value}}"); 0 }}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_write_code_point(value: u32) -> u8 {{
    print!("{{}}", char::from_u32(value).unwrap_or(char::REPLACEMENT_CHARACTER));
    0
}}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_write_byte(value: u8) -> u8 {{ print!("{{value}}"); 0 }}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_write_string(value: usize) -> u8 {{
    print!("{{}}", unsafe {{ string_value(value) }});
    0
}}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_write_object(value: usize) -> u8 {{
    unsafe {{ render_object(value) }};
    0
}}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_write_string_list(value: usize) -> u8 {{
    let values = unsafe {{ string_list(value) }};
    print!("[");
    for (index, value) in values.iter().enumerate() {{
        if index > 0 {{ print!(", "); }}
        print!("{{value}}");
    }}
    print!("]");
    0
}}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_write_arguments(value: usize) -> u8 {{
    let arguments = unsafe {{ command_arguments(value) }};
    print!("Arguments {{{{executable: {{}}, values: ", arguments.executable);
    foster_rt_v1_write_string_list(&arguments.values as *const Vec<String> as usize);
    print!("}}}}");
    0
}}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_write_separator() -> u8 {{ print!(" "); 0 }}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_write_newline() -> u8 {{ println!(); 0 }}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_string_constant(index: i64) -> usize {{
    let index = usize::try_from(index).unwrap_or_else(|_| bounds_error("constant", index, constants().len()));
    constants().get(index).map(|value| value as *const String as usize)
        .unwrap_or_else(|| bounds_error("constant", index as i64, constants().len()))
}}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_args_executable(value: usize) -> usize {{
    unsafe {{ &command_arguments(value).executable as *const String as usize }}
}}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_args_values(value: usize) -> usize {{
    unsafe {{ &command_arguments(value).values as *const Vec<String> as usize }}
}}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_string_list_empty(value: usize) -> u8 {{
    u8::from(unsafe {{ string_list(value).is_empty() }})
}}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_string_list_length(value: usize) -> i64 {{
    unsafe {{ string_list(value).len() as i64 }}
}}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_string_list_get(value: usize, index: i64) -> usize {{
    let values = unsafe {{ string_list(value) }};
    let index = usize::try_from(index).unwrap_or_else(|_| bounds_error("argument", index, values.len()));
    values.get(index).map(|value| value as *const String as usize)
        .unwrap_or_else(|| bounds_error("argument", index as i64, values.len()))
}}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_string_list_head(value: usize) -> usize {{
    foster_rt_v1_string_list_get(value, 0)
}}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_string_empty(value: usize) -> u8 {{
    u8::from(unsafe {{ string_value(value).is_empty() }})
}}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_string_length(value: usize) -> i64 {{
    unsafe {{ string_value(value).chars().count() as i64 }}
}}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_string_head(value: usize) -> u32 {{
    unsafe {{ string_value(value).chars().next() }}
        .map(|value| value as u32)
        .unwrap_or_else(|| bounds_error("string", 0, 0))
}}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_string_rest(value: usize) -> usize {{
    let text = unsafe {{ string_value(value) }};
    let offset = text.chars().next().map_or(0, char::len_utf8);
    Box::into_raw(Box::new(text[offset..].to_owned())) as usize
}}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_string_whitespace(value: usize) -> u8 {{
    u8::from(unsafe {{ string_value(value).chars().all(char::is_whitespace) }})
}}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_string_concat(left: usize, right: usize) -> usize {{
    let mut result = unsafe {{ string_value(left).clone() }};
    result.push_str(unsafe {{ string_value(right) }});
    Box::into_raw(Box::new(result)) as usize
}}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_string_byte_length(value: usize) -> i64 {{
    unsafe {{ string_value(value).len() as i64 }}
}}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_string_copy_bytes(value: usize, destination: usize) -> u8 {{
    let bytes = unsafe {{ string_value(value).as_bytes() }};
    unsafe {{ std::ptr::copy_nonoverlapping(bytes.as_ptr(), destination as *mut u8, bytes.len()) }};
    0
}}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_string_from_utf8(data: usize, length: i64) -> usize {{
    let length = usize::try_from(length).unwrap_or_else(|_| std::process::abort());
    let bytes = unsafe {{ std::slice::from_raw_parts(data as *const u8, length) }};
    let text = std::str::from_utf8(bytes).unwrap_or_else(|_| {{
        eprintln!("error: Bytes are not valid UTF-8");
        std::process::exit(2);
    }});
    Box::into_raw(Box::new(text.to_owned())) as usize
}}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_copy_bytes(destination: usize, source: usize, length: i64) -> u8 {{
    let length = usize::try_from(length).unwrap_or_else(|_| std::process::abort());
    unsafe {{ std::ptr::copy_nonoverlapping(source as *const u8, destination as *mut u8, length) }};
    0
}}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_code_point_whitespace(value: u32) -> u8 {{
    u8::from(char::from_u32(value).is_some_and(char::is_whitespace))
}}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_code_point_string(value: u32) -> usize {{
    let value = char::from_u32(value).unwrap_or(char::REPLACEMENT_CHARACTER);
    Box::into_raw(Box::new(value.to_string())) as usize
}}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_string_get(value: usize, index: i64) -> u32 {{
    let text = unsafe {{ string_value(value) }};
    let index = usize::try_from(index).unwrap_or_else(|_| bounds_error("string", index, text.chars().count()));
    text.chars().nth(index).map(|value| value as u32)
        .unwrap_or_else(|| bounds_error("string", index as i64, text.chars().count()))
}}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_string_equal(left: usize, right: usize) -> u8 {{
    u8::from(unsafe {{ string_value(left) == string_value(right) }})
}}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_parse_float(value: usize) -> f64 {{
    unsafe {{ string_value(value) }}.parse::<f64>().unwrap_or_else(|_| {{
        eprintln!("error: invalid Float text");
        std::process::exit(2);
    }})
}}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_format_float(value: f64) -> usize {{
    Box::into_raw(Box::new(value.to_string())) as usize
}}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_ref_load_i8(reference: usize) -> u8 {{ unsafe {{ *(reference as *const u8) }} }}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_ref_load_i32(reference: usize) -> u32 {{ unsafe {{ *(reference as *const u32) }} }}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_ref_load_i64(reference: usize) -> i64 {{ unsafe {{ *(reference as *const i64) }} }}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_ref_load_f64(reference: usize) -> f64 {{ unsafe {{ *(reference as *const f64) }} }}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_ref_load_ptr(reference: usize) -> usize {{ unsafe {{ *(reference as *const usize) }} }}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_ref_store_i8(reference: usize, value: u8) -> u8 {{
    unsafe {{ *(reference as *mut u8) = value }};
    0
}}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_ref_store_i32(reference: usize, value: u32) -> u8 {{
    unsafe {{ *(reference as *mut u32) = value }};
    0
}}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_ref_store_i64(reference: usize, value: i64) -> u8 {{
    unsafe {{ *(reference as *mut i64) = value }};
    0
}}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_ref_store_f64(reference: usize, value: f64) -> u8 {{
    unsafe {{ *(reference as *mut f64) = value }};
    0
}}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v1_ref_store_ptr(reference: usize, value: usize) -> u8 {{
    unsafe {{ *(reference as *mut usize) = value }};
    0
}}

{host_runtime}
{equality_runtime}
{runtime_assertions}

{declaration}
{release_declaration}

fn main() {{
    foster_rt_v1_host_initialize();
    {invocation}
    {print}
    {release}
}}
"#
    )
}

pub(super) fn link_executable(
    artifact: ObjectArtifact,
    output: &Path,
    options: CompileOptions,
) -> Result<(), FosterError> {
    let output = absolute_path(output)?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            native_error(format!(
                "cannot create output directory `{}`: {error}",
                parent.display()
            ))
        })?;
    }
    let temporary = TemporaryDirectory::create()?;
    let object = temporary.path.join(if cfg!(windows) {
        "program.obj"
    } else {
        "program.o"
    });
    let shim = temporary.path.join("entry.rs");
    fs::write(&object, artifact.bytes)
        .map_err(|error| native_error(format!("cannot write `{}`: {error}", object.display())))?;
    fs::write(
        &shim,
        entry_source(
            artifact.result,
            artifact.accepts_arguments,
            &artifact.runtime_strings,
            artifact.releases_result,
        ),
    )
    .map_err(|error| native_error(format!("cannot write linker shim: {error}")))?;

    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let result = Command::new(&rustc)
        .arg("--edition=2024")
        .arg(&shim)
        .arg("-C")
        .arg(if options.optimize {
            "opt-level=2"
        } else {
            "opt-level=0"
        })
        .arg("-C")
        .arg(format!("link-arg={}", object.display()))
        .arg("-o")
        .arg(&output)
        .output()
        .map_err(|error| {
            native_error(format!(
                "cannot run `{}` to link the executable: {error}",
                Path::new(&rustc).display()
            ))
        })?;
    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        return Err(native_error(format!(
            "native linker failed with {}{}{}",
            result.status,
            if stderr.trim().is_empty() { "" } else { ": " },
            stderr.trim()
        )));
    }
    Ok(())
}
