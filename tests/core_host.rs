use std::io::{Read, Write};
use std::net::TcpListener;

use foster::vm::Value;

fn assert_string(value: Value, expected: &str) {
    assert_eq!(value.as_string(), Some(expected));
}

#[test]
fn standard_time_clocks_cross_the_host_boundary_with_canonical_readings() {
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let value = foster::run(
        r#"
import std.time

func clock_read<T>(clock: Clock<T>) -> T { clock.now() }

func main() -> List<Int> {
    let wall = clock_read(SystemClock.new())
    let first = clock_read(ContinuousClock.new())
    let second = clock_read(ContinuousClock.new())
    [wall.epoch_seconds(), wall.nanosecond(), first.until(second).total_nanoseconds()]
}
"#,
    )
    .unwrap();
    let after = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let values = value.as_list().unwrap();
    let [
        Value::Integer(seconds),
        Value::Integer(nanosecond),
        Value::Integer(elapsed),
    ] = values
    else {
        panic!("time clocks returned an unexpected representation: {values:?}");
    };
    assert!((*seconds >= before) && (*seconds <= after));
    assert!((0..1_000_000_000).contains(nanosecond));
    assert!(*elapsed >= 0);
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
    assert_eq!(checked, 803);

    let mut modules = 0;
    let mut types = 0;
    let mut required_methods = 0;
    for (_, module) in compilation.hir.modules.iter() {
        if !(module.name.starts_with("core.") || module.name.starts_with("std."))
            || module.source_path.is_none()
        {
            continue;
        }
        modules += 1;
        assert!(
            module
                .documentation
                .as_deref()
                .is_some_and(|documentation| !documentation.trim().is_empty()),
            "{} is missing module documentation",
            module.name
        );
        for record_id in module.records.values() {
            let record = &compilation.hir.records[*record_id];
            if !record.public {
                continue;
            }
            types += 1;
            assert!(
                record
                    .documentation
                    .as_deref()
                    .is_some_and(|documentation| !documentation.trim().is_empty()),
                "{}.{} is missing type documentation",
                module.name,
                record.name
            );
            for method in &record.methods {
                required_methods += 1;
                assert!(
                    method
                        .documentation
                        .as_deref()
                        .is_some_and(|documentation| !documentation.trim().is_empty()),
                    "{}.{}.{} is missing required-method documentation",
                    module.name,
                    record.name,
                    method.name
                );
            }
        }
        for variant_id in module.variant_types.values() {
            let variant = &compilation.hir.variant_types[*variant_id];
            if !variant.public {
                continue;
            }
            types += 1;
            assert!(
                variant
                    .documentation
                    .as_deref()
                    .is_some_and(|documentation| !documentation.trim().is_empty()),
                "{}.{} is missing variant documentation",
                module.name,
                variant.name
            );
            for method in &variant.methods {
                required_methods += 1;
                assert!(
                    method
                        .documentation
                        .as_deref()
                        .is_some_and(|documentation| !documentation.trim().is_empty()),
                    "{}.{}.{} is missing required-method documentation",
                    module.name,
                    variant.name,
                    method.name
                );
            }
        }
    }
    assert!(modules >= 30);
    assert!(types >= 30);
    assert!(required_methods >= 10);
}

#[test]
fn foster_toml_parser_handles_toml_1_1_structures() {
    let document = r#"
title = "TOML \x31\e"
multiline = """
one
two"""
hex = 0xDEAD_BEEF
octal = 0o755
binary = 0b1101
scientific = 6.626e-34
minimum = -9223372036854775808
positive_inf = +inf
negative_nan = -nan
partial_time = 07:32
local_date = 1979-05-27
inline = { first.name = "Tom", values = [1, true, { x = 2 }], }

[[products]]
name = "Hammer"

[[products]]
name = "Nail"

[products.details]
weight = 1.0
"#;
    let source = format!(
        r#"
import core.option
import core.result
import std.toml

func second_product_has_details(document: TomlDocument) -> Bool [consume document] {{
    branch (move document).get("products") {{
        Option.Some(TomlValue.Array(products)) -> branch {{
            products.length != 2 -> false
            _ -> branch products[1] {{
                TomlValue.Table(entries) -> branch find_details(move entries) {{
                    Option.Some(TomlValue.Table(details)) -> details.length == 1
                    _ -> false
                }}
                _ -> false
            }}
        }}
        _ -> false
    }}
}}

func find_details(entries: List<TomlEntry>) -> Option<TomlValue> {{
    return Option.None if entries.empty?
    return Option.Some(entries.head.value) if entries.head.key == "details"
    find_details(entries.rest)
}}

func main() -> Int {{
    branch parse({document:?}) {{
        Result.Error(_) -> 0
        Result.Ok(parsed) -> branch (move parsed).render() {{
            Result.Error(_) -> 1
            Result.Ok(rendered) -> branch parse(move rendered) {{
                Result.Error(_) -> 2
                Result.Ok(round_trip) -> branch {{
                    second_product_has_details(move round_trip) -> 42
                    _ -> 3
                }}
            }}
        }}
    }}
}}
"#
    );
    assert_eq!(foster::run(&source).unwrap(), Value::Integer(42));
}

