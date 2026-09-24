use std::path::Path;
use std::process::Command;

fn check_output(output: std::process::Output, expected: &str) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), expected);
}

#[test]
fn composed_futures_and_remote_forwarding_run_on_both_backends() {
    let compilation =
        foster::compile(include_str!("fixtures/programs/composable_future.fos")).unwrap();
    assert_eq!(foster::vm::run(&compilation).unwrap().to_string(), "42");
    let directory = std::env::temp_dir().join(format!("foster-future-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let executable = directory.join(format!("future{}", std::env::consts::EXE_SUFFIX));
    foster::native::build_executable(&compilation, &executable, Default::default()).unwrap();
    check_output(Command::new(executable).output().unwrap(), "42");
}

fn prepare(directory: &Path) {
    std::fs::create_dir_all(directory).unwrap();
    for name in ["ready", "survived", "drop-ready", "drop-survived"] {
        let _ = std::fs::remove_file(directory.join(name));
    }
    std::fs::write(directory.join("cwd-marker"), "marker").unwrap();
}

#[test]
fn processes_capture_poll_terminate_and_cleanup_on_both_backends() {
    let directory = std::env::temp_dir().join(format!("foster-process-{}", std::process::id()));
    prepare(&directory);
    let helper = directory.join(format!("child{}", std::env::consts::EXE_SUFFIX));
    let output = Command::new("rustc")
        .arg("--edition=2024")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/process_child.rs"))
        .arg("-o")
        .arg(&helper)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let compilation = foster::compile(include_str!("fixtures/programs/process_spawn.fos")).unwrap();
    let arguments = foster::entry::CommandArguments::new(
        "fixture",
        [helper.to_str().unwrap(), directory.to_str().unwrap()],
    );
    let program = foster::vm::compile_with_options(
        &compilation,
        foster::vm::CompileOptions { optimize: true },
    )
    .unwrap();
    let encoded = foster::vm::encode_program(&program).unwrap();
    let decoded = foster::vm::decode_program(&encoded).unwrap();
    assert_eq!(
        foster::vm::Machine::new(&decoded)
            .run_main_with_arguments(&arguments)
            .unwrap()
            .to_string(),
        "Result.Ok(42)"
    );
    std::thread::sleep(std::time::Duration::from_millis(2200));
    assert!(!directory.join("survived").exists());
    assert!(!directory.join("drop-survived").exists());

    prepare(&directory);
    let executable = directory.join(format!("process{}", std::env::consts::EXE_SUFFIX));
    foster::native::build_executable(&compilation, &executable, Default::default()).unwrap();
    check_output(
        Command::new(executable)
            .args([helper.as_os_str(), directory.as_os_str()])
            .output()
            .unwrap(),
        "Result.Ok(42)",
    );
    std::thread::sleep(std::time::Duration::from_millis(2200));
    assert!(!directory.join("survived").exists());
    assert!(!directory.join("drop-survived").exists());
}

#[test]
fn await_requires_consumption_and_process_handles_are_private() {
    for source in [
        "import core.future\nimport core.result\nimport core.remote_error\ntype Worker = {}\nimpl Worker { func work(self) -> Int { 1 }\nfunc accept(self, value: Future<Result<Int, RemoteError>>) -> Int [consume value] { 1 } }\nfunc main() -> Int { let first = remote Worker {}\nlet second = remote Worker {}\nlet pending = first.work()\nawait second.accept(move pending)\n0 }",
        "type Bad = { func resolve(self) -> Int [read self] }\nimpl Bad { func resolve(self) -> Int [read self] { 1 } }\nfunc main() -> Int { await Bad {} }",
        "import std.process\nfunc main() -> Process { Process { token: 1, identifier: 1 } }",
        "import core.future\ntype Ready = & Future<Int> & {}\nimpl Ready { func resolve(self) -> Int [consume self] { 1 } }\nfunc main() -> Int { let ready = Ready {}\nawait ready\nawait ready }",
    ] {
        assert!(
            foster::compile(source).is_err(),
            "invalid future accepted: {source}"
        );
    }
}

#[test]
fn process_cleanup_runs_during_unwinding_on_both_backends() {
    let directory =
        std::env::temp_dir().join(format!("foster-process-unwind-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let helper = directory.join(format!("child{}", std::env::consts::EXE_SUFFIX));
    let output = Command::new("rustc")
        .arg("--edition=2024")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/process_child.rs"))
        .arg("-o")
        .arg(&helper)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let source = r#"
import std.process
import std.fs
import core.result
func main(arguments: Arguments) -> Result<Int, ProcessError> {
    let child = try process::spawn(arguments.values[0], ["sleep", arguments.values[1].copy(), arguments.values[2].copy()])
    loop { break if fs::exists?(arguments.values[1]) }
    assert(child.pid() > 0)
    panic("process cleanup test")
}
"#;
    let compilation = foster::compile(source).unwrap();
    let ready = directory.join("ready");
    let survived = directory.join("survived");
    prepare(&directory);
    let arguments = foster::entry::CommandArguments::new(
        "fixture",
        [
            helper.to_str().unwrap(),
            ready.to_str().unwrap(),
            survived.to_str().unwrap(),
        ],
    );
    let error =
        foster::vm::run_with_arguments(&compilation, Default::default(), &arguments).unwrap_err();
    assert!(error.to_string().contains("process cleanup test"));
    std::thread::sleep(std::time::Duration::from_millis(2200));
    assert!(!survived.exists());
    prepare(&directory);
    let executable = directory.join(format!("unwind{}", std::env::consts::EXE_SUFFIX));
    foster::native::build_executable(&compilation, &executable, Default::default()).unwrap();
    let output = Command::new(executable)
        .args([&helper, &ready, &survived])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("process cleanup test"));
    std::thread::sleep(std::time::Duration::from_millis(2200));
    assert!(!survived.exists());
}
