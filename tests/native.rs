use foster::native::{CompileOptions, NativeType, build_executable, compile_object};

#[test]
fn compiles_reachable_primitive_functions_to_an_object() {
    let compilation = foster::compile(
        r#"
func unused_text() -> String { "the native reachability pass must ignore this" }

func choose(early: Bool, left: Float, right: Float) -> Float {
    return left if early
    right
}

func main() -> Float {
    let count = 0
    loop {
        count = count + 1
        continue if count < 2
        break
    }
    assert(2 < 3, "native assertion should pass")
    choose(true, 40.0 + 2.5, 0.0)
}
"#,
    )
    .unwrap();
    let artifact = compile_object(&compilation, CompileOptions::default()).unwrap();
    assert!(!artifact.bytes.is_empty());
    assert!(
        artifact
            .bytes
            .windows(b"foster_layout_0".len())
            .any(|bytes| bytes == b"foster_layout_0"),
        "native object should retain physical layout descriptors"
    );
    assert_eq!(artifact.result, NativeType::Float);
}

#[test]
fn compiles_lossless_integer_widening_to_an_int_result() {
    let compilation = foster::compile("func main() -> Int { 'A' }").unwrap();
    let artifact = compile_object(&compilation, CompileOptions::default()).unwrap();
    assert!(!artifact.bytes.is_empty());
    assert_eq!(artifact.result, NativeType::Int);
}

