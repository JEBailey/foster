use foster_host::{HostContext, HostProvider};
use std::io::{self, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

const SOURCE: &str = include_str!("fixtures/programs/standard_input.fos");
const INPUT: &[u8] = b"h\xc3\xa9llo\r\n\n\xff\nlast\r";

struct InputHost(Mutex<std::io::Cursor<Vec<u8>>>);
impl HostProvider for InputHost {
    fn stdin_read(&self, maximum: usize) -> io::Result<Vec<u8>> {
        use std::io::Read;
        let mut bytes = vec![0; maximum];
        let count = self.0.lock().unwrap().read(&mut bytes)?;
        bytes.truncate(count);
        Ok(bytes)
    }
}

#[test]
fn standard_input_uses_provider_in_both_vm_modes() {
    let compilation = foster::compile(SOURCE).unwrap();
    for optimize in [false, true] {
        let program =
            foster::vm::compile_with_options(&compilation, foster::vm::CompileOptions { optimize })
                .unwrap()
                .into_verified()
                .unwrap();
        let host = HostContext::with_provider(
            ".",
            Arc::new(InputHost(Mutex::new(std::io::Cursor::new(INPUT.to_vec())))),
        );
        assert_eq!(
            foster::vm::Machine::with_host_context(&program, host)
                .run_main()
                .unwrap(),
            foster::vm::Value::Integer(0)
        );
    }
}

#[test]
fn binary_reads_and_adapters_share_input_cursor() {
    let compilation = foster::compile(
        r#"
import core.bytes
import core.result.Result
import core.option.Option
import std.io
import std.io.Stdin
func main() -> Int {
    let first = Stdin.new()
    let second = Stdin.new()
    branch first.read(2) {
        Result.Ok(contents) -> { assert(contents.hex() == "00ff") }
        _ -> panic("binary read failed")
    }
    branch second.read_line() {
        Result.Ok(Option.Some(line)) -> { assert(line == "shared") }
        _ -> panic("shared line read failed")
    }
    branch io::read_all(first) {
        Result.Ok(contents) -> { assert(contents.hex() == "010203") }
        _ -> panic("read_all failed")
    }
    0
}
"#,
    )
    .unwrap();
    for optimize in [false, true] {
        let program =
            foster::vm::compile_with_options(&compilation, foster::vm::CompileOptions { optimize })
                .unwrap()
                .into_verified()
                .unwrap();
        let host = HostContext::with_provider(
            ".",
            Arc::new(InputHost(Mutex::new(std::io::Cursor::new(
                b"\0\xffshared\n\x01\x02\x03".to_vec(),
            )))),
        );
        assert_eq!(
            foster::vm::Machine::with_host_context(&program, host)
                .run_main()
                .unwrap(),
            foster::vm::Value::Integer(0)
        );
    }
}

fn run_piped(mut command: Command) {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(INPUT).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn command_line_reads_redirected_standard_input() {
    let mut command = Command::new(env!("CARGO_BIN_EXE_foster"));
    command.arg("run").arg(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/programs/standard_input.fos"
    ));
    run_piped(command);
}

#[test]
fn native_executable_reads_redirected_standard_input() {
    let compilation = foster::compile(SOURCE).unwrap();
    let executable = std::env::temp_dir().join(format!(
        "foster-stdin-{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    foster::native::build_executable(
        &compilation,
        &executable,
        foster::native::CompileOptions::default(),
    )
    .unwrap();
    run_piped(Command::new(&executable));
    std::fs::remove_file(executable).unwrap();
}

#[test]
fn input_provider_denial_errors_and_bounds_are_preserved() {
    struct Denied;
    impl HostProvider for Denied {}
    let host = HostContext::with_provider(".", Arc::new(Denied));
    assert_eq!(
        host.stdin_read(1).unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    for maximum in [-1, 0, 1048577, i64::MAX] {
        assert_eq!(
            host.stdin_read(maximum).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
    struct Oversized;
    impl HostProvider for Oversized {
        fn stdin_read(&self, _: usize) -> io::Result<Vec<u8>> {
            Ok(vec![0; 2])
        }
    }
    let host = HostContext::with_provider(".", Arc::new(Oversized));
    assert_eq!(
        host.stdin_read(1).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    struct Failed;
    impl HostProvider for Failed {
        fn stdin_read(&self, _: usize) -> io::Result<Vec<u8>> {
            Err(io::Error::other("input failed"))
        }
    }
    assert_eq!(
        HostContext::with_provider(".", Arc::new(Failed))
            .stdin_read(1)
            .unwrap_err()
            .to_string(),
        "input failed"
    );
}
