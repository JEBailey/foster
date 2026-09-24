//! End-to-end coverage of Clang discovery, Foster mapping, and generated bindings.
#[cfg(all(windows, target_arch = "x86_64"))]
#[test]
fn foster_header_tool_builds_scalars_and_reports_unsupported_declarations() {
    use std::{fs, process::Command};
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let directory = std::env::temp_dir().join(format!("foster header tool {}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let tool = directory.join("cbind.exe");
    let compiled = Command::new(env!("CARGO_BIN_EXE_foster"))
        .arg("build")
        .arg(root.join("tools/cbind"))
        .arg("--native")
        .arg("--output")
        .arg(&tool)
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&compiled.stdout),
        String::from_utf8_lossy(&compiled.stderr)
    );
    let header = directory.join("sample header.h");
    let source = directory.join("sample source.c");
    let output = directory.join("sample.dll");
    fs::write(
        directory.join("dependency.h"),
        "#define DEPENDENCY_ONLY 91\nint dependency_only(void);\n",
    )
    .unwrap();
    fs::write(
        &header,
        r#"
#include <stdint.h>
#include "dependency.h"
typedef int32_t Count;
#define VERSION "6.0\nquoted \"value\""
#define WIDE_TEXT L"wide"
#define MASK ((1u << 5) | 3u)
#define MASK_ALIAS MASK
#define FRACTION (1.0 / 4.0)
#define WIDE_BITS 0xffffffffffffffffULL
#define MINIMUM (-9223372036854775807LL - 1LL)
#define GONE 3
#undef GONE
#define CHANGED 1
#undef CHANGED
#define CHANGED 42
#define MARKER
#define NOT_CONSTANT header_zero()
typedef enum Mode { MODE_NEGATIVE = -3, MODE_NEXT, MODE_FLAG = 1 << 7, MODE_ALIAS = MODE_FLAG } Mode;
typedef enum { FLAG_HIGH = 0x80000000u } Flags;
typedef Mode ModeAlias;
#define DECLARE(name) Count name(Count a, Count b)
DECLARE(header_add);
/// Returns zero without changing state.
int header_zero(void);
int header_text(const char *text);
void *header_open(void);
int header_log(const char *, ...);
long header_long(long);
typedef struct Vec { float x, y; } Vec;
typedef struct Color { uint8_t r, g, b, a; } Color;
#define RED ((Color){ .r = 230, .g = 41, .b = 55, .a = 255 })
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
typedef struct Array { int data[4]; float grid[2][2]; Vec points[2]; char name[4]; Mode mode; Flags flags; } Array;
#define ARRAY_VALUE ((Array){ .data = {1,2,3,4}, .grid = {{1,2},{3,4}}, .points = {{1,2},{3,4}}, .name = {'a',0,'b',(char)255}, .mode = MODE_NEGATIVE, .flags = FLAG_HIGH })
typedef struct Flexible { int count; int data[]; } Flexible;
typedef struct Bits { unsigned int value : 3; } Bits;
typedef union Union { int i; float f; } Union;
Pointer bad_pointer(Pointer value);
Array header_array(Array value);
ModeAlias header_mode(ModeAlias value);
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
int header_text(const char *text) { return text ? 42 : 0; }
Packet header_roundtrip(Packet value) { calls++; value.point.x += 2; value.tint.g = 42; value.bounds.width += 1.5f; return value; }
Packed header_packed(Packed value) { value.amount *= 2; return value; }
int header_calls(void) { return calls; }
Array header_array(Array value) { calls++; value.data[3] += 10; value.grid[1][0] += 0.5f; value.points[1].x += 2; return value; }
ModeAlias header_mode(ModeAlias value) { return value; }
"#).unwrap();
    let invoke = |extra: &[&str]| {
        Command::new(&tool)
            .current_dir(&directory)
            .arg("--header")
            .arg(&header)
            .arg("--output")
            .arg(&output)
            .arg("--source")
            .arg(&source)
            .args(extra)
            .output()
            .unwrap()
    };
    let rejected = invoke(&[]);
    assert!(
        !rejected.status.success(),
        "unsupported declarations must fail by default"
    );
    let report =
        fs::read_to_string(output.with_extension("unsupported.txt")).unwrap_or_else(|error| {
            panic!(
                "missing report: {error}; stdout: {}; stderr: {}",
                String::from_utf8_lossy(&rejected.stdout),
                String::from_utf8_lossy(&rejected.stderr)
            )
        });
    for symbol in [
        "header_open",
        "header_text",
        "header_log",
        "header_long",
        "bad_pointer",
        "Flexible",
        "bad_bits",
        "bad_union",
        "DECLARE",
        "NOT_CONSTANT",
        "WIDE_TEXT",
    ] {
        assert!(report.contains(symbol), "{report}");
    }
    assert!(
        !report.contains("dependency_only"),
        "included headers must not be imported"
    );
    let selected = invoke(&[
        "--function",
        "header_add",
        "--function",
        "header_text",
        "--c-string",
        "header_text:0",
        "--manifest-only",
    ]);
    assert!(
        selected.status.success(),
        "{}",
        String::from_utf8_lossy(&selected.stderr)
    );
    let selected_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(output.with_extension("bindings.json")).unwrap())
            .unwrap();
    assert_eq!(selected_json["operations"].as_array().unwrap().len(), 2);
    assert_eq!(selected_json["operations"][1]["parameters"][0], "c_string");
    let invalid_string = invoke(&[
        "--function",
        "header_add",
        "--c-string",
        "header_add:0",
        "--manifest-only",
    ]);
    assert!(!invalid_string.status.success());
    let missing_compiler = invoke(&["--clang", "foster-cbind-missing-compiler"]);
    assert!(!missing_compiler.status.success());
    let built = invoke(&["--skip-unsupported"]);
    assert!(
        built.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr)
    );
    let manifest_text = fs::read_to_string(output.with_extension("bindings.json")).unwrap();
    let manifest = foster::foreign::Manifest::parse(&manifest_text).unwrap();
    let document: serde_json::Value = serde_json::from_str(&manifest_text).unwrap();
    assert_eq!(document["operations"].as_array().unwrap().len(), 7);
    let constants = document["constants"].as_array().unwrap();
    let constant = |name: &str| constants.iter().find(|c| c["name"] == name).unwrap();
    assert_eq!(constant("MODE_NEXT")["value"], -2);
    assert_eq!(constant("MODE_ALIAS")["value"], 128);
    assert_eq!(constant("MASK_ALIAS")["value"], 35);
    assert_eq!(constant("CHANGED")["value"], 42);
    assert_eq!(constant("RED")["value"]["g"], 41);
    assert!(
        !constants
            .iter()
            .any(|c| ["DEPENDENCY_ONLY", "GONE", "MARKER"].contains(&c["name"].as_str().unwrap()))
    );
    // The standalone tool also builds reviewed contracts, resolving their paths
    // beside the manifest even when invoked from another working directory.
    let mut reviewed = document.clone();
    reviewed["sources"] = serde_json::json!(["sample source.c"]);
    reviewed["headers"] = serde_json::json!(["sample.import.h"]);
    let reviewed_path = directory.join("reviewed.json");
    fs::write(&reviewed_path, serde_json::to_string(&reviewed).unwrap()).unwrap();
    let reviewed_build = Command::new(&tool)
        .current_dir(root)
        .arg("--manifest")
        .arg(&reviewed_path)
        .arg("--output")
        .arg(directory.join("reviewed.dll"))
        .output()
        .unwrap();
    assert!(
        reviewed_build.status.success(),
        "{}",
        String::from_utf8_lossy(&reviewed_build.stderr)
    );
    let mut generated = fs::read_to_string(output.with_extension("fos")).unwrap();
    assert!(
        generated.contains("c_header_add(a: Int, b: Int)"),
        "{generated}"
    );
    assert!(generated.contains("Returns zero without changing state."));
    assert!(generated.contains("pub type CMode = Int"));
    assert!(generated.contains("pub type CModeAlias = Int"));
    assert!(generated.contains("pub type CFlags = Int"));
    assert!(generated.contains("pub grid: List<List<Float>>"));
    generated.push_str(
        r#"
func main() -> Result<Int, CError> {
    assert(C_MASK == 35 && C_MASK_ALIAS == 35)
    assert(C_MODE_NEGATIVE == -3 && C_MODE_NEXT == -2 && C_MODE_ALIAS == 128)
    assert(C_FRACTION == 0.25 && C_WIDE_BITS == -1 && C_CHANGED == 42)
    assert(C_MINIMUM() == -9223372036854775807 - 1)
    assert(C_VERSION == "6.0\nquoted \"value\"")
    let red = C_RED()
    assert(red.r == 230 && red.g == 41 && red.b == 55 && red.a == 255)
    let before = try c_header_calls()
    let array = C_ARRAY_VALUE()
    let copied_array = array.copy()
    let array_result = try c_header_array(array)
    assert(array_result.data == [1,2,3,14])
    assert(array_result.grid[1][0] == 3.5 && array_result.points[1].x == 5.0)
    assert(array_result.name == [97,0,98,255])
    assert(array_result.mode == -3 && array_result.flags == C_FLAG_HIGH)
    assert(copied_array.data[3] == 4 && copied_array.grid[1][0] == 3.0)
    let mode = C_MODE_NEGATIVE
    assert((try c_header_mode(mode)) == -3)
    array.data = [1,2,3]
    branch c_header_array(array) { Result.Ok(_) -> panic("short array reached C") Result.Error(_) -> () }
    array.data = [1,2,3,4,5]
    branch c_header_array(array) { Result.Ok(_) -> panic("long array reached C") Result.Error(_) -> () }
    array.data = [1,2,3,4]
    array.grid = [[1.0,2.0],[3.0]]
    branch c_header_array(array) { Result.Ok(_) -> panic("short nested array reached C") Result.Error(_) -> () }
    array.grid = [[1.0,2.0],[3.0,4.0]]
    array.name = [0,1,2,256]
    branch c_header_array(array) { Result.Ok(_) -> panic("invalid char byte reached C") Result.Error(_) -> () }
    array.name = [0,1,2,3]
    array.flags = 4294967296
    branch c_header_array(array) { Result.Ok(_) -> panic("enum overflow reached C") Result.Error(_) -> () }
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
    assert((try c_header_calls()) == before + 2)
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
    let missing = invoke(&["--function", "missing_function"]);
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

    let mut wrong_array = document.clone();
    let array = wrong_array["records"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|record| record["name"] == "Array")
        .unwrap();
    array["fields"][0]["type"]["length"] = serde_json::json!(3);
    wrong_array["constants"] = serde_json::json!([]);
    fs::write(&wrong_path, serde_json::to_string(&wrong_array).unwrap()).unwrap();
    let error = foster::foreign::build(
        &wrong_path,
        &directory.join("wrong-array.dll"),
        std::path::Path::new("clang"),
    )
    .unwrap_err();
    assert!(
        error.contains("C field type mismatch: Array.data"),
        "{error}"
    );

    // A header need not declare any functions to produce a usable Foster module.
    let constants_header = directory.join("constants only.h");
    fs::write(
        &constants_header,
        r#"
#define ANSWER (6 * 7)
#define BINARY "a\0b"
#define UNICODE "caf\xc3\xa9"
enum { FIRST = -4, SECOND };
"#,
    )
    .unwrap();
    let constants_output = directory.join("constants.dll");
    let built = Command::new(&tool)
        .arg("--header")
        .arg(&constants_header)
        .arg("--output")
        .arg(&constants_output)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr)
    );
    let mut source = fs::read_to_string(constants_output.with_extension("fos")).unwrap();
    source.push_str("\nfunc main() -> Int {\nassert(C_SECOND == -3)\nassert(C_BINARY.bytes.length == 3 && C_BINARY.bytes[1].int == 0)\nassert(C_UNICODE == \"café\")\nC_ANSWER\n}\n");
    let compilation = foster::compile(&source).unwrap();
    assert_eq!(foster::vm::run(&compilation).unwrap().to_string(), "42");
}
