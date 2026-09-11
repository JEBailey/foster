//! The same source and expectations must hold for both execution engines and optimization modes.
use foster::{native, vm};

#[test]
fn grapheme_accessors_work_without_imports() {
    check(
        "implicit-graphemes",
        r#"
func main() -> Int {
    let text = "é👩‍💻"
    let count = [move text] () -> text.length
    assert(count() == 2)
    assert("é!".head == "é")
    assert("é!".rest == "!")
    42
}
"#,
        Ok("42"),
    );
}

#[test]
fn grapheme_string_operations_agree_in_both_backends() {
    check(
        "graphemes",
        include_str!("fixtures/programs/graphemes.fos"),
        Ok("42"),
    );
}

#[test]
fn builtin_sequence_tail_calls_preserve_concrete_types() {
    check(
        "concrete-sequence-tails",
        r#"
import core.string
import core.list
import core.bytes
import std.sequence

func tail<T>(values: Sequence<T>) -> Sequence<T> { values.rest() }
func main() -> Int {
    let text = "Aé!"
    let numbers = [1, 2, 3]
    let octets = "abc".bytes
    assert(text.rest() == text.rest)
    assert(text.rest().rest() == "!")
    assert(numbers.rest() == numbers.rest)
    assert(numbers.rest().rest() == [3])
    assert(octets.rest() == octets.rest)
    assert(octets.rest().hex() == "6263")
    assert("".rest() == "")
    assert(tail(text).head() == "é")
    assert(tail(numbers).head() == 2)
    assert(tail(octets).length() == 2)
    42
}
"#,
        Ok("42"),
    );
}

