#[test]
fn callback_contracts_reject_unsafe_shapes() {
    for callback in [
        r#"{"name":"Event","method":"event","parameters":["c_string"],"result":"void","delivery":"direct"}"#,
        r#"{"name":"Event","method":"event","parameters":[],"result":"i32","delivery":"queued","failure":"zero"}"#,
        r#"{"name":"Event","method":"event","parameters":[],"result":"i32","delivery":"direct"}"#,
        r#"{"name":"Event","method":"event","parameters":[{"array":"i32","length":2}],"result":"void","delivery":"direct"}"#,
    ] {
        let manifest =
            format!(r#"{{"abi":1,"headers":[],"callbacks":[{callback}],"operations":[]}}"#);
        assert!(
            foster::foreign::Manifest::parse(&manifest).is_err(),
            "{callback}"
        );
    }
    for parameter in [
        r#"{"callback":"Missing"}"#,
        r#"{"callback":"Event","retained":true}"#,
    ] {
        let manifest = format!(
            r#"{{"abi":1,"headers":[],"callbacks":[{{"name":"Event","method":"event","parameters":[],"result":"void","delivery":"direct"}}],"operations":[{{"name":"f","symbol":"f","parameters":[{parameter}],"result":"void"}}]}}"#
        );
        assert!(
            foster::foreign::Manifest::parse(&manifest).is_err(),
            "{parameter}"
        );
    }
}

#[cfg(all(windows, target_arch = "x86_64"))]
#[test]
fn callbacks_preserve_state_lifetimes_and_thread_boundaries_on_both_backends() {
    use std::{fs, process::Command};
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixture = root.join("tests/fixtures/c_bridge");
    let directory = root.join("target/callback-tests");
    fs::create_dir_all(&directory).unwrap();
    let output = foster::foreign::build(
        &fixture.join("callbacks.json"),
        &directory.join("callbacks.dll"),
        std::path::Path::new("clang"),
    )
    .unwrap();
    let source = fs::read_to_string(output.with_extension("fos")).unwrap()
        + "\n"
        + &fs::read_to_string(fixture.join("callbacks.fos")).unwrap();
    fs::write(directory.join("test.fos"), &source).unwrap();
    let compiled = foster::compile(&source).unwrap();
    assert_eq!(
        foster::vm::run(&compiled).unwrap().to_string(),
        "Result.Ok(42)"
    );
    for optimize in [false, true] {
        let executable = directory.join(format!("callbacks-{optimize}.exe"));
        foster::native::build_executable(
            &compiled,
            &executable,
            foster::native::CompileOptions { optimize },
        )
        .unwrap();
        let run = Command::new(executable).output().unwrap();
        assert!(
            run.status.success(),
            "native callback fixture failed: {}\n{}",
            run.status,
            String::from_utf8_lossy(&run.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "Result.Ok(42)");
    }
}
