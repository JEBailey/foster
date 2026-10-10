#[cfg(all(windows, target_arch = "x86_64"))]
use foster::foreign::Manifest;
use foster::foreign::runtime;

#[cfg(all(windows, target_arch = "x86_64"))]
#[test]
fn bridge_dependencies_resolve_beside_forward_slash_paths() {
    let directory =
        std::env::temp_dir().join(format!("foster bridge dependencies {}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("dependency.c");
    std::fs::write(
        &source,
        "__declspec(dllexport) int linked_value(void) { return 42; }\n",
    )
    .unwrap();
    std::fs::write(directory.join("dependency.h"), "int linked_value(void);\n").unwrap();
    let compiled = std::process::Command::new("clang")
        .args(["--target=x86_64-pc-windows-msvc", "-shared", "-o"])
        .arg(directory.join("foster_test_dependency.dll"))
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let contract = r#"{"abi":1,"headers":["dependency.h"],"libraries":["foster_test_dependency.lib"],"operations":[{"name":"value","symbol":"linked_value","result":"i32"}]}"#;
    let manifest_path = directory.join("bindings.json");
    std::fs::write(&manifest_path, contract).unwrap();
    let library = foster::foreign::build(
        &manifest_path,
        &directory.join("bridge.dll"),
        std::path::Path::new("clang"),
        "bridge.dll",
    )
    .unwrap();
    let schema = Manifest::parse(contract).unwrap().identity();
    let forward = library.to_str().unwrap().replace('\\', "/");
    assert_eq!(
        runtime::exchange(&forward, &schema, 0, 0, false, &[]),
        runtime::response(Ok(42i64.to_le_bytes().to_vec()))
    );
}

#[test]
fn c_resources_are_private_and_cannot_cross_remote_boundaries() {
    for source in [
        "import std.ffi\nimport std.ffi.*\nimport static std.ffi.*\nfunc main() -> CResource { CResource { token: 1 } }",
        "import std.ffi\nimport std.ffi.*\nimport static std.ffi.*\nfunc transfer(value: CResource) { remote value }\nfunc main() {}",
        "import std.ffi\nimport std.ffi.*\nimport static std.ffi.*\ntype Box<T> = { value: T }\nfunc transfer(value: Box<CResource>) { remote value }\nfunc main() {}",
        "import std.ffi\nimport std.ffi.*\nimport static std.ffi.*\nimport core.result.*\ntype Box = { value: Result<CResource, Int> }\nfunc transfer(value: Box) { remote value }\nfunc main() {}",
    ] {
        let error = foster::compile(source)
            .expect_err("unsafe C ownership must be rejected")
            .to_string();
        assert!(
            error.contains("private fields") || error.contains("remote-object boundary"),
            "{error}"
        );
    }
}

#[test]
fn c_wire_rejects_malformed_and_preserves_exact_scalar_bits() {
    for bytes in [vec![], vec![0], vec![0; 7], vec![0; 9]] {
        assert!(runtime::int(&bytes).is_err());
    }
    for value in [i64::MIN, -1, 0, i64::MAX] {
        assert_eq!(runtime::int(&value.to_le_bytes()).unwrap(), value);
    }
    assert!(runtime::int(&[0]).is_err());
    assert!(runtime::exchange("relative.dll", "", 0, 0, false, &[]).starts_with(&[1]));
}

#[cfg(all(windows, target_arch = "x86_64"))]
#[test]
fn generated_c_bridge_runs_on_vm_and_native_with_exact_cleanup() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest_path = root.join("tests/fixtures/c_bridge/bindings.json");
    let manifest = Manifest::parse(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    let temporary =
        std::env::temp_dir().join(format!("foster-c-integration-{}", std::process::id()));
    std::fs::create_dir_all(&temporary).unwrap();
    let library = foster::foreign::build(
        &manifest_path,
        &temporary.join("fixture.dll"),
        std::path::Path::new("clang"),
        &temporary
            .join("fixture.dll")
            .to_string_lossy()
            .replace('\\', "/"),
    )
    .unwrap();
    let path = library.to_str().unwrap();
    let schema = manifest.identity();
    assert!(runtime::exchange(path, "stale", 0, 0, false, &[]).starts_with(&[1]));
    assert!(runtime::exchange(path, &schema, 9999, 0, false, &[]).starts_with(&[1]));
    let oversized = [i64::MAX.to_le_bytes(), 1i64.to_le_bytes()].concat();
    assert!(runtime::exchange(path, &schema, 0, 0, false, &oversized).starts_with(&[1]));
    // Constructors cannot be invoked as scalar calls to extract their tokens.
    assert!(runtime::exchange(path, &schema, 1, 0, false, &42i64.to_le_bytes()).starts_with(&[1]));
    let adopted = runtime::exchange(path, &schema, 1, 0, true, &42i64.to_le_bytes());
    assert!(adopted.starts_with(&[0]));
    let token = runtime::int(&adopted[1..]).unwrap();
    assert!(
        std::thread::spawn(move || runtime::exchange("", "", 2, token, false, &[]))
            .join()
            .unwrap()
            .starts_with(&[1])
    );
    runtime::release(token).unwrap();
    assert!(runtime::release(token).is_err());
    assert!(runtime::exchange("", "", 2, token, false, &[]).starts_with(&[1]));
    let compilation = foster::compile(include_str!("fixtures/c_bridge/main.fos")).unwrap();
    let arguments = foster::entry::CommandArguments::new("fixture", [path, &schema]);
    for optimize in [false, true] {
        let program =
            foster::vm::compile_with_options(&compilation, foster::vm::CompileOptions { optimize })
                .unwrap();
        let binary = foster::vm::encode_program(&program).unwrap();
        let decoded = foster::vm::decode_program(&binary).unwrap();
        let value = foster::vm::Machine::new(&decoded.clone().into_verified().unwrap())
            .run_main_with_arguments(&arguments)
            .unwrap();
        assert_eq!(value.to_string(), "Result.Ok(42)");
        let executable = temporary.join(format!("fixture-{optimize}.exe"));
        foster::native::build_executable(
            &compilation,
            &executable,
            foster::native::CompileOptions { optimize },
        )
        .unwrap();
        let output = std::process::Command::new(&executable)
            .args([path, &schema])
            .output()
            .unwrap();
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
    // Generated wrappers are checked as real Foster, including resource ownership.
    let generated = std::fs::read_to_string(library.with_extension("fos")).unwrap();
    let wrapped = foster::compile(&(generated + "\nfunc main() -> Result<Int, CError> {\nlet owner = try create(41)\ntry set(owner, 42)\nget(owner)\n}\n")).unwrap();
    assert_eq!(
        foster::vm::run(&wrapped).unwrap().to_string(),
        "Result.Ok(42)"
    );
    assert_eq!(
        runtime::exchange(path, &schema, 4, 0, false, &[]),
        vec![0; 9]
    );
    // An assertion still runs resource cleanup. An earlier local audit runs
    // after the resource's deinit and observes the C allocation counter at zero.
    let failure_source = include_str!("fixtures/c_bridge/main.fos")
        .replace("func main(arguments:", "func successful_main(arguments:")
        + r#"
type Audit = { bridge: CBridge }
impl Audit {
    func deinit(self: Self) -> () { println(live(self.bridge).unwrap_or(-1)) }
}
func main(arguments: Arguments) -> Result<(), CError> {
    let bridge = CBridge.at(arguments.values[0].copy(), arguments.values[1].copy())
    let audit = Audit { bridge: CBridge.at(arguments.values[0].copy(), arguments.values[1].copy()) }
    let resource = try create(bridge, 10)
    assert(false, "injected C owner failure")
    Result.Ok(())
}
"#;
    let failure = foster::compile(&failure_source).unwrap();
    assert!(foster::vm::run_with_arguments(&failure, Default::default(), &arguments).is_err());
    assert_eq!(
        runtime::exchange(path, &schema, 4, 0, false, &[]),
        vec![0; 9]
    );
    let executable = temporary.join("failure.exe");
    foster::native::build_executable(&failure, &executable, Default::default()).unwrap();
    let output = std::process::Command::new(executable)
        .args([path, &schema])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "0");
    assert!(String::from_utf8_lossy(&output.stderr).contains("injected C owner failure"));
    let generic_source = include_str!("fixtures/c_bridge/main.fos")
        .replace("func main(arguments:", "func successful_main(arguments:")
        + r#"
type Container<T> = { value: T }
func transfer<T>(value: Container<T>) -> () [consume value] {
    let worker = remote value
    ()
}
func main(arguments: Arguments) -> Result<(), CError> {
    let bridge = CBridge.at(arguments.values[0].copy(), arguments.values[1].copy())
    let owner = try create(bridge, 10)
    transfer(Container { value: move owner })
    Result.Ok(())
}
"#;
    let generic = foster::compile(&generic_source).unwrap();
    let error = foster::vm::run_with_arguments(&generic, Default::default(), &arguments)
        .unwrap_err()
        .to_string();
    assert!(error.contains("C resources cannot cross"), "{error}");
    assert_eq!(
        runtime::exchange(path, &schema, 4, 0, false, &[]),
        vec![0; 9]
    );
    let error = foster::native::compile_object(&generic, Default::default())
        .expect_err("native generic resource transfer must fail")
        .to_string();
    assert!(error.contains("C resources cannot cross"), "{error}");
}
