//! The same source and expectations must hold for both execution engines and optimization modes.
use foster::{native, vm};

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
            "native {name}: {}",
            String::from_utf8_lossy(&output.stderr)
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
    let prepared = native::prepare(&compilation).unwrap();
    let scratch = Scratch::new(name);
    for optimize in [false, true] {
        let result = vm::run_with_options(&compilation, vm::CompileOptions { optimize });
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
    assert(text == "after")
    let values = [0, 2]
    set(ref values[0], 40)
    assert(values == [40, 2])
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
fn discarded_remote_failures_do_not_terminate_the_application() {
    check(
        "discarded-remote-failure",
        r#"
type Worker = {}
impl Worker {
    func fail(self) { assert(false) }
}
func main() -> Int {
    let worker = remote Worker {}
    worker.fail()
    42
}
"#,
        Ok("42"),
    );
}

#[test]
fn remote_runtime_failures_stop_nested_execution() {
    for (name, result_type, expression) in [
        ("overflow", "Int", "9223372036854775807 + value"),
        ("division", "Int", "value / 0"),
        ("shift", "Byte", "Byte.unchecked(value) << 8"),
        ("bounds", "Int", "[value][9]"),
        ("text-head", "CodePoint", "\"\".head"),
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
