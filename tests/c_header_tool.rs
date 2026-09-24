//! End-to-end coverage of Clang discovery, Foster mapping, and generated bindings.
#[cfg(all(windows, target_arch = "x86_64"))]
#[test]
fn foster_header_tool_builds_scalars_and_reports_unsupported_declarations() {
    use std::{fs, process::Command};
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let directory = std::env::temp_dir().join(format!("foster header tool {}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let header = directory.join("sample header.h");
    let source = directory.join("sample source.c");
    let output = directory.join("sample.dll");
    fs::write(
        directory.join("dependency.h"),
        "int dependency_only(void);\n",
    )
    .unwrap();
    fs::write(
        &header,
        r#"
#include <stdint.h>
#include "dependency.h"
typedef int32_t Count;
#define DECLARE(name) Count name(Count a, Count b)
DECLARE(header_add);
int header_zero(void);
void *header_open(void);
int header_log(const char *, ...);
long header_long(long);
typedef struct Vec { float x, y; } Vec;
typedef struct Color { uint8_t r, g, b, a; } Color;
typedef struct { float width, height; } Rectangle;
typedef struct Packet {
    Vec point;
    Color tint;
    Rectangle bounds;
    _Bool enabled;
    int16_t delta;
    uint64_t bits;
    int type, c_type, copy;
} Packet;
#pragma pack(push, 1)
typedef struct Packed { uint8_t tag; double amount; } Packed;
#pragma pack(pop)
Packet header_roundtrip(Packet value);
Packed header_packed(Packed value);
int header_calls(void);
typedef struct Pointer { int *data; } Pointer;
typedef struct Array { int data[4]; } Array;
typedef struct Bits { unsigned int value : 3; } Bits;
typedef union Union { int i; float f; } Union;
Pointer bad_pointer(Pointer value);
Array bad_array(Array value);
Bits bad_bits(Bits value);
Union bad_union(Union value);
"#,
    )
    .unwrap();
    fs::write(&source, r#"
#include "sample header.h"
static int calls;
Count header_add(Count a, Count b) { return a+b; }
int header_zero(void) { return 0; }
Packet header_roundtrip(Packet value) { calls++; value.point.x += 2; value.tint.g = 42; value.bounds.width += 1.5f; return value; }
Packed header_packed(Packed value) { value.amount *= 2; return value; }
int header_calls(void) { return calls; }
"#).unwrap();
    let invoke = |extra: &[&str]| {
        Command::new("powershell")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(root.join("tools/cbind/cbind.ps1"))
            .arg("-Header")
            .arg(&header)
            .arg("-Output")
            .arg(&output)
            .arg("-Sources")
            .arg(&source)
            .arg("-Foster")
            .arg(env!("CARGO_BIN_EXE_foster"))
            .args(extra)
            .output()
            .unwrap()
    };
    let rejected = invoke(&[]);
    assert!(
        !rejected.status.success(),
        "unsupported declarations must fail by default"
    );
    let report = fs::read_to_string(output.with_extension("unsupported.txt")).unwrap();
    for symbol in [
        "header_open",
        "header_log",
        "header_long",
        "bad_pointer",
        "bad_array",
        "bad_bits",
        "bad_union",
    ] {
        assert!(report.contains(symbol), "{report}");
    }
    assert!(
        !report.contains("dependency_only"),
        "included headers must not be imported"
    );
    let built = invoke(&["-SkipUnsupported"]);
    assert!(
        built.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr)
    );
    let manifest_text = fs::read_to_string(output.with_extension("bindings.json")).unwrap();
    let manifest = foster::foreign::Manifest::parse(&manifest_text).unwrap();
    let document: serde_json::Value = serde_json::from_str(&manifest_text).unwrap();
    assert_eq!(document["operations"].as_array().unwrap().len(), 5);
    let mut generated = fs::read_to_string(output.with_extension("fos")).unwrap();
    generated.push_str(
        r#"
func main() -> Result<Int, CError> {
    let before = try c_header_calls()
    let packet = CPacket {
        point: CVec { x: 1.25, y: -2.5 }
        tint: CColor { r: 255, g: 0, b: 128, a: 255 }
        bounds: CRectangle { width: 10.0, height: 20.0 }
        enabled: true
        delta: -123
        bits: -1
        c_type: 1
        c_c_type: 2
        c_copy: 3
    }
    let changed = try c_header_roundtrip(packet)
    assert(changed.point.x == 3.25 && changed.point.y == -2.5)
    assert(changed.tint.r == 255 && changed.tint.g == 42 && changed.tint.b == 128)
    assert(changed.bounds.width == 11.5 && changed.bounds.height == 20.0)
    assert(changed.enabled && changed.delta == -123 && changed.bits == -1)
    assert(changed.c_type == 1 && changed.c_c_type == 2 && changed.c_copy == 3)
    assert(packet.point.x == 1.25 && packet.tint.g == 0)
    let copied = changed.copy()
    assert(copied.point.x == 3.25)
    packet.tint.r = 256
    branch c_header_roundtrip(packet) {
        Result.Ok(_) -> panic("out of range struct field reached C")
        Result.Error(_) -> ()
    }
    assert((try c_header_calls()) == before + 1)
    let packed = try c_header_packed(CPacked { tag: 7, amount: 10.25 })
    assert(packed.tag == 7 && packed.amount == 20.5)
    c_header_add(20, 22)
}
"#,
    );
    let compilation = foster::compile(&generated).unwrap();
    assert_eq!(
        foster::vm::run(&compilation).unwrap().to_string(),
        "Result.Ok(42)"
    );
    for optimize in [false, true] {
        let executable = directory.join(format!("structs-{optimize}.exe"));
        foster::native::build_executable(
            &compilation,
            &executable,
            foster::native::CompileOptions { optimize },
        )
        .unwrap();
        let run = Command::new(executable).output().unwrap();
        assert!(
            run.status.success(),
            "{}",
            String::from_utf8_lossy(&run.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "Result.Ok(42)");
    }
    let path = output.to_str().unwrap();
    assert!(
        foster::foreign::runtime::exchange(path, &manifest.identity(), 2, 0, false, "")
            .starts_with("01")
    );
    let missing = invoke(&["-Functions", "missing_function"]);
    assert!(!missing.status.success(), "misspelled selections must fail");
    let mut wrong: serde_json::Value = serde_json::from_str(&manifest_text).unwrap();
    let color = wrong["records"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|r| r["name"] == "Color")
        .unwrap();
    color["fields"][0]["type"] = serde_json::json!("i32");
    let wrong_path = directory.join("wrong.json");
    fs::write(&wrong_path, serde_json::to_string(&wrong).unwrap()).unwrap();
    let error = foster::foreign::build(
        &wrong_path,
        &directory.join("wrong.dll"),
        std::path::Path::new("clang"),
    )
    .unwrap_err();
    assert!(error.contains("C field type mismatch"), "{error}");
}
