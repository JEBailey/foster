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
    assert_eq!(checked, 536);

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
fn toml_parses_nested_values_and_reports_source_positions() {
    let value = foster::run(
        r#"
import core.option
import core.result
import std.toml

func enabled(document: TomlDocument) -> Bool [consume document] {
    branch (move document).get("package") {
        Option.None -> false
        Option.Some(value) -> branch (move value).get("enabled") {
            Option.Some(TomlValue.Bool(enabled)) -> enabled
            _ -> false
        }
    }
}

func error_line() -> Int {
    branch parse("title = \"ok\"\nbroken = [\n") {
        Result.Ok(_) -> 0
        Result.Error(error) -> error.line
    }
}

func main() -> Int {
    branch parse("title = \"Foster\"\n[package]\nenabled = true\n") {
        Result.Error(_) -> 0
        Result.Ok(document) -> branch {
            enabled(move document) -> error_line()
            _ -> 0
        }
    }
}
"#,
    )
    .unwrap();
    assert_eq!(value, Value::Integer(2));
}

#[test]
fn toml_renders_typed_documents() {
    let value = foster::run(
        r#"
import core.result
import std.toml

func main() -> String {
    let document = TomlDocument {
        entries: [TomlEntry { key: "count", value: TomlValue.Int(3) }, TomlEntry { key: "title", value: TomlValue.String("Foster") }]
    }
    branch (move document).render() {
        Result.Ok(source) -> source
        Result.Error(error) -> error.message
    }
}
"#,
    )
    .unwrap();
    assert_string(value, "count = 3\ntitle = \"Foster\"\n");
}

#[test]
fn toml_preserves_every_value_category() {
    let value = foster::run(
        r#"
import core.result
import std.toml

func score_value(value: TomlValue) -> Int {
    branch value {
        TomlValue.String(_) -> 1
        TomlValue.Int(_) -> 2
        TomlValue.Float(_) -> 4
        TomlValue.Bool(_) -> 8
        TomlValue.DateTime(_) -> 16
        TomlValue.Array(values) -> 32 + values.length
        TomlValue.Table(entries) -> 64 + score_entries(move entries)
    }
}

func score_entries(entries: List<TomlEntry>) -> Int {
    return 0 if entries.empty?
    score_value(entries.head.value) + score_entries(entries.rest)
}

func main() -> Int {
    let source = "array = [1, 2]\nboolean = true\ndatetime = 1979-05-27T07:32:00Z\nfloat = 1.5\ninteger = 3\nstring = \"x\"\n[table]\nnested = false\n"
    branch parse(move source) {
        Result.Error(_) -> 0
        Result.Ok(document) -> branch (move document).render() {
            Result.Error(_) -> 0
            Result.Ok(rendered) -> branch parse(move rendered) {
                Result.Error(_) -> 0
                Result.Ok(round_trip) -> score_entries(move round_trip.entries)
            }
        }
    }
}
"#,
    )
    .unwrap();
    assert_eq!(value, Value::Integer(137));
}

#[test]
fn toml_render_rejects_invalid_date_time_values() {
    let value = foster::run(
        r#"
import core.result
import std.toml

func main() -> Int {
    let document = TomlDocument { entries: [TomlEntry { key: "when", value: TomlValue.DateTime("not-a-date") }] }
    branch (move document).render() {
        Result.Ok(_) -> -1
        Result.Error(error) -> error.line + error.column
    }
}
"#,
    )
    .unwrap();
    assert_eq!(value, Value::Integer(0));
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
fn foster_toml_parser_reports_common_error_conditions() {
    let cases = [
        "value = 1\nvalue = 2\n",
        "value = 1__0\n",
        "value = +0x10\n",
        "value = 01.2\n",
        "value = 2025-02-30\n",
        "value = \"\\q\"\n",
        "[table]\n[table]\n",
        "value = 1\n[value.child]\n",
        "value = { key = 1, key = 2 }\n",
        "value = [1, 2\n",
    ];
    let checks = cases
        .iter()
        .map(|input| format!("score(parse({input:?}))"))
        .collect::<Vec<_>>()
        .join(" + ");
    let source = format!(
        r#"
import core.result
import std.toml

func score(outcome: Result<TomlDocument, TomlError>) -> Int {{
    branch outcome {{
        Result.Ok(_) -> 0
        Result.Error(error) -> branch {{
            error.message.empty? -> 0
            error.line <= 0 -> 0
            error.column <= 0 -> 0
            _ -> 1
        }}
    }}
}}

func main() -> Int {{ {checks} }}
"#
    );
    assert_eq!(foster::run(&source).unwrap(), Value::Integer(10));
}

#[test]
fn foster_written_string_and_integer_algorithms_execute() {
    let source = r#"
import core.int
import core.string

func main() -> String {
    let parts = "one,two,three".split(",")
    (parts.join("-") + ":" + (-42).as_string()).upper()
}
"#;
    assert_string(foster::run(source).unwrap(), "ONE-TWO-THREE:-42");
}

#[test]
fn string_take_while_accepts_predicates_and_preserves_unicode_boundaries() {
    let source = r#"
import core.functions
import core.string

func ascii_digit?(value: CodePoint) -> Bool {
    branch {
        value < '0' -> false
        value > '9' -> false
        _ -> true
    }
}

func main() -> String {
    "123λ45".take_while(ascii_digit?)
}
"#;
    assert_string(foster::run(source).unwrap(), "123");
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
    let values = Map.empty()
    values = (move values).put("answer", 42)
    option_value((move values).get("answer"))
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));
}

#[test]
fn option_and_result_support_queries_fallbacks_recovery_and_flattening() {
    let source = r#"
import core.option as option
import core.result as result

func option_fallback() -> Int { 20 }
func option_recovery() -> Option<Int> { Option.Some(3) }
func no_int() -> Option<Int> { Option.None }
func nested_option() -> Option<Option<Int>> { Option.Some(Option.Some(2)) }
func result_fallback(error: String) -> Int { error.length }
func result_recovery(error: String) -> Result<Int, Int> { Result.Ok(error.length) }
func failed_int(message: String) -> Result<Int, String> { Result.Error(message) }
func nested_result() -> Result<Result<Int, String>, String> { Result.Ok(Result.Ok(5)) }

func main() -> Int {
    let a = no_int().unwrap_or_else(option_fallback)
    let b = nested_option().flatten().unwrap_or(0)
    let c = no_int().or_else(option_recovery).unwrap_or(0)
    let d = failed_int("four").unwrap_or_else(result_fallback)
    let e = nested_result().flatten().unwrap_or(0)
    let f = failed_int("six").or_else(result_recovery).unwrap_or(0)
    let absent = no_int().absent?()
    let failed = failed_int("failure").error?()
    branch {
        absent -> branch {
            failed -> a + b + c + d + e + f
            _ -> 0
        }
        _ -> 0
    }
}
"#;

    assert_eq!(foster::run(source).unwrap(), Value::Integer(37));
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
        Result.Ok(connection) -> write(move connection)
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
    start(tcp::connect("127.0.0.1", {port}))
}}
"#
    );
    assert_string(foster::run(&source).unwrap(), "pong");
    assert_eq!(&server.join().unwrap(), b"ping");
}