#[test]
fn grapheme_boundaries_conform_to_unicode_in_both_backends() {
    let cases = include_str!("../tools/unicode/17.0.0/GraphemeBreakTest.txt")
        .lines()
        .map(|line| line.split('#').next().unwrap().trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    assert!(cases.len() > 700);
    let source = include_str!("fixtures/programs/grapheme_conformance.fos")
        .replace("__CASES__", &cases.join("\\n"));
    check("grapheme-conformance", &source, Ok("42"));
}

#[test]
fn for_loops_agree_in_both_backends() {
    check(
        "for-loops",
        include_str!("fixtures/programs/for_loops.fos"),
        Ok("42"),
    );
}

#[test]
fn while_loops_agree_in_both_backends() {
    check(
        "while-loops",
        include_str!("fixtures/programs/while_loops.fos"),
        Ok("42"),
    );
}

#[test]
fn returned_callables_can_be_forwarded_selected_and_nested() {
    check(
        "returned-callables",
        include_str!("fixtures/programs/returned_callables.fos"),
        Ok("42"),
    );
}

#[test]
fn code_point_arithmetic_promotes_both_operands_to_int() {
    check(
        "code-point-arithmetic",
        r#"
func main() -> Int {
    assert('9' - '0' == 9)
    assert('0' - '9' == -9)
    assert('😀' - 'a' == 128415)
    assert('a' + 'b' == 195)
    42
}
"#,
        Ok("42"),
    );
}

#[test]
fn standard_library_sha256_agrees_in_both_backends() {
    check(
        "sha256",
        include_str!("fixtures/programs/sha256.fos"),
        Ok("42"),
    );
}

#[test]
fn unicode_classification_and_casing_agree_in_both_backends() {
    check(
        "unicode",
        include_str!("fixtures/programs/unicode.fos"),
        Ok("42"),
    );
}

#[test]
fn unicode_mappings_conform_to_ucd_in_both_backends() {
    check(
        "unicode-conformance",
        include_str!("fixtures/programs/unicode_conformance.fos"),
        Ok("42"),
    );
}

#[test]
fn collection_contracts_preserve_concrete_implementations() {
    check(
        "collection-contracts",
        include_str!("fixtures/programs/collection_contracts.fos"),
        Ok("42"),
    );
}

#[test]
fn ordered_composition_defaults_agree_in_both_backends() {
    check(
        "ordered-generics",
        include_str!("fixtures/programs/ordered_generic_defaults.fos"),
        Ok("42"),
    );
    check(
        "ordered-composition",
        include_str!("fixtures/programs/ordered_composition.fos"),
        Ok("42"),
    );
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/composition_defaults");
    let compilation = foster::check_package(path).unwrap();
    check_compilation("default-lexical-scope", &compilation, Ok("42"));
}

#[test]
fn foster_hash_collections_agree_in_both_backends() {
    check(
        "hash-collections",
        include_str!("fixtures/programs/hash_collections.fos"),
        Ok("42"),
    );
}

#[test]
fn implicit_unit_results_agree_in_both_backends() {
    check(
        "unit-results",
        include_str!("fixtures/programs/unit_results.fos"),
        Ok("42"),
    );
}

#[test]
fn foster_library_loops_slices_and_builders_agree() {
    check(
        "library-algorithms",
        include_str!("fixtures/programs/library_algorithms.fos"),
        Ok("42"),
    );
}

#[test]
fn owned_list_reads_return_copy_or_a_typed_error_in_both_backends() {
    check(
        "list-at-result",
        r#"
import core.list
import core.copy
import core.result
type Plain = { value: Int }
type Item = & Copy & { value: Int }
impl Item {
    func copy(self) -> self { Item { value: self.value + 1 } }
}
func checked(values: List<Int>, index: Int) -> Result<Int, ListReadError> {
    let value = try values.at(index)
    Result.Ok(value + 1)
}
func main() -> Bool {
    assert([41].at(0) == Result.Ok(41))
    assert([41].at(-1) == Result.Error(ListReadError.OutOfBounds))
    assert([41].at(1) == Result.Error(ListReadError.OutOfBounds))
    assert(checked([41], 0) == Result.Ok(42))
    assert(checked([], 0) == Result.Error(ListReadError.OutOfBounds))
    assert([Plain { value: 1 }].at(0) == Result.Error(ListReadError.NotCopyable))
    assert([Plain { value: 1 }].at(9) == Result.Error(ListReadError.OutOfBounds))
    assert([Item { value: 41 }].at(0) == Result.Ok(Item { value: 42 }))
    assert(["hello"].at(0) == Result.Ok("hello"))
    true
}
"#,
        Ok("true"),
    );
}

struct Scratch(std::path::PathBuf);

#[test]
fn destructors_follow_ownership_and_run_before_fields() {
    check_stdout(
        "deinit",
        r#"
import core.drop
import core.copy
type Tracked = & Drop & Copy & { id: Int }
impl Tracked {
    func copy(self) -> self { Tracked { id: self.id + 10 } }
    func deinit(self) -> () { println(self.id) }
}
type Outer = & Drop & { child: Tracked }
impl Outer { func deinit(self) -> () { println(100) } }
func finish_value(value: Tracked) -> () [consume value] { println(200) }
func scope() -> () {
    let value = Tracked { id: 1 }
    let copied = value.copy()
    finish_value(move copied)
    println(300)
    let moved = value
    println(moved.id)
    ()
}
func main() -> Int {
    scope()
    let outer = Outer { child: Tracked { id: 2 } }
    println(400)
    42
}
"#,
        "200\n11\n300\n1\n1\n400\n100\n2\n42",
    );
}

fn check_stdout(name: &str, source: &str, expected: &str) {
    check_process_output(name, source, expected, None);
}

fn check_process_output(name: &str, source: &str, expected: &str, failure: Option<&str>) {
    let compilation = foster::compile(source).unwrap();
    let prepared = native::prepare(&compilation).unwrap();
    let scratch = Scratch::new(name);
    for optimize in [false, true] {
        let bytecode =
            vm::compile_with_options(&compilation, vm::CompileOptions { optimize }).unwrap();
        let path = scratch.0.join(format!("program-{optimize}.fbc"));
        std::fs::write(&path, vm::encode_program(&bytecode).unwrap()).unwrap();
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_foster"))
            .arg("run")
            .arg(path)
            .output()
            .unwrap();
        assert_eq!(
            output.status.success(),
            failure.is_none(),
            "VM {name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        if let Some(message) = failure {
            assert!(
                String::from_utf8_lossy(&output.stderr).contains(message),
                "VM {name}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .replace("\r\n", "\n")
                .trim(),
            expected,
            "VM {name}, optimize={optimize}"
        );
        let path = scratch.0.join(format!(
            "program-{optimize}{}",
            std::env::consts::EXE_SUFFIX
        ));
        prepared
            .build_executable(&path, native::CompileOptions { optimize })
            .unwrap();
        let output = std::process::Command::new(path).output().unwrap();
        assert_eq!(
            output.status.success(),
            failure.is_none(),
            "native {name}, optimize={optimize}, status={}: {}\nstdout: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
        if let Some(message) = failure {
            assert!(
                String::from_utf8_lossy(&output.stderr).contains(message),
                "native {name}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .replace("\r\n", "\n")
                .trim(),
            expected,
            "native {name}, optimize={optimize}"
        );
    }
}

#[test]
fn destructors_handle_replacement_branches_and_loop_exits() {
    check_stdout(
        "deinit-control-flow",
        r#"
import core.drop
type Item = & Drop & { id: Int }
impl Item { func deinit(self) -> () { println(self.id) } }
func finish_value(value: Item) -> () [consume value] { println(60) }
func early() -> () { let item = Item { id: 7 }
 return () if true
 () }
func main() -> Int {
    let item = Item { id: 1 }
    println(10)
    item = Item { id: 2 }
    println(20)
    branch { true -> { let inner = Item { id: 3 }
 println(30)
 () }
 _ -> () }
    let index = 0
    loop {
        let inner = Item { id: 4 + index }
        index = index + 1
        continue if index == 1
        break
    }
    finish_value(Item { id: 6 })
    early()
    println(80)
    42
}
"#,
        "10\n1\n20\n30\n3\n4\n5\n60\n6\n7\n80\n2\n42",
    );
}

#[test]
fn list_at_copy_results_have_independent_cleanup() {
    check_stdout(
        "deinit-list-copy",
        r#"
import core.drop
import core.copy
import core.list
import core.result
type Item = & Drop & Copy & { id: Int }
impl Item {
    func copy(self) -> self { Item { id: self.id + 10 } }
    func deinit(self) -> () { println(self.id) }
}
type Resource = & Drop & { id: Int }
impl Resource { func deinit(self) -> () { println(self.id) } }
func read_first<T>(values: List<T>) -> Result<T, ListReadError> { values.at(0) }
func main() -> Int {
    let values = [Item { id: 1 }]
    let resources = [Resource { id: 2 }]
    assert(resources.at(0) == Result.Error(ListReadError.NotCopyable))
    branch read_first(values) {
        Result.Ok(copied) -> { assert(copied.id == 11)
 println(30)
 () }
        Result.Error(_) -> { assert(false)
 () }
    }
    println(50)
    42
}
"#,
        "30\n11\n50\n2\n1\n42",
    );
}

#[test]
fn enum_destructors_observe_the_live_payload() {
    check_stdout(
        "deinit-enum",
        r#"
import core.drop
enum Choice = Value(Int) | Empty & Drop
impl Choice {
    func deinit(self) -> () {
        branch self { Choice.Value(value) -> println(value)
 Choice.Empty -> println(0) }
    }
}
func main() -> Int {
    let first = Choice.Value(1)
    let second = Choice.Empty
    println(10)
    42
}
"#,
        "10\n0\n1\n42",
    );
}

impl Scratch {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "foster-parity-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn check(name: &str, source: &str, expected: Result<&str, &str>) {
    let compilation = foster::compile(source).unwrap();
    check_compilation(name, &compilation, expected);
}

#[test]
fn tcp_connections_close_on_return_try_failure_and_runtime_failure() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::{Duration, Instant};
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let source = format!(
        r#"
import std.net.tcp
import core.result
type Worker = {{}}
func fail() -> Result<(), NetworkError> {{
    Result.Error(NetworkError {{ operation: "test", message: "expected" }})
}}
func use_socket(connection: Connection, mode: Int) -> Result<(), NetworkError> [consume connection] {{
    try connection.write_text("ping")
    branch {{
        mode == 1 -> {{
            try fail()
            ()
        }}
        mode == 2 -> {{
            try (move connection).close()
            ()
        }}
        mode == 3 -> {{
            let values = [1]
            assert(values[mode] == 1)
            ()
        }}
        _ -> ()
    }}
    Result.Ok(())
}}
impl Worker {{
    func execute(self, mode: Int) -> Result<(), NetworkError> {{
        let connection = try tcp::connect("127.0.0.1", {port})
        use_socket(move connection, mode)
    }}
}}
func observe() -> Result<String, NetworkError> {{
    let connection = try tcp::connect("127.0.0.1", {port})
    try connection.set_timeout(5000)
    connection.read_text(16)
}}
func main() -> Bool {{
    let mode = 0
    loop {{
        break if mode == 4
        let worker = remote Worker {{}}
        let outcome = await worker.execute(mode)
        assert(outcome.error?() == (mode == 3))
        assert(observe() == Result.Ok("closed"))
        mode = mode + 1
    }}
    true
}}
"#
    );
    let compilation = foster::compile(&source).unwrap();
    let prepared = native::prepare(&compilation).unwrap();
    let scratch = Scratch::new("tcp-drop");
    for optimize in [false, true] {
        let executable = scratch
            .0
            .join(format!("tcp-{optimize}{}", std::env::consts::EXE_SUFFIX));
        prepared
            .build_executable(&executable, native::CompileOptions { optimize })
            .unwrap();
        for native_run in [false, true] {
            let server_socket = listener.try_clone().unwrap();
            let server = std::thread::spawn(move || -> std::io::Result<()> {
                let accept = || {
                    let deadline = Instant::now() + Duration::from_secs(10);
                    loop {
                        match server_socket.accept() {
                            Ok((stream, _)) => return Ok(stream),
                            Err(error)
                                if error.kind() == std::io::ErrorKind::WouldBlock
                                    && Instant::now() < deadline =>
                            {
                                std::thread::sleep(Duration::from_millis(5))
                            }
                            Err(error) => return Err(error),
                        }
                    }
                };
                for _ in 0..4 {
                    let mut stream = accept()?;
                    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
                    let mut ping = [0; 4];
                    stream.read_exact(&mut ping)?;
                    assert_eq!(&ping, b"ping");
                    let mut byte = [0];
                    assert_eq!(
                        stream.read(&mut byte)?,
                        0,
                        "socket should close before the next connection"
                    );
                    let mut observer = accept()?;
                    observer.write_all(b"closed")?;
                }
                Ok(())
            });
            let result = if native_run {
                let output = std::process::Command::new(&executable).output().unwrap();
                (
                    output.status.success(),
                    String::from_utf8_lossy(&output.stdout).trim().to_owned(),
                    String::from_utf8_lossy(&output.stderr).into_owned(),
                )
            } else {
                match vm::run_with_options(&compilation, vm::CompileOptions { optimize }) {
                    Ok(value) => (true, value.to_string(), String::new()),
                    Err(error) => (false, String::new(), error.to_string()),
                }
            };
            let server_result = server.join().unwrap();
            assert!(
                result.0,
                "native={native_run}, optimize={optimize}: {}",
                result.2
            );
            assert_eq!(result.1, "true");
            server_result.unwrap();
        }
    }
}

#[test]
fn tcp_listeners_release_the_port_and_borrows_preserve_the_owner() {
    let reservation = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = reservation.local_addr().unwrap().port();
    drop(reservation);
    check(
        "tcp-listener-drop",
        &format!(
            r#"
import std.net.tcp
import core.result
func borrowed(listener: Listener) -> Bool {{
    assert(listener.location.port() == {port})
    tcp::listen("127.0.0.1", {port}).error?()
}}
func lifetime(explicit: Bool) -> Result<(), NetworkError> {{
    let listener = try tcp::listen("127.0.0.1", {port})
    assert(borrowed(listener))
    branch {{
        explicit -> {{ try (move listener).close()
            () }}
        _ -> ()
    }}
    Result.Ok(())
}}
func main() -> Bool {{
    assert(lifetime(false).success?())
    assert(lifetime(false).success?())
    assert(lifetime(true).success?())
    lifetime(false).success?()
}}
"#
        ),
        Ok("true"),
    );
}

fn check_compilation(
    name: &str,
    compilation: &foster::compiler::Compilation,
    expected: Result<&str, &str>,
) {
    let prepared = native::prepare(compilation).unwrap();
    let scratch = Scratch::new(name);
    for optimize in [false, true] {
        let result = vm::run_with_options(compilation, vm::CompileOptions { optimize });
        match expected {
            Ok(value) => assert_eq!(
                result.unwrap().to_string(),
                value,
                "VM {name}, optimize={optimize}"
            ),
            Err(message) => assert!(
                result.unwrap_err().to_string().contains(message),
                "VM {name}"
            ),
        }
        let executable = scratch.0.join(format!(
            "program-{optimize}{}",
            std::env::consts::EXE_SUFFIX
        ));
        prepared
            .build_executable(&executable, native::CompileOptions { optimize })
            .unwrap();
        let output = std::process::Command::new(executable).output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        match expected {
            Ok(value) => {
                assert!(output.status.success(), "native {name}: {stderr}");
                assert_eq!(
                    String::from_utf8_lossy(&output.stdout).trim(),
                    value,
                    "native {name}, optimize={optimize}"
                );
            }
            Err(message) => {
                assert!(
                    !output.status.success(),
                    "native {name} unexpectedly succeeded"
                );
                assert!(stderr.contains(message), "native {name}: {stderr}");
            }
        }
    }
}

#[test]
fn collection_cursors_release_visited_and_unvisited_values_on_failure() {
    check_process_output(
        "cursor-cleanup",
        r#"
import std.iter
import core.option
import core.drop
type Held = & Drop & { id: Int }
impl Held { func deinit(self) -> () { println(self.id) } }
func main() -> Int {
    let cursor = [Held { id: 1 }, Held { id: 2 }].iterator()
    branch cursor.next() {
        Option.Some(value) -> { assert(value.id == 1) }
        Option.None -> { assert(false) }
    }
    assert(false, "cursor failure")
    0
}
"#,
        "1\n2",
        Some("cursor failure"),
    );
}

#[test]
fn collection_cursors_preserve_snapshots_and_decode_utf8() {
    check(
        "collection-cursors",
        r#"
import std.iter
import std.iter.map
import std.iter.take
import core.option
import core.string
import core.byte
func saved() -> Iterator<String> {
    let source = ["first", "second"]
    source.iterator()
}
func empty_list() -> List<Int> { [] }
func main() -> Bool {
    let source = [1, 2, 3]
    let first = source.iterator()
    let second = source.iterator()
    source.push(4)
    source[0] = 99
    assert(first.next() == Option.Some(1))
    assert(second.collect() == [1, 2, 3])
    assert(first.collect() == [2, 3])
    assert(first.next() == Option.None)
    assert(first.next() == Option.None)
    assert(source == [99, 2, 3, 4])
    assert(saved().collect() == ["first", "second"])
    assert(empty_list().iterator().count() == 0)
    let text = "aλ€🦀"
    let letters = text.iterator()
    let other = text.iterator()
    text = "changed"
    assert(letters.next() == Option.Some("a"))
    assert(letters.next() == Option.Some("λ"))
    assert(letters.next() == Option.Some("€"))
    assert(letters.next() == Option.Some("🦀"))
    assert(letters.next() == Option.None)
    assert(letters.next() == Option.None)
    assert(other.collect() == ["a", "λ", "€", "🦀"])
    assert("".iterator().count() == 0)
    assert("aλ€🦀".iterator().map((value: String) -> value).take(3).collect() == ["a", "λ", "€"])
    let octets = "ab".bytes.iterator()
    assert(octets.next() == Option.Some(Byte.unchecked(97)))
    assert(octets.next() == Option.Some(Byte.unchecked(98)))
    assert(octets.next() == Option.None)
    assert("".bytes.iterator().count() == 0)
    true
}
"#,
        Ok("true"),
    );
}

#[test]
fn generic_sequence_iterators_dispatch_all_builtin_representations() {
    check(
        "sequence-iterators",
        r#"
import std.iter
import core.option
import core.bytes
import core.byte
import std.iter.map
import std.iter.filter
import std.iter.skip
import std.iter.take
func iterator<T>(values: Sequence<T>) -> Iterator<T> [consume values] {
    Iterator.from_sequence(move values)
}
func main() -> Bool {
    let numbers = iterator([7, 8])
    assert(numbers.next() == Option.Some(7))
    assert(numbers.next() == Option.Some(8))
    assert(numbers.next() == Option.None)
    assert(numbers.next() == Option.None)
    let text = iterator("λ🦀")
    assert(text.next() == Option.Some("λ"))
    assert(text.next() == Option.Some("🦀"))
    assert(text.next() == Option.None)
    let characters = iterator(['λ', '🦀'])
    assert(characters.collect() == ['λ', '🦀'])
    let octets = iterator("ab".bytes)
    assert(octets.next() == Option.Some(Byte.unchecked(97)))
    assert(octets.next() == Option.Some(Byte.unchecked(98)))
    assert(octets.next() == Option.None)
    let values = [1, 2, 3]
    let first = values.iterator()
    let second = values.iterator()
    assert(first.next() == Option.Some(1))
    assert(second.next() == Option.Some(1))
    assert(first.count() == 2)
    assert(second.count() == 2)
    assert(values == [1, 2, 3])
    let words = iterator(["one", "two"])
    assert(words.collect() == ["one", "two"])
    assert(iterator("").count() == 0)
    let pipeline = [1, 2, 3, 4, 5].iterator().map((value: Int) -> value * 2).filter((value: Int) -> value > 4).skip(1).take(2).collect()
    assert(pipeline == [8, 10])
    true
}
"#,
        Ok("true"),
    );
}

#[test]
fn generic_sequence_user_accessors_can_change_tail_representation() {
    check(
        "sequence-user-accessors",
        r#"
import std.iter
import std.sequence
import core.string
type TextSlice = & Sequence<String> & { text: String }
impl TextSlice {
    func empty?(self) -> Bool { self.text.empty? }
    func length(self) -> Int { self.text.length }
    func head(self) -> String { self.text.head }
    func rest(self) -> String { self.text.rest }
}
func letters(values: Sequence<String>) -> List<String> [consume values] {
    let cursor = Iterator.from_sequence(move values)
    cursor.collect()
}
func measure<T>(values: Sequence<T>) -> Int { values.length() }
func main() -> Bool {
    assert(letters(TextSlice { text: "λ🦀!" }) == ["λ", "🦀", "!"])
    assert(letters(TextSlice { text: "" }) == "".graphemes())
    assert(letters(["a", "b"]) == ["a", "b"])
    assert(measure(TextSlice { text: "λ🦀!" }) == 3)
    assert(measure([1, 2]) == 2)
    assert(sequence::count("banana", (value: String) -> value == "a") == 3)
    assert(sequence::count([1, 2, 3], (value: Int) -> value > 1) == 2)
    true
}
"#,
        Ok("true"),
    );
}

#[test]
fn generic_sequence_failure_releases_the_callers_values() {
    check_process_output(
        "sequence-failure",
        r#"
import std.iter
import std.iter.map
import core.drop
type Held = & Drop & { text: String }
impl Held { func deinit(self) -> () { println(self.text) } }
func main() -> Int {
    let held = Held { text: "cleaned" }
    let cursor = [1, 2, 3].iterator().map((value: Int) -> {
        assert(value < 2, "sequence failure")
        value
    })
    cursor.collect()
    0
}
"#,
        "cleaned",
        Some("sequence failure"),
    );
}

#[test]
fn aggregates_compare_by_value() {
    check(
        "equality",
        r#"
import core.list
type Point = { x: Int, label: String }
enum Choice = Number(Int) | Text(String) | Empty
func empty() -> List<Int> { [] }
func main() -> Bool {
    assert(Point { x: 1, label: "a" } == Point { x: 1, label: "a" })
    assert(Point { x: 1, label: "a" } != Point { x: 2, label: "a" })
    assert([1, 2] == [1, 2])
    assert([1, 2] != [1, 3])
    assert([1] != [1, 2])
    assert(Choice.Number(1) == Choice.Number(1))
    assert(Choice.Number(1) != Choice.Number(2))
    assert(Choice.Empty == Choice.Empty)
    assert(Choice.Number(1) != Choice.Empty)
    assert([Choice.Text("abc")] == [Choice.Text("a" + "bc")])
    assert([[1], [2]] == [[1], [2]])
    assert([0.0] == [-0.0])
    assert(empty() == empty())
    assert("abc".bytes == ("a" + "bc").bytes)
    assert("abc".bytes != "abd".bytes)
    assert([Point { x: 1, label: "a" }].contains?(Point { x: 1, label: "a" }))
    let nan = 0.0 / 0.0
    assert([nan] != [nan])
    let values = [1, 2]
    values[0] = 3
    values == [3, 2]
}
"#,
        Ok("true"),
    );
}

#[test]
fn assignment_evaluates_the_value_before_the_destination() {
    check(
        "assignment-order",
        r#"
type Trace = { value: Int }

impl Trace {
    func mark(self: Trace, digit: Int) -> Int [mut self.value] {
        self.value = self.value * 10 + digit
        digit
    }
}

func main() -> Int {
    let trace = Trace { value: 0 }
    let values = [0]
    values[trace.mark(2) - 2] = trace.mark(1)
    trace.value * 100 + values[0]
}
"#,
        Ok("1201"),
    );
}

#[test]
fn calls_aggregates_projections_and_branches_follow_source_order() {
    check(
        "evaluation-order",
        r#"
type Trace = { value: Int }
type Sink = {}
type Pair = { a: Int, z: Int }

impl Trace {
    func mark(self: Trace, digit: Int) -> Int [mut self.value] {
        self.value = self.value * 10 + digit
        digit
    }

    func reset(self: Trace) -> () [mut self.value] {
        self.value = 0
        ()
    }
}
func combine(left: Int, right: Int) -> Int { left * 10 + right }

impl Trace {
    func callable(self: Trace) -> func(Int, Int) -> Int [mut self.value] {
        self.mark(1)
        combine
    }

    func sink(self: Trace) -> Sink [mut self.value] {
        self.mark(1)
        Sink {}
    }
}

impl Sink {
    func accept(self: Sink, value: Int) -> Int { value }
}

impl Trace {
    func values(self: Trace) -> List<Int> [mut self.value] {
        self.mark(1)
        [7]
    }

    func record(self: Trace, digit: Int, answer: Bool) -> Bool [mut self.value] {
        self.mark(digit)
        answer
    }
}

func main() -> Int {
    let trace = Trace { value: 0 }

    assert(combine(trace.mark(1), trace.mark(2)) == 12)
    assert(trace.value == 12)

    trace.reset()
    assert(trace.callable()(trace.mark(2), trace.mark(3)) == 23)
    assert(trace.value == 123)

    trace.reset()
    assert(trace.sink().accept(trace.mark(2)) == 2)
    assert(trace.value == 12)

    trace.reset()
    let values = [trace.mark(1), trace.mark(2)]
    assert(values == [1, 2] && trace.value == 12)

    trace.reset()
    let pair = Pair { z: trace.mark(1), a: trace.mark(2) }
    assert(pair.z == 1 && pair.a == 2 && trace.value == 12)

    trace.reset()
    assert(trace.mark(1) * 10 + trace.mark(2) == 12)
    assert(trace.value == 12)

    trace.reset()
    assert(trace.values()[trace.mark(2) - 2] == 7)
    assert(trace.value == 12)

    trace.reset()
    let selected = branch {
        trace.record(1, false) -> 0
        trace.record(2, true) -> 7
        _ -> 9
    }
    assert(selected == 7 && trace.value == 12)
    trace.value
}
"#,
        Ok("12"),
    );
}

#[test]
fn partial_application_captures_operands_once_in_source_order() {
    check(
        "partial-application-timing",
        r#"
type Trace = { value: Int }
type Sink = {}

impl Trace {
    func mark(self: Trace, digit: Int) -> Int [mut self.value] {
        self.value = self.value * 10 + digit
        digit
    }

    func reset(self: Trace) -> () [mut self.value] {
        self.value = 0
        ()
    }
}

func combine(left: Int, middle: Int, right: Int) -> Int {
    left * 100 + middle * 10 + right
}

impl Trace {
    func callable(self: Trace) -> func(Int, Int, Int) -> Int [mut self.value] {
        self.mark(1)
        combine
    }

    func sink(self: Trace) -> Sink [mut self.value] {
        self.mark(1)
        Sink {}
    }
}

impl Sink {
    func accept(self: Sink, prefix: Int, value: Int) -> Int { prefix * 10 + value }
}

func main() -> Int {
    let trace = Trace { value: 0 }
    let combine_middle = trace.callable()(trace.mark(2), _, trace.mark(3))
    assert(trace.value == 123)
    assert(combine_middle(4) == 243)
    assert(combine_middle(5) == 253)
    assert(trace.value == 123)

    trace.reset()
    let accept_value = trace.sink().accept(trace.mark(2), _)
    assert(trace.value == 12)
    assert(accept_value(7) == 27)
    assert(trace.value == 12)

    let make_code_point = from_code_point(_)
    assert(make_code_point(65) == 'A')
    trace.value
}
"#,
        Ok("12"),
    );
}

#[test]
fn borrowed_temporaries_share_the_complete_full_expression() {
    check(
        "full-expression-temporaries",
        r#"
func make(value: Int) -> Int { value }

func combine[left: group Int, right: group Int](first: ref[left] Int, second: ref[right] Int) -> Int {
    first * 10 + second
}

func main() -> Int {
    combine(ref (make(4)), ref (make(2)))
}
"#,
        Ok("42"),
    );
}

#[test]
fn minimum_integer_negation_is_a_language_error() {
    check(
        "negation",
        "func main() -> Int { let value = -9223372036854775807 - 1\n -value }",
        Err("overflow"),
    );
}

#[test]
fn assertions_are_language_errors() {
    check(
        "assertion",
        "func main() { assert(false, \"parity failure\") }",
        Err("parity failure"),
    );
}

#[test]
fn ownership_and_remote_ordering_agree() {
    check(
        "ownership-remote",
        r#"
import core.result as outcomes

type Counter = { value: Int }
impl Counter {
    func increment(self: Counter, amount: Int) -> Int [mut self] {
        self.value = self.value + amount
        self.value
    }
}
func set[g: group Int](value: ref[g] Int, replacement: Int) -> Int [mut g] {
    value = replacement
}
func take(value: String) -> () [consume value] { () }
func main() -> Int {
    let text = "before"
    take(move text)
    text = "after"
    assert(text == "after", "text assignment")
    let values = [0, 2]
    set(ref values[0], 40)
    assert(values == [40, 2], "reference assignment")
    let worker = remote Counter { value: 0 }
    let first = worker.increment(20)
    let second = worker.increment(2)
    (await first).unwrap_or(0) + (await second).unwrap_or(0)
}
"#,
        Ok("42"),
    );
}

#[test]
fn generic_calls_preserve_values() {
    check(
        "generics",
        r#"
type Echo<T> = { value: T }
impl Echo {
    func get<T>(self: Echo<T>) -> T { self.value }
}
func identity<T>(value: T) -> T { value }
func main() -> Bool { identity(Echo { value: true }.get()) }
"#,
        Ok("true"),
    );
}

#[test]
fn generic_remote_calls_preserve_logical_types() {
    check(
        "remote-generics",
        r#"
import core.result as outcomes

type Echo = {}
impl Echo {
    func identity<T>(self: Echo, value: T) -> T { value }
    func tag<T>(self: Echo, value: T) -> Int { 1 }
}
func main() -> Int {
    let worker = remote Echo {}
    let number = (await worker.identity(41)).unwrap_or(0)
    let tag = (await worker.tag(:hello)).unwrap_or(0)
    assert((await worker.identity(:hello)).unwrap_or(:fallback) == :hello)
    number + tag
}
"#,
        Ok("42"),
    );
}

#[test]
fn nested_remote_awaits_preserve_native_frames_and_owned_values() {
    check(
        "nested-remote-awaits",
        include_str!("fixtures/programs/native_virtual_threads.fos"),
        Ok("42"),
    );
}

#[test]
fn verifier_rejects_inconsistent_specialized_call_results() {
    for source in [
        "type Echo<T> = { value: T }\nimpl Echo {\n    func get<T>(self: Echo<T>) -> T { self.value }\n}\nfunc main() -> Bool { Echo { value: true }.get() }",
        "func identity<T>(value: T) -> T { value }\nfunc main() -> Bool { identity(true) }",
    ] {
        let compilation = foster::compile(source).unwrap();
        let mut program =
            vm::compile_with_options(&compilation, vm::CompileOptions { optimize: false }).unwrap();
        assert!(vm::verify(&program).is_ok());
        assert!(vm::decode_program(&vm::encode_program(&program).unwrap()).is_ok());
        program
            .functions
            .get_mut(&program.main.unwrap())
            .unwrap()
            .result_type = vm::VerificationType::Integer;
        assert!(
            vm::verify(&program).is_err(),
            "{:#?}",
            program.functions[&program.main.unwrap()]
        );
        assert!(vm::encode_program(&program).is_err());
    }
}

#[test]
fn remote_failures_are_contained_sticky_and_typed() {
    check(
        "remote-failure",
        r#"
import core.result
import core.remote_error
type Worker = { value: Int }
impl Worker {
    func good(self) -> Int { self.value }
    func domain(self) -> Result<Int, String> { Result.Error("domain") }
    func fail(self) -> Int {
        self.value = 9
        assert(false, "worker failed")
        self.value = 100
        100
    }
    func later(self, text: String) -> String [consume text] { text }
}
func main() -> Int {
    let worker = remote Worker { value: 42 }
    let completed = worker.good()
    let domain = await worker.domain()
    assert(domain == Result.Ok(Result.Error("domain")))
    let failed = worker.fail()
    let queued = worker.later("queued")
    let failure = await failed
    assert(failure == Result.Error(RemoteError.Failed("assertion failed: worker failed")))
    assert((await queued).error?())
    assert((await worker.later("later")).error?())
    assert((await completed) == Result.Ok(42))
    let healthy = remote Worker { value: 7 }
    assert((await healthy.good()) == Result.Ok(7))
    42
}
"#,
        Ok("42"),
    );
}

#[test]
fn discarded_remote_requests_require_completion_before_owner_exit() {
    let error = foster::compile(
        r#"
 type Worker = {}
 impl Worker { func fail(self) -> () { assert(false) } }
 func main() -> Int { let worker = remote Worker {}
 worker.fail()
 42 }
 "#,
    )
    .expect_err("discarded request was accepted");
    assert_eq!(error.code.as_deref(), Some("E0730"));
}

#[test]
fn completed_remote_outcomes_outlive_the_owner_and_receiver_cleanup() {
    check_stdout(
        "remote-completed",
        r#"
import core.result
import core.remote_error
import core.drop
type Held = & Drop & {}
impl Held { func deinit(self) -> () { println("closed") } }
type Worker = { held: Held }
impl Worker { func value(self) -> Int { 42 } }
func completed() -> Future<Result<Int, RemoteError>> {
    let worker = remote Worker { held: Held {} }
    let earlier = worker.value()
    await worker.value()
    earlier
}
func main() -> Int { (await completed()).unwrap_or(0) }
"#,
        "closed\n42",
    );
}

#[test]
fn remote_shutdown_resolves_pending_futures_without_draining() {
    let mut compilation = foster::compile(
        r#"
import core.result
import core.remote_error
type Worker = {}
impl Worker {
    func barrier(self) -> Int { 0 }
    func forever(self) -> Int { loop {}
0 }
    func queued(self) -> Int { assert(false, "queued work started")
0 }
}
type Job = {
    owner: Remote<Worker>
    running: Future<Result<Int, RemoteError>>
    queued: Future<Result<Int, RemoteError>>
}
func start() -> Job {
    let worker = remote Worker {}
    let running = worker.forever()
    let queued = worker.queued()
    await worker.barrier()
    Job { owner: move worker, running: move running, queued: move queued }
}
func finish(worker: Remote<Worker>) -> () [consume worker] { () }
func main() -> Bool {
    let job = start()
    let running = move job.running
    let queued = move job.queued
    finish(move job.owner)
    assert((await running) == Result.Error(RemoteError.Shutdown))
    assert((await queued) == Result.Error(RemoteError.Shutdown))
    true
}
"#,
    )
    .unwrap();
    // Remove the completion witness after semantic checking to exercise the runtime
    // backstop independently of the compiler's rejection of pending owner exits.
    let waits = compilation.hir.expressions.iter().filter_map(|(id, expression)| match expression {
        foster::hir::Expr::Await(value) => match &compilation.hir.expressions[*value] {
            foster::hir::Expr::Call { callee, .. } if matches!(&compilation.hir.expressions[*callee], foster::hir::Expr::Member { name, .. } if name == "barrier") => Some(id),
            _ => None,
        },
        _ => None,
    }).collect::<Vec<_>>();
    assert_eq!(waits.len(), 1);
    compilation.hir.expressions[waits[0]] = foster::hir::Expr::Unit;
    check_compilation("remote-shutdown", &compilation, Ok("true"));
}

#[test]
fn remote_runtime_failures_stop_nested_execution() {
    for (name, result_type, expression) in [
        ("overflow", "Int", "9223372036854775807 + value"),
        ("division", "Int", "value / 0"),
        ("shift", "Byte", "Byte.unchecked(value) << 8"),
        ("bounds", "Int", "[value][9]"),
        ("text-head", "String", "\"\".head"),
        ("code-point", "CodePoint", "from_code_point(1114112)"),
        ("byte", "Byte", "Byte.unchecked(256)"),
        ("float", "Float", "parse_float(\"invalid\")"),
    ] {
        let source = format!(
            r#"
import core.result as outcomes

import core.byte
import core.float
func nested(value: Int) -> {result_type} {{ {expression} }}
type Worker = {{}}
impl Worker {{
    func fail(self) -> {result_type} {{
        let value = nested(1)
        println("must not continue after failure")
        value
    }}
}}
func main() -> Bool {{
    assert((Byte.unchecked(1) << 7).int == 128)
    let worker = remote Worker {{}}
    (await worker.fail()).error?()
}}
"#
        );
        check(&format!("remote-{name}"), &source, Ok("true"));
    }
}

#[test]
fn borrowed_remote_failure_releases_access_and_remains_terminal() {
    check(
        "borrowed-remote-failure",
        r#"
import core.result
import core.remote_error
type Worker = { value: Int }
impl Worker {
    func check(self) -> Int {
        assert(self.value != 0, "borrowed failure")
        self.value
    }
    func set(self, value: Int) { self.value = value }
}
func main() -> Bool {
    let state = Worker { value: 0 }
    let reader = remote ref state
    assert((await reader.check()).error?())
    state.set(42)
    (await reader.check()) == Result.Error(RemoteError.Failed("assertion failed: borrowed failure"))
}
"#,
        Ok("true"),
    );
}

#[test]
fn overloaded_remote_methods_deliver_typed_outcomes() {
    check(
        "overloaded-remote",
        r#"
import core.result
import core.remote_error
type Worker = {}
impl Worker {
    func get(self, value: Int) -> Int { value }
    func get(self, value: CodePoint) -> Int {
        assert(false, "overloaded failure")
        0
    }
}
func main() -> Bool {
    let worker = remote Worker {}
    assert((await worker.get(42)) == Result.Ok(42))
    (await worker.get('x')) == Result.Error(RemoteError.Failed("assertion failed: overloaded failure"))
}
"#,
        Ok("true"),
    );
}

#[test]
fn destructors_continue_after_failure_and_preserve_the_original_error() {
    let declarations = r#"
import core.drop
type Item = & Drop & { id: Int }
impl Item {
    func deinit(self) -> () {
        println(self.id)
        assert(self.id != 2, "cleanup failed")
    }
}
"#;
    for (name, body, expected, error) in [
        (
            "deinit-unwind",
            "let first = Item { id: 1 }\nlet second = Item { id: 2 }\nassert(false, \"original failed\")\n42",
            "2\n1",
            "original failed",
        ),
        (
            "deinit-failure",
            "let first = Item { id: 1 }\nlet second = Item { id: 2 }\n42",
            "2\n1",
            "cleanup failed",
        ),
    ] {
        let source = format!("{declarations}\nfunc main() -> Int {{ {body} }}");
        check_process_output(name, &source, expected, Some(error));
    }
}

#[test]
fn projected_moves_transfer_cleanup_to_the_new_owner() {
    check_stdout(
        "deinit-projected-move",
        r#"
import core.drop
type Item = & Drop & { id: Int }
impl Item { func deinit(self) -> () { println(self.id) } }
type Box = { child: Item }
func finish_value(value: Item) -> () [consume value] { println(10) }
func main() -> Int {
    let box = Box { child: Item { id: 1 } }
    finish_value(move box.child)
    println(20)
    let values = [Item { id: 2 }]
    finish_value(move values[0])
    println(30)
    42
}
"#,
        "10\n1\n20\n10\n2\n30\n42",
    );
}

#[test]
fn remote_consumed_arguments_run_deinit_before_completion() {
    check_stdout(
        "deinit-remote",
        r#"
import core.drop
import core.result
type Item = & Drop & { id: Int }
impl Item { func deinit(self) -> () { println(self.id) } }
type Worker = { id: Int }
impl Worker {
    func inspect(self, item: Item) -> Int [consume item] {
        println(10)
        item.id
    }
}
func main() -> Int {
    let worker = remote Worker { id: 0 }
    assert(await worker.inspect(Item { id: 1 }) == Result.Ok(1))
    println(20)
    42
}
"#,
        "10\n1\n20\n42",
    );
}

#[test]
fn destructor_temporaries_survive_the_full_expression() {
    check_stdout(
        "deinit-temporaries",
        r#"
import core.drop
type Item = & Drop & { id: Int }
impl Item { func deinit(self) -> () { println(self.id) } }
func inspect(value: Item) -> Int { println(10)
    value.id }
func later() -> Int { println(20)
    40 }
func combine(left: Int, right: Int) -> Int { println(30)
    left + right }
func main() -> Int {
    let result = combine(inspect(Item { id: 2 }), later())
    println(50)
    result
}
"#,
        "10\n20\n30\n2\n50\n42",
    );
}

#[test]
fn invalid_destructors_and_partial_moves_are_rejected() {
    for (source, expected) in [
        (
            "type Item = { id: Int }\nimpl Item { func deinit(self) -> Int { 0 } }",
            "deinit must return ()",
        ),
        (
            "type Item = { id: Int }\nimpl Item { func deinit(self) -> () {} }\nfunc main() -> () { Item { id: 1 }.deinit() }",
            "cannot be called directly",
        ),
        (
            "type Item = { name: String }\nimpl Item { func deinit(self) -> () {} }\nfunc main() -> String { let item = Item { name: \"x\" }\nmove item.name }",
            "cannot move a field",
        ),
        (
            "import core.copy\ntype Item = & Copy & { id: Int }\nimpl Item { func copy(self) -> Int { 1 } }\nfunc require_copy(value: Copy) -> () {}\nfunc main() -> () { require_copy(Item { id: 1 }) }",
            "expected `Item`, found `Int`",
        ),
    ] {
        let error = match foster::compile(source) {
            Err(error) => error,
            Ok(_) => panic!("invalid source was accepted: {source}"),
        };
        assert!(error.to_string().contains(expected), "{error}");
    }
}

#[test]
fn empty_resources_and_closure_environments_keep_their_drop_identity() {
    check_stdout(
        "deinit-empty-closure",
        r#"
import core.drop
import core.copy
type Empty = & Drop & Copy & {}
impl Empty {
    func copy(self) -> self { Empty {} }
    func deinit(self) -> () { println(1) }
}
type Item = & Drop & { id: Int }
impl Item { func deinit(self) -> () { println(self.id) } }
enum Parcel = Packed(Item)
func main() -> Int {
    let original = Empty {}
    let copied = original.copy()
    let child = Item { id: 3 }
    let parcel = Parcel.Packed(child)
    let captured = Item { id: 4 }
    let callback = [move captured] () -> captured.id
    assert(callback() == 4)
    println(2)
    42
}
"#,
        "2\n4\n3\n1\n1\n42",
    );
}

#[test]
fn indirect_callable_result_provenance_agrees_in_both_backends() {
    check_stdout(
        "callable-result-provenance",
        r#"
type Holder<T> = { callback: T }
func describe(value: Int) -> String { "number" }
func make() -> func(Int) -> String { describe }
func invoke(callback: func(Int) -> String, value: Int) -> String { callback(value) }
func first[g: group Int](left: ref[g] Int, right: ref[g] Int) -> ref[g] Int { ref left }
func main() -> Int {
    let values = [10]
    let selected = ref values[0]
    let held = Holder { callback: describe }
    let text = held.callback(selected)
    let forwarded = invoke(held.callback, selected)
    let captured = [ref selected] () -> { selected
        "number" }
    let snapshot = captured()
    let factory_result = make()
    let returned_text = factory_result(selected)
    let callbacks = [describe]
    callbacks.push(describe)
    callbacks[0] = describe
    let listed_text = callbacks[0](selected)
    values.push(20)
    assert(text.length + forwarded.length + snapshot.length + returned_text.length + listed_text.length == 30)
    let left = [10]
    let right = [20]
    let choice = branch { true -> Holder { callback: first }
        _ -> Holder { callback: first } }
    let answer = choice.callback(ref left[0], ref right[0])
    right.push(30)
    println(answer)
    42
}
"#,
        "10\n42",
    );
}

#[test]
fn compound_ownership_conditions_and_short_circuit_effects_agree() {
    check(
        "compound-conditions",
        r#"
func choose(a: Bool, b: Bool) -> Int {
    let values = [10]
    let selected = ref values[0]
    branch { a && b -> values.push(20)
 _ -> () }
    branch { not a || not b -> selected
 _ -> 0 }
}
func bump[g: group Int](count: ref[g] Int) -> Bool [mut g] {
    count = count + 1
    true
}
func main() -> Int {
    let count = 0
    false && bump(ref count)
    true || bump(ref count)
    true && bump(ref count)
    false || bump(ref count)
    assert(count == 2)
    choose(false, false) + choose(false, true) + choose(true, false) + choose(true, true) + count
}
"#,
        Ok("32"),
    );
}

#[test]
fn nested_indexed_assignments_update_the_original_place() {
    check(
        "nested-indexed-writes",
        r#"
type Item = { text: String, number: Int }
type Box = { items: List<Item> }
func main() -> Int {
    let items = [Item { text: "before", number: 1 }]
    items[0].text = "after"
    assert(items[0].text == "after")
    let nested = [[Item { text: "before", number: 2 }]]
    nested[0][0].number = 42
    assert(nested[0][0].number == 42)
    let box = Box { items: [Item { text: "before", number: 3 }] }
    box.items[0].text = "changed"
    assert(box.items[0].text == "changed")
    let matrix = [[1, 2]]
    matrix[0][1] = 42
    assert(matrix[0][1] == 42)
    let alias = ref items
    alias[0].number = 42
    assert(items[0].number == 42)
    42
}
"#,
        Ok("42"),
    );
}

#[test]
fn nested_indexed_writes_preserve_iterator_snapshots() {
    check(
        "nested-index-snapshot",
        r#"
import std.iter
import core.option
import core.string
type Item = { text: String, number: Int }
impl Item { func copy(self) -> self { Item { text: self.text.copy(), number: self.number } } }
func rename[g: group List<Item>](items: ref[g] List<Item>) -> () [mut g] {
    items[0].text = "after"
    ()
}
func main() -> Bool {
    let items = [Item { text: "before", number: 1 }]
    let snapshot = items.iterator()
    items[0].text = "after"
    assert(items[0].text == "after")
    let passed = [Item { text: "before", number: 2 }]
    let passed_snapshot = passed.iterator()
    rename(ref passed)
    assert(passed[0].text == "after")
    assert(branch passed_snapshot.next() { Option.Some(item) -> item.text == "before"
 _ -> false })
    branch snapshot.next() { Option.Some(item) -> item.text == "before"
 _ -> false }
}
"#,
        Ok("true"),
    );
}

#[test]
fn nested_indexed_writes_evaluate_rhs_then_each_index_once() {
    check_stdout(
        "nested-index-order",
        r#"
type Item = { number: Int }
func index(label: String) -> Int { println(label)
0 }
func value() -> Int { println("rhs")
42 }
func main() -> Int {
    let items = [[Item { number: 0 }]]
    items[index("outer")][index("inner")].number = value()
    items[0][0].number
}
"#,
        "rhs\nouter\ninner\n42",
    );
}

#[test]
fn nested_indexed_writes_replace_owned_fields_and_drop_them_once() {
    check_stdout(
        "nested-index-drop",
        r#"
import core.drop
type Resource = & Drop & { id: Int }
impl Resource { func deinit(self) -> () { println(self.id) } }
type Item = { resource: Resource }
func main() -> Int {
    let items = [Item { resource: Resource { id: 1 } }]
    items[0].resource = Resource { id: 2 }
    println(10)
    42
}
"#,
        "1\n10\n2\n42",
    );
}

#[test]
fn nested_indexed_writes_keep_bounds_checks_and_failure_order() {
    for (outer, inner, trace) in [
        (0, -1, "rhs\nouter\ninner"),
        (0, 1, "rhs\nouter\ninner"),
        (1, 0, "rhs\nouter"),
    ] {
        let source = format!(
            r#"
type Item = {{ number: Int }}
func index(label: String, value: Int) -> Int {{ println(label)
value }}
func value() -> Int {{ println("rhs")
42 }}
func main() -> Int {{
    let items = [[Item {{ number: 0 }}]]
    items[index("outer", {outer})][index("inner", {inner})].number = value()
    0
}}
"#
        );
        check_process_output("nested-index-bounds", &source, trace, Some("index"));
    }
}

#[test]
fn nested_indexed_writes_support_generic_and_callable_fields() {
    check(
        "nested-index-generic",
        r#"
type Box<T> = { value: T }
func set<T>[g: group List<Box<T>>](items: ref[g] List<Box<T>>, value: T) -> () [mut g, consume value] { items[0].value = value
() }
func first(value: Int) -> Int { value }
func second(value: Int) -> Int { value + 1 }
func main() -> Int {
    let items = [Box { value: 0 }]
    set(ref items, 41)
    assert(items[0].value == 41)
    let words = [Box { value: "before" }]
    set(ref words, "after")
    assert(words[0].value == "after")
    let callbacks = [Box { value: first }]
    callbacks[0].value = second
    callbacks[0].value(items[0].value)
}
"#,
        Ok("42"),
    );
}

#[test]
fn nested_indexed_writes_preserve_disjoint_loans() {
    check(
        "nested-index-disjoint",
        r#"
type Item = { left: List<Int>, right: List<Int> }
func main() -> Int {
    let items = [Item { left: [10], right: [20] }]
    let selected = ref items[0].left[0]
    items[0].right = [42]
    assert(items[0].right[0] == 42)
    selected
}
"#,
        Ok("10"),
    );
}

#[test]
fn indexed_references_return_as_values() {
    check(
        "indexed-reference-results",
        r#"
type Item = { left: List<Int>, right: List<Int> }
func integer() -> Int {
    let items = [Item { left: [42], right: [20] }]
    let selected = ref items[0].left[0]
    selected
}
func boolean() -> Bool {
    let values = [true]
    let selected = ref values[0]
    return selected
}
func floating() -> Float {
    let values = [1.5]
    let selected = ref values[0]
    selected
}
func main() -> Int {
    assert(boolean())
    assert(floating() == 1.5)
    integer()
}
"#,
        Ok("42"),
    );
}

#[test]
fn indexed_reference_results_cannot_escape_local_managed_origins() {
    let error = foster::compile(
        r#"
func text() -> String {
    let values = ["returned text"]
    let selected = ref values[0]
    selected
}

func main() -> String { text() }
"#,
    )
    .unwrap_err();
    assert_eq!(error.code.as_deref(), Some("E0402"), "{error}");
}

#[test]
fn explicit_library_contracts_dispatch_on_both_backends() {
    check(
        "explicit-library-contracts",
        r#"
import core.result
import core.byte
import core.bytes.buffer
type ListView = & List<Int> & {}
type TextView = & String & {}
type BufferView = & ByteBuffer & {}
func inspect(values: ListView) -> Int {
    let mapped = values.map((value: Int) -> value + 1)
    mapped.at(0).unwrap_or(0)
}
func text_size(text: TextView) -> Int { text.trim().length }
func fill(builder: BufferView) -> Int {
    builder.push(Byte.unchecked(42))
    builder.length()
}
func main() -> Int { inspect([34]) + text_size(" answer ") + fill(ByteBuffer.empty()) }
"#,
        Ok("42"),
    );
}

#[test]
fn cursors_preserve_checkpoints_and_unicode_boundaries() {
    check(
        "cursor",
        include_str!("fixtures/programs/cursor.fos"),
        Ok("42"),
    );
}

#[test]
fn generic_cursor_dispatch_preserves_reader_state() {
    check(
        "cursor-dispatch",
        include_str!("fixtures/programs/cursor_dispatch.fos"),
        Ok("42"),
    );
}

#[test]
fn user_record_names_do_not_select_builtin_representations() {
    check(
        "core-name-collisions",
        include_str!("fixtures/programs/core_name_collisions.fos"),
        Ok("42"),
    );
}