#[test]
fn filesystem_mutation_operations_create_copy_move_and_remove_entries() {
    let root = std::env::temp_dir().join(format!(
        "foster-core-fs-mutation-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    let nested = root.join("parent").join("child");
    let source_path = nested.join("source.txt");
    let copied_path = nested.join("copied.txt");
    let moved_path = nested.join("moved.txt");
    let root_literal = serde_json::to_string(&root.to_string_lossy()).unwrap();
    let nested_literal = serde_json::to_string(&nested.to_string_lossy()).unwrap();
    let source_literal = serde_json::to_string(&source_path.to_string_lossy()).unwrap();
    let copied_literal = serde_json::to_string(&copied_path.to_string_lossy()).unwrap();
    let moved_literal = serde_json::to_string(&moved_path.to_string_lossy()).unwrap();
    let source = format!(
        r#"
import core.result
import std.fs
import std.io

func unit(outcome: Result<(), IoError>) -> () {{
    let succeeded = branch outcome {{
        Result.Ok(_) -> true
        Result.Error(_) -> false
    }}
    assert(succeeded)
}}
func count(outcome: Result<Int, IoError>) -> Int {{
    branch outcome {{
        Result.Ok(value) -> value
        Result.Error(_) -> 0
    }}
}}
func main() -> Int {{
    unit(create_directory_all({nested_literal}))
    unit(write_text({source_literal}, "hello"))
    let copied = count(copy_file({source_literal}, {copied_literal}))
    unit(rename({copied_literal}, {moved_literal}))
    unit(remove_file({source_literal}))
    unit(remove_file({moved_literal}))
    unit(remove_directory({nested_literal}))
    unit(remove_directory({root_literal} + "/parent"))
    unit(remove_directory({root_literal}))
    copied
}}
"#
    );

    assert_eq!(foster::run(&source).unwrap(), Value::Integer(5));
    assert!(!root.exists());
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

func read_after_write(path: String, outcome: Result<(), IoError>) -> String {{
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
    let path = {path_literal}
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

func read_after_write(path: String, outcome: Result<(), IoError>) -> String {{
    branch outcome {{
        Result.Error(error) -> error.message
        Result.Ok(_) -> render(read_bytes(path))
    }}
}}

func render(outcome: Result<Bytes, IoError>) -> String {{
    branch outcome {{
        Result.Error(error) -> error.message
        Result.Ok(contents) -> contents.hex()
    }}
}}

func main() -> String {{
    let path = {path_literal}
    let contents = bytes_or_empty(Bytes.from_hex("00ff8041"))
    read_after_write(path, write_bytes(path, contents))
}}
"#
    );

    assert_string(foster::run(&source).unwrap(), "00ff8041");
    assert_eq!(std::fs::read(&path).unwrap(), [0, 255, 128, 65]);
    std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn filesystem_positioned_reads_and_appends_stream_binary_chunks() {
    let directory = std::env::temp_dir().join(format!(
        "foster-core-streaming-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("payload.bin");
    let path_literal = serde_json::to_string(&path.to_string_lossy()).unwrap();
    let source = format!(
        r#"
import core.bytes
import core.result
import std.fs
import std.io

func decoded(value: String) -> Bytes {{
    branch Bytes.from_hex(value) {{
        Result.Ok(contents) -> contents
        Result.Error(_) -> Bytes.empty()
    }}
}}

func count(outcome: Result<Int, IoError>) -> Int {{
    branch outcome {{
        Result.Ok(value) -> value
        Result.Error(_) -> -1
    }}
}}

func chunk(outcome: Result<Bytes, IoError>) -> String {{
    branch outcome {{
        Result.Ok(value) -> value.hex()
        Result.Error(error) -> error.message
    }}
}}

func main() -> String {{
    let file = File.from({path_literal})
    let replaced = count(file.write(decoded("000102")))
    let appended = count(file.append(decoded("03040506")))
    let length = count(file.length())
    replaced.as_string() + ":" + length.as_string() + ":" + appended.as_string() + ":" + chunk(file.read_at(2, 3))
}}
"#
    );

    assert_string(foster::run(&source).unwrap(), "3:7:4:020304");
    assert_eq!(std::fs::read(&path).unwrap(), [0, 1, 2, 3, 4, 5, 6]);
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
    let cwd = text(environment::current_directory())
    text(paths::canonicalize(paths::join(cwd, ".")))
}
"#;

    let expected = std::env::current_dir().unwrap().canonicalize().unwrap();
    assert_string(
        foster::run(source).unwrap(),
        expected.to_string_lossy().as_ref(),
    );
}

#[test]
fn typed_files_implement_location_based_resource_capabilities() {
    let directory = std::env::temp_dir().join(format!(
        "foster-resource-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("resource.bin");
    let path_literal = serde_json::to_string(&path.to_string_lossy()).unwrap();
    let source = format!(
        r#"
import core.bytes
import core.int
import core.result
import std.fs
import std.io
import std.path as paths
import std.resource

func identifier_text(value: ResourceIdentifier) -> String {{ value.resource_id() }}

func resource_location_text(value: Resource<paths::Path>) -> String {{ value.location.as_string() }}

func require_read_write(value: ReadWrite<IoError>) {{ () }}

func require_streaming(positioned: PositionedReadable<IoError>, appendable: Appendable<IoError>, sized: Sized<IoError>) {{
    ()
}}

func replace(value: Writable<IoError>, contents: Bytes) -> Result<Int, IoError> [mut value] {{
    value.write(contents)
}}

func load(value: Readable<IoError>) -> Result<Bytes, IoError> [mut value] {{
    value.read()
}}

func render(file: File, outcome: Result<Int, IoError>) -> String [mut file, consume outcome] {{
    require_read_write(file)
    require_streaming(file, file, file)
    branch move outcome {{
        Result.Error(error) -> error.message
        Result.Ok(written) -> branch load(file) {{
            Result.Error(error) -> error.message
            Result.Ok(contents) -> identifier_text(file.location) + ":" + resource_location_text(file) + ":" + written.as_string() + ":" + contents.hex()
        }}
    }}
}}

func main() -> String {{
    let path = paths::Path.from({path_literal})
    let file = File.at(move path)
    let contents = branch Bytes.from_hex("00ff41") {{
        Result.Error(_) -> Bytes.empty()
        Result.Ok(value) -> value
    }}
    render(file, replace(file, contents))
}}
"#
    );

    let expected = format!(
        "{}:{}:3:00ff41",
        path.to_string_lossy(),
        path.to_string_lossy()
    );
    assert_string(foster::run(&source).unwrap(), &expected);
    assert_eq!(std::fs::read(&path).unwrap(), [0, 255, 65]);
    std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn uri_is_a_parsed_resource_identifier_without_implicit_io() {
    let source = r#"
import core.result
import std.resource
import std.uri

func identifier_text(value: ResourceIdentifier) -> String { value.resource_id() }

func render(outcome: Result<Uri, UriError>) -> String [consume outcome] {
    branch move outcome {
        Result.Error(error) -> error.message
        Result.Ok(value) -> identifier_text(value)
    }
}

func main() -> String {
    render(Uri.parse("https://example.com/assets/data.bin"))
}
"#;

    assert_string(
        foster::run(source).unwrap(),
        "https://example.com/assets/data.bin",
    );

    let invalid = source.replace(
        "https://example.com/assets/data.bin",
        "1https://example.com",
    );
    assert_string(
        foster::run(&invalid).unwrap(),
        "URI scheme must begin with an ASCII letter",
    );
}

#[test]
fn resource_identifiers_preserve_provider_kinds_and_reject_accidental_matches() {
    let uri_as_file = r#"
import core.result
import std.fs
import std.uri

func open(outcome: Result<Uri, UriError>) [consume outcome] {
    branch move outcome {
        Result.Error(_) -> ()
        Result.Ok(value) -> {
            File.at(move value)
            ()
        }
    }
}

func main() { open(Uri.parse("https://example.com/artifact")) }
"#;
    let error = foster::compile(uri_as_file).unwrap_err();
    assert!(
        error.message.contains("Path") || error.message.contains("File.at"),
        "{}",
        error.message
    );

    let displayable_integer = r#"
import std.resource

func identify(value: ResourceIdentifier) -> String { value.resource_id() }
func main() -> String { identify(42) }
"#;
    let error = foster::compile(displayable_integer).unwrap_err();
    assert!(
        error.message.contains("ResourceIdentifier") || error.message.contains("resource_id"),
        "{}",
        error.message
    );
}

#[test]
fn tcp_endpoints_are_uri_shaped_resource_identifiers() {
    let source = r#"
import core.result
import std.net.tcp
import std.resource
import std.uri

func identifier_text(value: ResourceIdentifier) -> String { value.resource_id() }

func endpoint_text(outcome: Result<TcpEndpoint, NetworkError>) -> String [consume outcome] {
    branch move outcome {
        Result.Error(error) -> error.message
        Result.Ok(endpoint) -> identifier_text(endpoint)
    }
}

func parse_endpoint(outcome: Result<Uri, UriError>) -> String [consume outcome] {
    branch move outcome {
        Result.Error(error) -> error.message
        Result.Ok(location) -> endpoint_text(TcpEndpoint.from_uri(move location))
    }
}

func main() -> String {
    let dns = parse_endpoint(Uri.parse("tcp://example.com:443"))
    let ipv6 = parse_endpoint(Uri.parse("tcp://[::1]:8080"))
    let scheme = parse_endpoint(Uri.parse("http://example.com:80"))
    let port = parse_endpoint(Uri.parse("tcp://example.com:65536"))
    dns + "|" + ipv6 + "|" + scheme + "|" + port
}
"#;

    assert_string(
        foster::run(source).unwrap(),
        "tcp://example.com:443|tcp://[::1]:8080|http://example.com:80: URI scheme must be tcp|tcp://example.com:65536: TCP URI port must be a decimal integer from 0 through 65535",
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
import std.resource
import std.uri

func open(outcome: Result<Uri, UriError>) -> String [consume outcome] {{
    branch move outcome {{
        Result.Error(error) -> error.message
        Result.Ok(location) -> start(tcp::connect_uri(move location))
    }}
}}

func connection_location(value: Resource<TcpEndpoint>) -> String {{
    value.location.as_string()
}}

func require_closeable(value: Closable<NetworkError>) {{ () }}

func start(outcome: Result<Connection, NetworkError>) -> String {{
    branch outcome {{
        Result.Error(error) -> error.message
        Result.Ok(connection) -> {{
            require_closeable(connection)
            connection_location(connection) + ":" + write(move connection)
        }}
    }}
}}

func write(connection: Connection) -> String [consume connection] {{
    let outcome = connection.write_text("ping")
    send(move connection, move outcome)
}}

func send(connection: Connection, outcome: Result<(), NetworkError>) -> String [consume connection, consume outcome] {{
    branch outcome {{
        Result.Error(error) -> error.message
        Result.Ok(_) -> read_connection(move connection)
    }}
}}

func read_connection(connection: Connection) -> String [consume connection] {{
    let outcome = connection.read_text(64)
    receive(move connection, move outcome)
}}

func receive(connection: Connection, outcome: Result<String, NetworkError>) -> String [consume connection, consume outcome] {{
    branch outcome {{
        Result.Error(error) -> error.message
        Result.Ok(text) -> finish(move connection, move text)
    }}
}}

func finish(connection: Connection, text: String) -> String [consume connection, consume text] {{
    (move connection).close()
    text
}}

func main() -> String {{
    open(Uri.parse("tcp://127.0.0.1:{port}"))
}}
"#
    );
    assert_string(
        foster::run(&source).unwrap(),
        &format!("tcp://127.0.0.1:{port}:pong"),
    );
    assert_eq!(&server.join().unwrap(), b"ping");
}
