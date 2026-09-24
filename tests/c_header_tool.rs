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
    fs::write(&header, "#include <stdint.h>\n#include \"dependency.h\"\ntypedef int32_t Count;\n#define DECLARE(name) Count name(Count a, Count b)\nDECLARE(header_add);\nint header_zero(void);\nvoid *header_open(void);\nint header_log(const char *, ...);\nlong header_long(long);\n").unwrap();
    fs::write(&source, "#include \"sample header.h\"\nCount header_add(Count a, Count b) { return a+b; }\nint header_zero(void) { return 0; }\n").unwrap();
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
    for symbol in ["header_open", "header_log", "header_long"] {
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
    assert_eq!(manifest.operations.len(), 2);
    let mut generated = fs::read_to_string(output.with_extension("fos")).unwrap();
    generated.push_str("\nfunc main() -> Result<Int, CError> { c_header_add(20, 22) }\n");
    let compilation = foster::compile(&generated).unwrap();
    assert_eq!(
        foster::vm::run(&compilation).unwrap().to_string(),
        "Result.Ok(42)"
    );
    let missing = invoke(&["-Functions", "missing_function"]);
    assert!(!missing.status.success(), "misspelled selections must fail");
}
