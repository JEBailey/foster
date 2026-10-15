use foster::foreign::Manifest;

#[test]
fn borrowed_buffer_contracts_reject_unsupported_layouts_and_retention() {
    for parameter in [
        r#"{"buffer":"f32","length":"size","borrowed":true}"#,
        r#"{"buffer":"i32","length":"size","borrowed":true}"#,
        r#"{"buffer":"u8","length":"size","borrowed":true,"mutable":true}"#,
        r#"{"buffer":"i64","length":"size","borrowed":true,"output":true}"#,
        r#"{"buffer":"u8","length":"size","borrowed":true,"retained":true}"#,
    ] {
        let manifest = format!(
            r#"{{"abi":1,"headers":[],"operations":[{{"name":"test","symbol":"test","parameters":[{parameter}],"result":"void"}}]}}"#
        );
        assert!(Manifest::parse(&manifest).is_err(), "{parameter}");
    }
    let parameter = r#"{"buffer":"i64","length":"size","borrowed":true}"#;
    let manifest = format!(
        r#"{{"abi":1,"headers":[],"operations":[{{"name":"test","symbol":"test","parameters":[{parameter},{parameter}],"result":"void"}}]}}"#
    );
    assert!(Manifest::parse(&manifest).is_err());
    for mode in ["receiver", "creates"] {
        let manifest = format!(
            r#"{{"abi":1,"headers":[],"resources":[{{"name":"Owner","c_type":"Owner","destroy":"destroy_owner"}}],"operations":[{{"name":"test","symbol":"test","parameters":[{parameter}],"result":"void","{mode}":"Owner"}}]}}"#
        );
        assert!(Manifest::parse(&manifest).is_err());
    }
    let manifest = format!(
        r#"{{"abi":1,"headers":[],"callbacks":[{{"name":"Event","method":"event","parameters":[],"result":"void","delivery":"direct"}}],"operations":[{{"name":"test","symbol":"test","parameters":[{parameter},{{"callback":"Event"}}],"result":"void"}}]}}"#
    );
    assert!(Manifest::parse(&manifest).is_err());
}

#[cfg(all(windows, target_arch = "x86_64"))]
#[test]
fn borrowed_buffers_preserve_layout_lifetimes_and_checks_in_both_backends() {
    use std::{fs, path::Path, process::Command};
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixture = root.join("tests/fixtures/c_bridge");
    let directory = root
        .join("target")
        .join(format!("borrowed-c-test-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let dll = directory.join("borrowed.dll");
    foster::foreign::build(
        &fixture.join("borrowed.json"),
        &dll,
        Path::new("clang"),
        &dll.to_string_lossy().replace('\\', "/"),
    )
    .unwrap();
    let bindings = fs::read_to_string(dll.with_extension("fos")).unwrap();
    let adapter = fs::read_to_string(dll.with_extension("bridge.c")).unwrap();
    let borrowed_case = adapter
        .split("case 1: {")
        .last()
        .unwrap()
        .split("case 2: {")
        .next()
        .unwrap();
    assert!(borrowed_case.contains("foster_borrow"));
    assert!(!borrowed_case.contains("calloc"));
    let body = fs::read_to_string(fixture.join("borrowed.fos")).unwrap();
    let (imports, declarations): (Vec<_>, Vec<_>) =
        body.lines().partition(|line| line.starts_with("import "));
    let bindings = bindings.replacen(
        "import std.ffi.*",
        &("import std.ffi.*\n".to_owned() + &imports.join("\n")),
        1,
    );
    let compiled = foster::compile(&(bindings + "\n" + &declarations.join("\n"))).unwrap();
    for optimize in [false, true] {
        assert_eq!(
            foster::vm::run_with_options(&compiled, foster::vm::CompileOptions { optimize })
                .unwrap()
                .to_string(),
            "Result.Ok(42)"
        );
        let executable = directory.join(format!("borrowed-{optimize}.exe"));
        foster::native::build_executable(
            &compiled,
            &executable,
            foster::native::CompileOptions { optimize },
        )
        .unwrap();
        let output = Command::new(executable).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "Result.Ok(42)"
        );
    }
    // Prove that the host resolver passes the caller's actual array address.
    let data = [7i64, 8, 9];
    let schema = Manifest::parse(&fs::read_to_string(fixture.join("borrowed.json")).unwrap())
        .unwrap()
        .identity();
    let oversized = vec![0u8; foster::foreign::runtime::LIMIT + 1];
    assert_eq!(
        foster::foreign::runtime::exchange_buffer(
            &dll.to_string_lossy(),
            &schema,
            5,
            &[],
            foster::foreign::runtime::BorrowedBuffer::Bytes(&oversized),
        )
        .first(),
        Some(&1),
    );
    assert_eq!(
        foster::foreign::runtime::exchange_buffer(
            &dll.to_string_lossy(),
            "wrong-schema",
            6,
            &[],
            foster::foreign::runtime::BorrowedBuffer::Integers(&data),
        )
        .first(),
        Some(&1),
    );
    let response = foster::foreign::runtime::exchange_buffer(
        &dll.to_string_lossy(),
        &schema,
        6,
        &[],
        foster::foreign::runtime::BorrowedBuffer::Integers(&data),
    );
    let mut expected = 8i64.to_le_bytes().to_vec();
    expected.extend_from_slice(&(data.as_ptr() as usize as i64).to_le_bytes());
    assert_eq!(response, foster::foreign::runtime::response(Ok(expected)));
}