#[test]
fn compiles_symbol_literals_with_the_immutable_text_abi() {
    let compilation = foster::compile(r#"func main() -> Symbol { :hello }"#).unwrap();
    let artifact = compile_object(&compilation, CompileOptions::default()).unwrap();
    assert!(!artifact.bytes.is_empty());
    assert_eq!(artifact.result, NativeType::String);

    let executable = std::env::temp_dir().join(format!(
        "foster-native-symbol-test-{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    build_executable(&compilation, &executable, CompileOptions::default()).unwrap();
    let output = std::process::Command::new(&executable).output().unwrap();
    let _ = std::fs::remove_file(&executable);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "hello");
}

#[test]
fn compiles_the_command_arguments_entry_abi() {
    let compilation = foster::compile(include_str!("fixtures/programs/arguments.fos")).unwrap();
    let artifact = compile_object(&compilation, CompileOptions::default()).unwrap();
    assert!(!artifact.bytes.is_empty());
    assert_eq!(artifact.result, NativeType::String);
    assert!(artifact.accepts_arguments);
}

#[test]
fn lowers_records_and_copy_on_write_field_updates_to_cranelift() {
    let compilation = foster::compile(
        r#"
type Inner = { label: String, value: Int }
type Pair = { left: Inner, right: Inner }

func inner_value(inner: Inner) -> Int { inner.value }

func main() -> Int {
    let pair = Pair {
        left: Inner { label: "ok", value: 18 }
        right: Inner { label: "", value: 0 }
    }
    pair.right = Inner { label: "", value: 22 }
    inner_value(pair.left) + inner_value(pair.right) + pair.left.label.length
}
"#,
    )
    .unwrap();
    let artifact = compile_object(&compilation, CompileOptions::default()).unwrap();
    assert!(!artifact.bytes.is_empty());
    assert_eq!(artifact.result, NativeType::Int);

    let executable = std::env::temp_dir().join(format!(
        "foster-native-record-test-{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    build_executable(&compilation, &executable, CompileOptions::default()).unwrap();
    let output = std::process::Command::new(&executable).output().unwrap();
    let _ = std::fs::remove_file(&executable);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "42");
}

#[test]
fn lowers_native_reference_captures() {
    let compilation = foster::compile(
        r#"
func main() -> Int {
    let value = 39
    let add = [ref value] (amount: Int) -> {
        value = value + amount
        value
    }
    assert(add(1) == 40)
    let direct = 40
    assert(increment(ref direct, 2) == 42)
    let values = [40]
    assert(increment(ref values[0], 2) == 42)
    let boxed = Outer { inner: Boxed { value: 40 } }
    increment(ref boxed.inner.value, 2)
}

type Boxed<T> = { value: T }
type Outer<T> = { inner: Boxed<T> }

func increment[g: group Int](value: ref[g] Int, amount: Int) -> Int [mut g] {
    value = value + amount
    value
}
"#,
    )
    .unwrap();
    let executable = std::env::temp_dir().join(format!(
        "foster-native-reference-test-{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    build_executable(&compilation, &executable, CompileOptions::default()).unwrap();
    let output = std::process::Command::new(&executable).output().unwrap();
    let _ = std::fs::remove_file(&executable);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "42");
}

#[test]
fn lowers_variant_tags_payloads_and_pattern_bindings_to_cranelift() {
    let compilation = foster::compile(
        r#"
type Boxed = { value: Int }
enum Choice = Number(Boxed) | Empty

func main() -> Int {
    let choice = Number(Boxed { value: 42 })
    branch choice {
        Number(boxed) -> boxed.value
        Empty -> 0
    }
}
"#,
    )
    .unwrap();
    let artifact = compile_object(&compilation, CompileOptions::default()).unwrap();
    assert!(!artifact.bytes.is_empty());

    let executable = std::env::temp_dir().join(format!(
        "foster-native-variant-test-{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    build_executable(&compilation, &executable, CompileOptions::default()).unwrap();
    let output = std::process::Command::new(&executable).output().unwrap();
    let _ = std::fs::remove_file(&executable);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "42");
}

#[test]
fn monomorphizes_generic_functions_and_record_layouts() {
    let compilation = foster::compile(
        r#"
type Boxed<T> = { value: T }
enum Maybe<T> = Some(T) | None

func identity<T>(value: T) -> T [consume value] { value }
func boxed<T>(value: T) -> Boxed<T> [consume value] { Boxed { value } }
func Boxed.unbox<T>(self: Boxed<T>) -> T [consume self] { self.value }
func value_or<T>(value: Maybe<T>, fallback: T) -> T [consume value, consume fallback] {
    branch value {
        Some(found) -> found
        None -> fallback
    }
}

func main() -> Int {
    assert(identity(1.5) == 1.5)
    boxed(identity(40)).unbox() + value_or(Some(2), 0)
}
"#,
    )
    .unwrap();
    let executable = std::env::temp_dir().join(format!(
        "foster-native-generic-test-{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    build_executable(&compilation, &executable, CompileOptions::default()).unwrap();
    let output = std::process::Command::new(&executable).output().unwrap();
    let _ = std::fs::remove_file(&executable);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "42");
}

#[test]
fn specializes_closure_environments_and_capture_destructors() {
    let compilation = foster::compile(
        r#"
type Offset = { value: Int }

func make_offset<T>(marker: T, offset: Offset) [consume marker, consume offset] {
    [move marker, move offset] (value: Int) -> {
        marker
        offset.value + value
    }
}

func main() -> Int {
    let add = make_offset(0, Offset { value: 40 })
    add(2)
}
"#,
    )
    .unwrap();
    let executable = std::env::temp_dir().join(format!(
        "foster-native-closure-test-{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    build_executable(&compilation, &executable, CompileOptions::default()).unwrap();
    let output = std::process::Command::new(&executable).output().unwrap();
    let _ = std::fs::remove_file(&executable);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "42");
}

#[test]
fn lowers_erased_callable_parameters_through_uniform_thunks() {
    let compilation = foster::compile(
        r#"
func apply(value: Int, operation: func(Int) -> Int) -> Int {
    operation(value)
}

func main() -> Int {
    let offset = 40
    let add = [copy offset] (value: Int) -> offset + value
    let double = (value: Int) -> value * 2
    assert(apply(21, double) == 42)
    apply(2, add)
}
"#,
    )
    .unwrap();
    let executable = std::env::temp_dir().join(format!(
        "foster-native-callable-test-{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    build_executable(&compilation, &executable, CompileOptions::default()).unwrap();
    let output = std::process::Command::new(&executable).output().unwrap();
    let _ = std::fs::remove_file(&executable);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "42");
}

#[test]
fn lowers_erased_union_arguments_through_owned_boxes() {
    let compilation = foster::compile(
        r#"
type Scalar = String | Int

func count(value: Scalar) -> Int { 1 }

func main() -> Int {
    count("Foster") + count(42)
}
"#,
    )
    .unwrap();
    let executable = std::env::temp_dir().join(format!(
        "foster-native-erased-test-{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    build_executable(&compilation, &executable, CompileOptions::default()).unwrap();
    let output = std::process::Command::new(&executable).output().unwrap();
    let _ = std::fs::remove_file(&executable);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "2");
}

#[test]
fn lowers_native_lists_and_core_list_algorithms() {
    let compilation = foster::compile(
        r#"
import core.list
import core.option
import core.float
import core.byte

func main() -> Int {
    let values = [10, 0]
    values[1] = 20
    values.push(12)
    let copied = values.append(100)
    assert(values.length == 3)
    assert(copied.length == 4)
    assert(from_code_point(65) == 'A')
    assert(parse_float("42.5") == 42.5)
    assert(42.5.as_string() == "42.5")
    assert(Byte.valid(255))
    assert(!Byte.valid(256))
    assert(Byte.unchecked(42) == Byte.unchecked(42))
    let add = (total: Int, value: Int) -> total + value
    assert(values.fold(0, add) == 42)
    branch values.last() {
        Option.Some(value) -> value + 30
        Option.None -> 0
    }
}

"#,
    )
    .unwrap();
    let executable = std::env::temp_dir().join(format!(
        "foster-native-list-test-{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    build_executable(&compilation, &executable, CompileOptions::default()).unwrap();
    let output = std::process::Command::new(&executable).output().unwrap();
    let _ = std::fs::remove_file(&executable);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "42");
}

#[test]
fn lowers_string_algorithms_and_descriptor_backed_bytes() {
    let compilation = foster::compile(
        r#"
import core.byte
import core.bytes
import core.result

func main() -> String {
    let encoded = "Foster λ".utf8
    assert(encoded.length == 9)
    assert(encoded.head == Byte.unchecked(70))
    assert(encoded.equal?("Foster λ".utf8))
    assert(!encoded.equal?("Foster".utf8))
    let decoded = branch String.from_utf8(encoded) {
        Result.Ok(value) -> value
        Result.Error(_) -> "invalid"
    }
    decoded + "!"
}

"#,
    )
    .unwrap();
    let executable = std::env::temp_dir().join(format!(
        "foster-native-bytes-test-{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    build_executable(&compilation, &executable, CompileOptions::default()).unwrap();
    let output = std::process::Command::new(&executable).output().unwrap();
    let _ = std::fs::remove_file(&executable);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "Foster λ!");
}

#[test]
fn lowers_foster_written_byte_buffers_over_native_lists() {
    let compilation = foster::compile(
        r#"
import core.byte
import core.bytes
import core.bytes.buffer

func main() -> String {
    let output = ByteBuffer.empty()
    output.push(Byte.unchecked(111))
    output.extend("k".utf8)
    output.snapshot().hex()
}
"#,
    )
    .unwrap();
    let executable = std::env::temp_dir().join(format!(
        "foster-native-byte-buffer-test-{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    build_executable(&compilation, &executable, CompileOptions::default()).unwrap();
    let output = std::process::Command::new(&executable).output().unwrap();
    let _ = std::fs::remove_file(&executable);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "6f6b");
}

#[test]
fn dispatches_contract_methods_with_arguments() {
    let compilation = foster::compile(
        r#"
type Identified = {
    pub func id(self) -> Int [read self]
    pub func offset(self, amount: Int) -> Int [read self]
}

type User = & Identified & { value: Int }
type Device = & Identified & { value: Int }

func User.id(self: User) -> Int { self.value }
func User.offset(self: User, amount: Int) -> Int { self.value + amount }
func Device.id(self: Device) -> Int { self.value }
func Device.offset(self: Device, amount: Int) -> Int { self.value + amount }

func increment_id(value: Identified) -> Int {
    value.id() + value.offset(2)
}

func main() -> Int {
    assert(increment_id(User { value: 20 }) == 42)
    increment_id(Device { value: 20 })
}
"#,
    )
    .unwrap();
    let executable = std::env::temp_dir().join(format!(
        "foster-native-contract-test-{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    build_executable(&compilation, &executable, CompileOptions::default()).unwrap();
    let output = std::process::Command::new(&executable).output().unwrap();
    let _ = std::fs::remove_file(&executable);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "42");
}

#[test]
fn dispatches_generic_contract_implementations() {
    let compilation = foster::compile(
        r#"
type Renderer<T> = {
    pub func render(self, value: T) -> String [read self]
}

type Formatter<T> = & Renderer<T> & {}

func Formatter.render<T>(self: Formatter<T>, value: T) -> String {
    "generic"
}

func render(value: Renderer<Int>) -> String { value.render(42) }
func main() -> String { render(Formatter {}) }
"#,
    )
    .unwrap();
    let executable = std::env::temp_dir().join(format!(
        "foster-native-generic-contract-test-{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    build_executable(&compilation, &executable, CompileOptions::default()).unwrap();
    let output = std::process::Command::new(&executable).output().unwrap();
    let _ = std::fs::remove_file(&executable);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "generic");
}

#[test]
fn dispatches_contracts_to_tagged_variants() {
    let compilation = foster::compile(
        r#"
type Scored = { pub func score(self) -> Int [read self] }

enum Choice = Number(Int)
    | Empty
    & Scored

func Choice.score(self: Choice) -> Int {
    branch self {
        Choice.Number(value) -> value
        Choice.Empty -> 0
    }
}

func score_of(value: Scored) -> Int { value.score() }
func main() -> Int { score_of(Choice.Number(42)) }
"#,
    )
    .unwrap();
    let executable = std::env::temp_dir().join(format!(
        "foster-native-variant-contract-test-{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    build_executable(&compilation, &executable, CompileOptions::default()).unwrap();
    let output = std::process::Command::new(&executable).output().unwrap();
    let _ = std::fs::remove_file(&executable);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "42");
}

#[test]
fn formats_and_releases_descriptor_backed_main_results() {
    let compilation = foster::compile(
        r#"
enum Choice = Number(Int) | Empty
type Summary = { choice: Choice, values: List<Int> }
func main() -> Summary {
    Summary { choice: Choice.Number(42), values: [20, 22] }
}
"#,
    )
    .unwrap();
    let executable = std::env::temp_dir().join(format!(
        "foster-native-format-test-{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    build_executable(&compilation, &executable, CompileOptions::default()).unwrap();
    let output = std::process::Command::new(&executable).output().unwrap();
    let _ = std::fs::remove_file(&executable);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "Summary {choice: Choice.Number(42), values: [20, 22]}"
    );
}

#[test]
fn lowers_printing_and_string_and_symbol_patterns() {
    let compilation = foster::compile(
        r#"
func text(value: String) -> Int {
    branch value {
        "Foster" -> 20
        _ -> 0
    }
}

func symbol(value: Symbol) -> Int {
    branch value {
        :ready -> 22
        _ -> 0
    }
}

func main() {
    print("answer:", text("Foster") + symbol(:ready))
    println("!")
}
"#,
    )
    .unwrap();
    let executable = std::env::temp_dir().join(format!(
        "foster-native-print-pattern-test-{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    build_executable(&compilation, &executable, CompileOptions::default()).unwrap();
    let output = std::process::Command::new(&executable).output().unwrap();
    let _ = std::fs::remove_file(&executable);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "answer: 42!\n");
}

#[test]
fn reports_native_arithmetic_failures_without_traps() {
    let compilation = foster::compile("func main() -> Int { 9223372036854775807 + 1 }").unwrap();
    let executable = std::env::temp_dir().join(format!(
        "foster-native-failure-test-{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    build_executable(&compilation, &executable, CompileOptions::default()).unwrap();
    let output = std::process::Command::new(&executable).output().unwrap();
    let _ = std::fs::remove_file(&executable);
    assert!(!output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr).trim(),
        "error: integer overflow"
    );
}

#[test]
fn lowers_native_path_environment_filesystem_clock_and_entropy_services() {
    let directory =
        std::env::temp_dir().join(format!("foster-native-host-test-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let executable = directory.join(format!("host-services{}", std::env::consts::EXE_SUFFIX));
    let compilation = foster::compile(
        r#"
import core.bytes
import core.result
import std.env as environment
import std.fs as filesystem
import std.io
import std.path as paths
import std.random
import std.time

func require_unit(outcome: Result<(), IoError>) -> () {
    branch outcome {
        Result.Ok(_) -> ()
        Result.Error(error) -> {
            assert(false, error.message)
            ()
        }
    }
}

func require_bytes(outcome: Result<Bytes, IoError>) -> Bytes [consume outcome] {
    branch move outcome {
        Result.Ok(value) -> value
        Result.Error(error) -> {
            assert(false, error.message)
            Bytes.empty()
        }
    }
}

func require_length(outcome: Result<Int, IoError>) -> Int {
    branch outcome {
        Result.Ok(value) -> value
        Result.Error(error) -> {
            assert(false, error.message)
            -1
        }
    }
}

func require_entropy(outcome: Result<Bytes, RandomError>) -> Bytes [consume outcome] {
    branch move outcome {
        Result.Ok(value) -> value
        Result.Error(error) -> {
            assert(false, error.message)
            Bytes.empty()
        }
    }
}

func require_text(outcome: Result<String, IoError>) -> String [consume outcome] {
    branch move outcome {
        Result.Ok(value) -> value
        Result.Error(error) -> {
            assert(false, error.message)
            ""
        }
    }
}

func main() -> String {
    let cwd = require_text(environment::current_directory())
    let path = paths::join(cwd, "payload.bin")
    require_unit(filesystem::write_bytes(path, "Foster".utf8))
    assert(filesystem::exists?(path))
    assert(filesystem::file?(path))
    assert(!filesystem::directory?(path))
    assert(require_length(filesystem::file_length(path)) == 6)
    let contents = require_bytes(filesystem::read_range(path, 1, 4))
    assert(contents.hex() == "6f737465")
    assert(time::now().nanosecond() >= 0)
    assert(require_entropy(random::SystemRandom.new().bytes(8)).length == 8)
    require_unit(filesystem::remove_file(path))
    contents.hex()
}
"#,
    )
    .unwrap();
    build_executable(&compilation, &executable, CompileOptions::default()).unwrap();
    let output = std::process::Command::new(&executable)
        .current_dir(&directory)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&directory);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "6f737465");
}

#[test]
fn lowers_native_tcp_resource_handles() {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
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
import core.result
import std.net.tcp

func finish(connection: Connection, text: String) -> String [consume connection, consume text] {{
    (move connection).close()
    text
}}

func receive(connection: Connection, outcome: Result<String, NetworkError>) -> String [consume connection, consume outcome] {{
    branch move outcome {{
        Result.Ok(text) -> finish(move connection, move text)
        Result.Error(error) -> error.message
    }}
}}

func send(connection: Connection, outcome: Result<(), NetworkError>) -> String [consume connection, consume outcome] {{
    branch move outcome {{
        Result.Ok(_) -> read_connection(move connection)
        Result.Error(error) -> error.message
    }}
}}

func read_connection(connection: Connection) -> String [consume connection] {{
    let outcome = connection.read_text(16)
    receive(move connection, move outcome)
}}

func use_connection(outcome: Result<Connection, NetworkError>) -> String [consume outcome] {{
    branch move outcome {{
        Result.Ok(connection) -> write_connection(move connection)
        Result.Error(error) -> error.message
    }}
}}

func write_connection(connection: Connection) -> String [consume connection] {{
    let outcome = connection.write_text("ping")
    send(move connection, move outcome)
}}

func main() -> String {{
    let reply = use_connection(tcp::connect("127.0.0.1", {port}))
    let failure = use_connection(tcp::connect("127.0.0.1", -1))
    reply + "|" + failure
}}
"#
    );
    let compilation = foster::compile(&source).unwrap();
    let executable = std::env::temp_dir().join(format!(
        "foster-native-tcp-test-{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    build_executable(&compilation, &executable, CompileOptions::default()).unwrap();
    let output = std::process::Command::new(&executable).output().unwrap();
    let _ = std::fs::remove_file(&executable);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "pong|port must be between 0 and 65535"
    );
    assert_eq!(&server.join().unwrap(), b"ping");
}

#[test]
fn lowers_native_remote_workers_and_blocking_await() {
    let compilation = foster::compile(
        r#"
type Counter = {
    value: Int
}

type Inspector = {}
type Snapshot = { value: Int }
type Echo = {}

func Counter.increment(self: Counter, amount: Int) -> Int [mut self] {
    self.value = self.value + amount
    self.value
}

func Counter.snapshot(self: Counter) -> Int [read self.value] { self.value }
func Counter.assign(self: Counter, value: Int) -> Int [mut self] {
    self.value = value
    self.value
}
func Counter.snapshot_record(self: Counter) -> Snapshot [read self.value] {
    Snapshot { value: self.value }
}
func Inspector.inspect(self: Inspector, counter: Counter) -> Int [read counter.value] {
    counter.value
}
func Echo.tag<T>(self: Echo, value: T) -> Int {
    value
    1
}

func main() -> Int {
    let counter = remote Counter { value: 0 }
    let first = counter.increment(2)
    let second = counter.increment(3)
    let ordered = await first + await second
    let snapshot = await counter.snapshot_record()

    let local = Counter { value: 0 }
    let reader = remote ref local
    let before = await reader.snapshot()
    local.assign(42)
    let after = await reader.snapshot()

    let inspected = Counter { value: 0 }
    let inspector = remote Inspector {}
    let pending = inspector.inspect(inspected)
    inspected.assign(42)
    let inspected_before = await pending
    let inspected_after = await inspector.inspect(inspected)

    let echo = remote Echo {}
    let generic = await echo.tag(1) + await echo.tag(1.5)
    ordered + snapshot.value + before + after + inspected_before + inspected_after + generic
}
"#,
    )
    .unwrap();
    let executable = std::env::temp_dir().join(format!(
        "foster-native-remote-test-{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    build_executable(&compilation, &executable, CompileOptions::default()).unwrap();
    let output = std::process::Command::new(&executable).output().unwrap();
    let _ = std::fs::remove_file(&executable);
    assert!(
        output.status.success(),
        "status {}: {}; stdout: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "98");
}
