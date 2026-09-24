use foster::foreign::Manifest;

#[test]
fn pointer_contracts_require_bounds_directions_and_cleanup() {
    for operation in [
        r#"{"name":"f","symbol":"f","result":"c_string","borrowed_result":true}"#,
        r#"{"name":"f","symbol":"f","result":"c_string","max_length":10}"#,
        r#"{"name":"f","symbol":"f","result":"bytes","release":"free","borrowed_result":true}"#,
        r#"{"name":"f","symbol":"f","result":{"out":"i32"}}"#,
        r#"{"name":"f","symbol":"f","result":"void","parameters":[{"out":"void"}]}"#,
        r#"{"name":"f","symbol":"f","result":"void","parameters":[{"buffer":"u8","length":"i16"}]}"#,
        r#"{"name":"f","symbol":"f","result":"c_string","borrowed_result":true,"max_length":16777217}"#,
    ] {
        let source = format!(r#"{{"abi":1,"headers":[],"operations":[{operation}]}}"#);
        assert!(Manifest::parse(&source).is_err(), "{operation}");
    }
}

#[cfg(all(windows, target_arch = "x86_64"))]
#[test]
fn copied_pointers_outputs_and_value_resources_work_on_vm_and_native() {
    use std::{fs, process::Command};
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixture = root.join("tests/fixtures/c_bridge");
    let directory = std::env::temp_dir().join(format!("foster-pointer-contracts-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let output = foster::foreign::build(
        &fixture.join("pointers.json"),
        &directory.join("pointers.dll"),
        std::path::Path::new("clang"),
    )
    .unwrap();
    let source = fs::read_to_string(output.with_extension("fos")).unwrap()
        + "\n"
        + &fs::read_to_string(fixture.join("pointers.fos")).unwrap();
    let compiled = foster::compile(&source).unwrap();
    assert_eq!(foster::vm::run(&compiled).unwrap().to_string(), "Result.Ok(42)");
    for optimize in [false, true] {
        let executable = directory.join(format!("pointers-{optimize}.exe"));
        foster::native::build_executable(
            &compiled,
            &executable,
            foster::native::CompileOptions { optimize },
        )
        .unwrap();
        let run = Command::new(executable).output().unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "Result.Ok(42)");
    }
}
