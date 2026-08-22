use std::io::{Read, Write};
use std::net::TcpListener;

use foster::vm::Value;

fn assert_string(value: Value, expected: &str) {
    assert_eq!(value.as_string(), Some(expected));
}

#[test]
fn every_standard_library_function_has_attached_documentation() {
    let compilation = foster::compile(
        r#"
import std.sequence
func main() -> Int { 0 }
"#,
    )
    .unwrap();
    let mut checked = 0;
    for (_, function) in compilation.hir.functions.iter() {
        let module = &compilation.hir.modules[function.module].name;
        if !(module.starts_with("core.") || module.starts_with("std."))
            || function.name.contains('$')
            || function.test_description.is_some()
        {
            continue;
        }
        checked += 1;
        assert!(
            function
                .documentation
                .as_deref()
                .is_some_and(|documentation| !documentation.trim().is_empty()),
            "{module}.{} is missing documentation",
            function.name
        );
    }
    assert_eq!(checked, 298);
}

#[test]
fn foster_written_string_and_integer_algorithms_execute() {
    let source = r#"
import core.int
import core.string

func main() -> String {
    parts = string.split("one,two,three", ",")
    string.upper(string.join(parts, "-") + ":" + int.to_string(-42))
}
"#;
    assert_string(foster::run(source).unwrap(), "ONE-TWO-THREE:-42");
}

#[test]
fn foster_written_map_constructs_through_its_associated_factory() {
    let source = r#"
import std.collections.map
import core.option

func option_value(value: Option<Int>) -> Int {
    branch value {
        Option.Some(number) -> number
        Option.None -> 0
    }
}

func main() -> Int {
    values = Map.empty()
    values = put(move values, "answer", 42)
    option_value(get(move values, "answer"))
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));
}

#[test]
fn core_io_reads_writes_and_inspects_host_files() {
    let directory = std::env::temp_dir().join(format!(
        "foster-core-io-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("message.txt");
    let path_literal = serde_json::to_string(&path.to_string_lossy()).unwrap();
    let source = format!(
        r#"
import std.fs
import std.io
import core.result

func read_after_write(path: String, outcome: Result<Unit, IoError>) -> String {{
    branch outcome {{
        Result.Error(error) -> error.message
        Result.Ok(_) -> read_result(read_text(path))
    }}
}}

func read_result(outcome: Result<String, IoError>) -> String {{
    branch outcome {{
        Result.Error(error) -> error.message
        Result.Ok(text) -> text
    }}
}}

func main() -> String {{
    path = {path_literal}
    read_after_write(path, write_text(path, "hello from Foster"))
}}
"#
    );

    let value = foster::run(&source).unwrap();
    assert_string(value, "hello from Foster");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello from Foster");
    std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn core_io_preserves_arbitrary_binary_files() {
    let directory = std::env::temp_dir().join(format!(
        "foster-core-binary-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("payload.bin");
    let path_literal = serde_json::to_string(&path.to_string_lossy()).unwrap();
    let source = format!(
        r#"
import core.bytes
import std.fs
import std.io
import core.result

func bytes_or_empty(outcome: Result<Bytes, HexError>) -> Bytes {{
    branch outcome {{
        Result.Error(_) -> Bytes.empty()
        Result.Ok(contents) -> contents
    }}
}}

func read_after_write(path: String, outcome: Result<Unit, IoError>) -> String {{
    branch outcome {{
        Result.Error(error) -> error.message
        Result.Ok(_) -> render(read_bytes(path))
    }}
}}

func render(outcome: Result<Bytes, IoError>) -> String {{
    branch outcome {{
        Result.Error(error) -> error.message
        Result.Ok(contents) -> contents.hex
    }}
}}

func main() -> String {{
    path = {path_literal}
    contents = bytes_or_empty(Bytes.from_hex("00ff8041"))
    read_after_write(path, write_bytes(path, contents))
}}
"#
    );

    assert_string(foster::run(&source).unwrap(), "00ff8041");
    assert_eq!(std::fs::read(&path).unwrap(), [0, 255, 128, 65]);
    std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn standard_path_and_environment_modules_share_io_errors() {
    let source = r#"
import core.result
import std.io
import std.path as paths
import std.env as environment

func text(outcome: Result<String, IoError>) -> String {
    branch outcome {
        Result.Error(error) -> error.message
        Result.Ok(value) -> value
    }
}

func main() -> String {
    cwd = text(environment.current_directory())
    text(paths.canonicalize(paths.join(cwd, ".")))
}
"#;

    let expected = std::env::current_dir().unwrap().canonicalize().unwrap();
    assert_string(
        foster::run(source).unwrap(),
        expected.to_string_lossy().as_ref(),
    );
}

#[test]
fn core_tcp_accepts_reads_and_writes_a_connection() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut connection, _) = listener.accept().unwrap();
        let mut request = [0; 4];
        connection.read_exact(&mut request).unwrap();
        connection.write_all(b"pong").unwrap();
        request
    });

    let source = format!(
        r#"
import std.net.tcp
import core.result

func start(outcome: Result<Connection, NetworkError>) -> String {{
    branch outcome {{
        Result.Error(error) -> error.message
        Result.Ok(connection) -> send(connection, tcp.write_text(connection, "ping"))
    }}
}}

func send(connection: Connection, outcome: Result<Unit, NetworkError>) -> String {{
    branch outcome {{
        Result.Error(error) -> error.message
        Result.Ok(_) -> receive(connection, tcp.read_text(connection, 64))
    }}
}}

func receive(connection: Connection, outcome: Result<String, NetworkError>) -> String {{
    branch outcome {{
        Result.Error(error) -> error.message
        Result.Ok(text) -> finish(connection, move text)
    }}
}}

func finish(connection: Connection, text: String) -> String {{
    tcp.close_connection(connection)
    text
}}

func main() -> String {{
    start(tcp.connect("127.0.0.1", {port}))
}}
"#
    );
    assert_string(foster::run(&source).unwrap(), "pong");
    assert_eq!(&server.join().unwrap(), b"ping");
}
