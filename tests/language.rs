use foster::vm::Value;

fn assert_string(value: Value, expected: &str) {
    assert_eq!(value.as_string(), Some(expected));
}

#[test]
fn try_unwraps_success_and_returns_errors_early() {
    let source = r#"
import core.result

func operation(fail: Bool) -> Result<Int, Int> {
    branch fail {
        true -> Result.Error(7)
        _ -> Result.Ok(21)
    }
}

func propagate(fail: Bool) -> Result<Bool, Int> {
    let value = try operation(fail)
    Result.Ok(value == 21)
}

func main() -> Result<Bool, Int> { propagate(false) }
"#;

    for optimize in [false, true] {
        let success =
            foster::run_with_options(source, foster::vm::CompileOptions { optimize }).unwrap();
        let Value::Variant {
            alternative,
            payload,
            ..
        } = success
        else {
            panic!("try success returned a non-Result value");
        };
        assert_eq!(alternative.as_ref(), "Ok");
        assert_eq!(payload, vec![Value::Bool(true)]);

        let failure_source = source.replace("propagate(false)", "propagate(true)");
        let failure =
            foster::run_with_options(&failure_source, foster::vm::CompileOptions { optimize })
                .unwrap();
        let Value::Variant {
            alternative,
            payload,
            ..
        } = failure
        else {
            panic!("try failure returned a non-Result value");
        };
        assert_eq!(alternative.as_ref(), "Error");
        assert_eq!(payload, vec![Value::Integer(7)]);
    }
}

#[test]
fn try_requires_compatible_result_types() {
    let non_result = r#"
import core.result
func checked() -> Result<Int, String> { Result.Ok(try 1) }
func main() -> () { () }
"#;
    let error = foster::compile(non_result).unwrap_err();
    assert!(
        error.message.contains("requires a Result value"),
        "{error:?}"
    );

    let non_result_function = r#"
import core.result
func operation() -> Result<Int, String> { Result.Ok(1) }
func checked() -> Int { try operation() }
func main() -> () { () }
"#;
    let error = foster::compile(non_result_function).unwrap_err();
    assert!(
        error
            .message
            .contains("enclosing function to return Result"),
        "{error:?}"
    );

    let mismatched_error = r#"
import core.result
func operation() -> Result<Int, String> { Result.Ok(1) }
func checked() -> Result<Int, Bool> { Result.Ok(try operation()) }
func main() -> () { () }
"#;
    let error = foster::compile(mismatched_error).unwrap_err();
    assert!(error.message.contains("same error type"), "{error:?}");
}

#[test]
fn assertions_stop_the_current_invocation_with_an_optional_message() {
    let passing = r#"
func checked(value: Int) -> Int {
    assert(value > 0)
    value + 1
}

func main() -> Int { checked(41) }
"#;
    assert_eq!(foster::run(passing).unwrap(), Value::Integer(42));

    let failing = r#"
func checked() -> Int {
    assert(false, "expected a positive value")
    [1][4]
}

func main() -> Int { checked() }
"#;
    let error = foster::run(failing).unwrap_err();
    assert_eq!(error.message, "assertion failed: expected a positive value");

    let condition = foster::compile("func main() -> () { assert(1) }").unwrap_err();
    assert!(
        condition.message.contains("type mismatch"),
        "{}",
        condition.message
    );

    let message = foster::compile("func main() -> () { assert(false, 1) }").unwrap_err();
    assert!(
        message.message.contains("type mismatch"),
        "{}",
        message.message
    );
}

#[test]
fn loops_support_nearest_break_and_continue_with_postfix_guards() {
    let source = r#"
func main() -> Int {
    let outer = 0
    let score = 0
    loop {
        outer = outer + 1
        continue if outer < 3
        let inner = 0
        loop {
            inner = inner + 1
            continue if inner < 2
            score = score + outer + inner
            break
        }
        break if outer == 4
    }
    score
}
"#;

    for optimize in [false, true] {
        assert_eq!(
            foster::run_with_options(source, foster::vm::CompileOptions { optimize }).unwrap(),
            Value::Integer(11)
        );
    }
}

#[test]
fn loop_transfers_require_an_enclosing_loop_and_boolean_guards() {
    for (keyword, source) in [
        ("break", "func main() -> () { break }"),
        ("continue", "func main() -> () { continue }"),
    ] {
        let error = foster::compile(source).unwrap_err();
        assert!(
            error.message.contains("may only appear inside"),
            "{keyword}: {error:?}"
        );
    }

    for source in [
        "func main() -> () { loop { break if 1 } }",
        "func main() -> () { loop { continue if \"no\" } }",
    ] {
        let error = foster::compile(source).unwrap_err();
        assert!(error.message.contains("type mismatch"), "{error:?}");
    }

    let error = foster::compile("func main() -> Int { loop { break } }").unwrap_err();
    assert!(error.message.contains("type mismatch"), "{error:?}");
}

#[test]
fn branch_arms_support_statement_blocks() {
    let source = r#"
enum Choice = First
    | Second

func main() -> Int {
    let conditional = branch {
        true -> {
            let increment = 10
            increment + 11
        }
        _ -> 0
    }
    let matched = branch Choice.First {
        Choice.First -> {
            let increment = 20
            increment + 1
        }
        Choice.Second -> 0
        _ -> {
            0
        }
    }
    conditional + matched
}
"#;

    for optimize in [false, true] {
        assert_eq!(
            foster::run_with_options(source, foster::vm::CompileOptions { optimize }).unwrap(),
            Value::Integer(42)
        );
    }
}

#[test]
fn wildcard_branch_arms_do_not_reach_later_tests() {
    let source = r#"
enum Choice = First
    | Second

func must_not_run() -> Bool {
    assert(false, "a test after a wildcard arm ran")
    true
}

func main() -> Int {
    let conditional = branch {
        _ -> 20
        must_not_run() -> 0
    }
    let matched = branch Choice.First {
        _ -> 22
        Choice.First -> 0
    }
    conditional + matched
}
"#;

    for optimize in [false, true] {
        assert_eq!(
            foster::run_with_options(source, foster::vm::CompileOptions { optimize }).unwrap(),
            Value::Integer(42)
        );
    }
}

#[test]
fn branch_arm_blocks_require_a_result_and_continue_requires_a_loop() {
    let missing_result = r#"
func main() -> Int {
    branch {
        true -> {
            let value = 1
        }
        _ -> 0
    }
}
"#;
    let error = foster::compile(missing_result).unwrap_err();
    assert!(error.message.contains("must end with a value"), "{error:?}");

    let no_loop = r#"
func main() -> Int {
    branch {
        _ -> {
            continue
        }
    }
}
"#;
    let error = foster::compile(no_loop).unwrap_err();
    assert!(error.message.contains("inside `loop`"), "{error:?}");
}

#[test]
fn continue_inside_a_branch_targets_the_enclosing_loop() {
    let source = r#"
func main() -> Int {
    let rounds = 0
    loop {
        rounds = rounds + 1
        let selected = branch {
            rounds < 3 -> { continue }
            _ -> rounds
        }
        break
    }

    let iterations = branch {
        _ -> {
            let count = 0
            loop {
                count = count + 1
                continue if count < 2
                break
            }
            count
        }
    }
    rounds + iterations
}
"#;

    for optimize in [false, true] {
        assert_eq!(
            foster::run_with_options(source, foster::vm::CompileOptions { optimize }).unwrap(),
            Value::Integer(5)
        );
    }
}

#[test]
fn command_main_receives_the_typed_arguments_record() {
    let source = r#"
import std.process

func main(arguments: Arguments) -> String {
    return arguments.executable if arguments.values.empty?
    return "two" if arguments.values.length == 2
    arguments.values[0]
}
"#;
    let arguments = foster::entry::CommandArguments::new("foster-test", ["left", "right"]);
    assert_string(
        foster::run_with_arguments(source, &arguments).unwrap(),
        "two",
    );
    assert_string(
        foster::run_with_arguments(
            source,
            &foster::entry::CommandArguments::new("foster-test", ["only"]),
        )
        .unwrap(),
        "only",
    );
}

#[test]
fn command_main_rejects_non_arguments_parameters() {
    for source in [
        "func main(value: Int) { value }",
        "func main(left: String, right: String) { left + right }",
        "type Arguments = { values: List<String> }\nfunc main(value: Arguments) { value.values }",
    ] {
        let error = foster::compile(source).unwrap_err();
        assert!(
            error.message.contains(
                "`main` must take no parameters or one `std.process.Arguments` parameter"
            ),
            "{error:?}"
        );
        assert_eq!(error.code.as_deref(), Some("E0901"));
        assert!(
            error
                .help
                .as_deref()
                .is_some_and(|help| help.contains("std.process"))
        );
    }
}

#[test]
fn test_declarations_compile_as_isolated_unit_functions() {
    let source = r#"
test "empty list has zero length" {
    let values = [1]
    values.length
    println()
}

test "another case" {}
"#;
    let parsed = foster::parse(source).unwrap();
    assert_eq!(parsed.tests.len(), 2);
    assert_eq!(parsed.tests[0].description, "empty list has zero length");

    let compilation = foster::compile(source).unwrap();
    assert_eq!(compilation.hir.tests.len(), 2);
    assert!(
        compilation.hir.modules[compilation.hir.module_named("main").unwrap()]
            .functions
            .values()
            .all(|function| !compilation.hir.tests.contains(function))
    );
    let program = foster::vm::compile(&compilation).unwrap();
    let machine = foster::vm::Machine::new(&program);
    for test in &compilation.hir.tests {
        assert_eq!(machine.run_function(*test).unwrap(), Value::Unit);
    }
}

#[test]
fn test_declarations_require_descriptions_and_unit_results() {
    let empty = foster::parse("test \"\" {}").unwrap_err();
    assert!(empty.message.contains("description cannot be empty"));

    let non_unit = foster::compile("test \"returns a value\" { 42 }").unwrap_err();
    assert!(
        non_unit.message.contains("expected `()`"),
        "{}",
        non_unit.message
    );
}

#[test]
fn guard_return_and_implicit_result() {
    let source = r#"
func first(characters: List<String>) -> String {
    return "" if characters.empty?
    characters.head
}

func main() {
    first(["F", "o"])
}
"#;
    assert_string(foster::run(source).unwrap(), "F");
}

#[test]
fn strings_use_the_foster_record_representation() {
    let source = r#"func main() -> String { "Foster λ" }"#;
    let compilation = foster::compile(source).unwrap();
    let main = compilation.hir.module_named("main").unwrap();
    let main = compilation.hir.function_named(main, "main").unwrap();
    let result = compilation.types.function_type(main).unwrap().result;
    let string_module = compilation.hir.module_named("core.string").unwrap();
    let string_record = compilation
        .hir
        .record_named(string_module, "String")
        .unwrap();
    assert!(matches!(
        compilation.types.types[result],
        foster::types::Type::Record { record, ref arguments }
            if record == string_record && arguments.is_empty()
    ));

    let Value::Record {
        record,
        name,
        fields,
    } = foster::run(source).unwrap()
    else {
        panic!("String did not use its Foster record representation")
    };
    assert_eq!(record, Some(string_record));
    assert_eq!(name, "String");
    assert_eq!(fields["value"].as_bytes(), Some("Foster λ".as_bytes()));

    let error = foster::compile(r#"func main() -> Bytes { "private".value }"#).unwrap_err();
    assert!(
        error.message.contains("field `String.value` is private"),
        "{}",
        error.message
    );
}

#[test]
fn symbols_and_bytes_use_foster_type_representations() {
    let compilation = foster::compile(
        r#"
func symbol() -> Symbol { :ready }
func bytes() -> Bytes { "data".utf8 }
func main() -> Symbol { symbol() }
"#,
    )
    .unwrap();
    let main = compilation.hir.module_named("main").unwrap();
    for (function_name, module_name, type_name) in [
        ("symbol", "core.symbol", "Symbol"),
        ("bytes", "core.bytes", "Bytes"),
    ] {
        let function = compilation.hir.function_named(main, function_name).unwrap();
        let result = compilation.types.function_type(function).unwrap().result;
        let module = compilation.hir.module_named(module_name).unwrap();
        let expected = compilation.hir.record_named(module, type_name).unwrap();
        assert!(matches!(
            compilation.types.types[result],
            foster::types::Type::Record { record, ref arguments }
                if record == expected && arguments.is_empty()
        ));
    }
    assert_eq!(
        foster::run("func main() { :ready }").unwrap().as_symbol(),
        Some("ready")
    );
    assert_eq!(
        foster::run("func main() { \"data\".utf8 }")
            .unwrap()
            .as_bytes(),
        Some(b"data".as_slice())
    );
    let error = foster::compile("func main(value: RawBytes) { value }").unwrap_err();
    assert!(
        error.message.contains("unknown type `RawBytes`"),
        "{}",
        error.message
    );
}

#[test]
fn lists_and_byte_buffers_use_foster_type_representations() {
    let compilation = foster::compile(
        r#"
import core.bytes.buffer as byte_buffer

func list() -> List<Int> { [1, 2, 3] }
func buffer() -> ByteBuffer { ByteBuffer.empty() }
func main() -> List<Int> { list() }
"#,
    )
    .unwrap();
    let main = compilation.hir.module_named("main").unwrap();
    for (function_name, module_name, type_name, argument_count) in [
        ("list", "core.list", "List", 1),
        ("buffer", "core.bytes.buffer", "ByteBuffer", 0),
    ] {
        let function = compilation.hir.function_named(main, function_name).unwrap();
        let result = compilation.types.function_type(function).unwrap().result;
        let module = compilation.hir.module_named(module_name).unwrap();
        let expected = compilation.hir.record_named(module, type_name).unwrap();
        assert!(matches!(
            compilation.types.types[result],
            foster::types::Type::Record { record, ref arguments }
                if record == expected && arguments.len() == argument_count
        ));
    }

    let value = foster::run("func main() { [1, 2, 3] }").unwrap();
    assert_eq!(
        value.as_list().unwrap(),
        &[Value::Integer(1), Value::Integer(2), Value::Integer(3)]
    );
    for raw in ["RawList<Int>", "RawByteBuffer"] {
        let error = foster::compile(&format!("func main(value: {raw}) {{ value }}")).unwrap_err();
        assert!(
            error.message.contains(&format!("unknown type `{raw}`"))
                || error.message.contains("unknown type `RawList`"),
            "{}",
            error.message
        );
    }

    let error = foster::compile(
        "func mystery() -> Int = intrinsic(\"unknown.operation\")\nfunc main() { 0 }",
    )
    .unwrap_err();
    assert!(
        error
            .message
            .contains("intrinsic key `unknown.operation` has no registered runtime implementation"),
        "{}",
        error.message
    );
}

#[test]
fn parses_stable_intrinsic_keys_and_opaque_intrinsic_types() {
    let program = foster::parse(
        "intrinsic type HostValue\nfunc HostValue.create() -> HostValue = intrinsic(\"host.create\")",
    )
    .unwrap();
    assert!(program.records[0].intrinsic);
    assert_eq!(
        program.functions[0].intrinsic.as_deref(),
        Some("host.create")
    );
}

#[test]
fn postfix_guards_conditionally_transfer_control() {
    let source = r#"
func choose(early: Bool) -> Int {
    return 10 if early
    20
}

func main() -> Int {
    choose(true) + choose(false)
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(30));
}

#[test]
fn enum_declarations_construct_and_match_tagged_cases() {
    let source = r#"
enum Value = Int(Int) | String(String) | List(List<Value>)

func wrap(value: Int) -> Value { Value.Int(value) }

func main() -> Int {
    branch wrap(42) {
        Int(value) -> value
        String(_) -> 0
        List(_) -> 0
    }
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));
}

#[test]
fn enum_cases_are_labels_with_explicit_payload_types() {
    let source = r#"
type Bar = { value: Int }

enum Foo = Bar
    | FooBar(String)

func describe(value: Foo) -> Int {
    branch value {
        Foo.Bar -> 0
        Foo.FooBar(text) -> text.length
    }
}

func main() -> Int { describe(Foo.FooBar("Foster")) }
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(6));
}

#[test]
fn enum_cases_carry_at_most_one_payload_type() {
    let error = foster::compile("enum Pair = Together(Int, String)\n").unwrap_err();
    assert!(
        error
            .message
            .contains("an enum case carries one payload type"),
        "{}",
        error.message
    );
}

#[test]
fn enum_values_require_an_explicit_constructor() {
    let error = foster::compile(
        r#"
enum Choice = Integer(Int) | Text(String)

func invalid() -> Choice { 42 }
"#,
    )
    .unwrap_err();
    assert!(error.message.contains("type mismatch"), "{}", error.message);
}

#[test]
fn single_member_type_declarations_accept_their_member_type() {
    let source = r#"
type foo = String

func length(value: foo) -> Int {
    value.length
}

func main() -> Int { length("Foster") }
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(6));
}

#[test]
fn generic_single_member_type_declarations_are_transparent_aliases() {
    let source = r#"
type items<T> = List<T>

func length(values: items<String>) -> Int { values.length }

func main() -> Int { length(["Foster", "language"]) }
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(2));
}

#[test]
fn recursive_type_aliases_are_rejected() {
    let error = foster::compile("type left = right\ntype right = left\n").unwrap_err();
    assert!(
        error.message.contains("recursively refers to itself"),
        "{}",
        error.message
    );
}

#[test]
fn reusable_function_aliases_preserve_callable_ownership() {
    let source = r#"
import core.functions

type Payload = { value: Int }

func matches<T>(value: T, predicate: Predicate<T>) -> Bool { predicate(value) }

func accept<T>(value: T, consumer: Consumer<T>) -> () [consume value] {
    consumer(move value)
}

func provide<T>(supplier: Supplier<T>) -> T { supplier() }

func discard(value: Payload) -> () [consume value] {}

func main() -> Int {
    let payload = Payload { value: 42 }
    accept(move payload, discard)
    branch {
        matches(42, (value: Int) -> value > 40) -> provide(() -> 42)
        _ -> 0
    }
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));
}

#[test]
fn union_declarations_separate_member_types_with_inline_pipes() {
    let source = r#"
type scalar = String | Int

func size(value: scalar) -> Int {
    1
}

func main() -> Int { size("Foster") + size(4) }
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(2));
}

#[test]
fn union_contracts_accept_any_member_without_runtime_construction() {
    let source = r#"
type Bar = { value: Int }
type Trigger = { value: Int }
type foo = List<Bar>
type fod = List<Trigger>
type X = foo | fod
type Direct = List<Bar> | List<Trigger>

func bars() -> foo { [] }
func triggers() -> fod { [] }
func empty() -> X { [] }
func direct_empty() -> Direct { [] }

func length(value: X) -> Int {
    value.length
}

func main() -> Int {
    length(bars()) + length(triggers()) + length(empty()) + direct_empty().length
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(0));
}

#[test]
fn union_contracts_do_not_synthesize_constructors() {
    let error = foster::compile(
        r#"
type Bar = { value: Int }
type foo = List<Bar>
type X = foo | String

func invalid(value: foo) -> X { X.foo(value) }
"#,
    )
    .unwrap_err();
    assert!(
        error.message.contains("has no constructors"),
        "{}",
        error.message
    );
}

#[test]
fn union_contracts_widen_when_each_source_member_satisfies_the_target() {
    let source = r#"
type Small = String | Int
type Wide = String | Int | Float

func choose(flag: Bool) -> Small {
    branch {
        flag -> "Foster"
        _ -> 42
    }
}

func widen(value: Small) -> Wide { value }

func main() -> Int {
    let widened = widen(choose(true))
    0
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(0));
}

#[test]
fn union_declarations_reject_a_leading_pipe() {
    let error = foster::compile("type Value = | String | Int").unwrap_err();
    assert!(
        error.message.contains("remove the leading `|`"),
        "{}",
        error.message
    );
}

#[test]
fn enum_cases_carry_record_values_without_expanding_their_fields() {
    let source = r#"
type Boxed = { value: Int }

enum Value = Boxed(Boxed)
    | String(String)

func wrap(value: Boxed) -> Value { Value.Boxed(move value) }

func main() -> Int {
    let boxed = Boxed { value: 42 }
    branch wrap(move boxed) {
        Boxed(value) -> value.value
        String(_) -> 0
    }
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));
}

#[test]
fn enum_construction_preserves_builtin_record_values() {
    let source = r#"
enum Value = String(String)
    | Int(Int)

func wrap(value: String) -> Value { Value.String(value) }

func main() -> Int {
    branch wrap("Foster") {
        String(value) -> value.length
        Int(value) -> value
    }
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(6));
}

#[test]
fn expected_enum_types_flow_into_branches_and_lists() {
    let source = r#"
enum Value = Int(Int)
    | String(String)

func choose(number: Bool) -> Value {
    branch {
        number -> Value.Int(7)
        _ -> Value.String("Foster")
    }
}

func first(values: List<Value>) -> Int {
    branch values[0] {
        Int(value) -> value
        String(value) -> value.length
    }
}

func main() -> Int {
    first([choose(true), Value.String("language")])
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(7));
}

#[test]
fn rejects_positional_payload_syntax_in_union_declarations() {
    let error = foster::compile(
        r#"
type Value =
List<Value>
| Table(List<Value>)
"#,
    )
    .unwrap_err();
    assert!(
        error.message.contains("union members are complete types"),
        "{}",
        error.message
    );
}

#[test]
fn postfix_guard_falls_through_to_a_parameter_result() {
    let source = r#"
func either(left: Bool, right: Bool) -> Bool {
    return true if left
    right
}

func main() -> Bool {
    either(false, false)
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Bool(false));
}

#[test]
fn postfix_guards_require_boolean_conditions() {
    let error = foster::compile("func main() -> Int { return 1 if 42\n0 }").unwrap_err();
    assert!(error.message.contains("Bool"), "{}", error.message);
}

#[test]
fn postfix_guards_only_apply_to_control_statements() {
    let expression = foster::compile("func main() { println() if true }").unwrap_err();
    assert_eq!(
        expression.message,
        "postfix `if` may only guard a control statement"
    );

    let binding = foster::compile("func main() { value = 1 if true\nvalue }").unwrap_err();
    assert_eq!(
        binding.message,
        "postfix `if` may only guard a control statement"
    );
}

#[test]
fn branch_and_recursion() {
    let source = include_str!("../examples/whitespace.fos");
    assert_string(foster::run(source).unwrap(), "Foster");
}

#[test]
fn symbols_and_arithmetic() {
    let source = r#"
func choose(value: Int) {
    branch {
        value > 10 -> :large
        _ -> :small
    }
}

func main() { choose(6 * 2) }
"#;
    assert_eq!(foster::run(source).unwrap().as_symbol(), Some("large"));
}

#[test]
fn conditional_branches_require_a_wildcard_arm() {
    let missing = foster::compile("func main() { branch { true -> 1 } }").unwrap_err();
    assert!(missing.message.contains("requires a `_` arm"));

    let legacy = foster::compile("func main() { branch { true -> 1 else -> 0 } }").unwrap_err();
    assert!(legacy.message.contains("expected expression"));
}

#[test]
fn runs_newly_unblocked_pima_ports() {
    assert_eq!(
        foster::run(include_str!("../examples/pima/curried_example.fos")).unwrap(),
        Value::Integer(19)
    );
    let Value::Float(root) = foster::run(include_str!("../examples/pima/newton.fos")).unwrap()
    else {
        panic!("Newton example should return Float")
    };
    assert!((root - 4.0).abs() < 0.001);
    assert_eq!(
        foster::run(include_str!("../examples/pima/birthday_paradox.fos")).unwrap(),
        Value::Float(23.0)
    );
}

#[test]
fn declarations_are_private_by_default_across_public_modules() {
    let error = foster::check_package("tests/fixtures/private_function").unwrap_err();
    assert!(
        error
            .message
            .contains("function `library.hidden` is private")
    );
}

#[test]
fn constructs_reads_and_mutates_nominal_records() {
    let source = r#"
pub type Person = {
    pub name: String
    pub age: Int
    internal_id: Int
}

func main() -> Int {
    let name = "Ada"
    let person = Person {
        name
        age: 37
        internal_id: 104
    }
    person.age = person.age + 1
    person.age
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(38));
}

#[test]
fn infers_generic_record_arguments() {
    let source = r#"
type Parsed<T> = {
    value: T
    remaining: String
}

func parse() -> Parsed<Int> {
    Parsed {
        value: 42
        remaining: ""
    }
}

func main() -> Int {
    parse().value
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));
}

#[test]
fn calls_functions_associated_with_record_types() {
    let source = r#"
type Box<T> = { value: T }

func Box.create<T>(value: T) -> Box<T> {
    Box { value }
}

func main() -> Int {
    Box.create(42).value
}
"#;
    let compilation = foster::compile(source).unwrap();
    let module = compilation.hir.module_named("main").unwrap();
    assert!(
        compilation
            .hir
            .function_named(module, "Box.create")
            .is_some()
    );
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));

    let unknown = foster::compile("func Missing.create() { 1 }\nfunc main() { 0 }").unwrap_err();
    assert!(unknown.message.contains("unknown record type `Missing`"));

    let methods = r#"
type Left = { value: Int }
type Right = { value: Int }
func Left.read(self: Left) -> Int { self.value }
func Right.read(self: Right) -> Int { self.value + 1 }
func main() -> Int { Left { value: 20 }.read() + Right { value: 20 }.read() }
"#;
    let compilation = foster::compile(methods).unwrap();
    let module = compilation.hir.module_named("main").unwrap();
    let left_read = compilation.hir.function_named(module, "Left.read").unwrap();
    assert_eq!(
        compilation.hir.functions[left_read].owner.as_deref(),
        Some("Left")
    );
    assert!(compilation.hir.functions[left_read].receiver.is_some());
    assert_eq!(foster::run(methods).unwrap(), Value::Integer(41));

    let bare = foster::compile(
        "type Box = { value: Int }\nfunc read(self: Box) { self.value }\nfunc main() { 0 }",
    )
    .unwrap_err();
    assert!(bare.message.contains("must qualify its name"));

    let misplaced =
        foster::compile("func read(value: Int, self: Int) {}\nfunc main() { 0 }").unwrap_err();
    assert!(misplaced.message.contains("must be the first parameter"));

    let mismatch = foster::compile(
        "type Box = {}\ntype Other = {}\nfunc Box.read(self: Other) {}\nfunc main() { 0 }",
    )
    .unwrap_err();
    assert!(
        mismatch
            .message
            .contains("owned by `Box` but receives `Other`")
    );
}

#[test]
fn owner_qualified_methods_do_not_reserve_builtin_member_names() {
    let source = r#"
type Bucket = { base: Int }
type Text = { base: Int }
type Matcher = { base: Int }

func Bucket.push(self: Bucket, value: Int) -> Int { self.base + value }
func Text.append(self: Text, value: Int) -> Int { self.base * value }
func Matcher.in?(self: Matcher, value: Int) -> Int { self.base - value }

func main() -> Int {
    let pushed = Bucket { base: 10 }.push(2)
    let appended = Text { base: 5 }.append(3)
    let matched = Matcher { base: 20 }.in?(4)
    pushed + appended + matched
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(43));
}

#[test]
fn zero_argument_methods_require_call_parentheses() {
    let source = r#"
type Counter = { value: Int }

func Counter.read(self: Counter) -> Int { self.value }

func main() {
    let value = Counter { value: 42 }.read
    ()
}
"#;
    let error = foster::compile(source).unwrap_err();
    assert!(
        error
            .message
            .contains("method `read` must be called with parentheses"),
        "{}",
        error.message
    );

    let called = source.replace(
        "func main() {\n    let value = Counter { value: 42 }.read\n    ()\n}",
        "func main() -> Int { Counter { value: 42 }.read() }",
    );
    assert_eq!(foster::run(&called).unwrap(), Value::Integer(42));
}

#[test]
fn list_operations_resolve_through_owner_qualified_methods() {
    let source = r#"
import core.list

func main() -> Int {
    let values = [1]
    values.push(2)
    let pushed_length = values.length
    let extended = List.append(move values, 3)
    branch {
        extended.contains?(3) -> pushed_length * 10 + extended.length
        _ -> 0
    }
}
"#;
    for optimize in [false, true] {
        assert_eq!(
            foster::run_with_options(source, foster::vm::CompileOptions { optimize }).unwrap(),
            Value::Integer(23)
        );
    }
}

#[test]
fn associated_functions_construct_private_record_representations() {
    assert_eq!(
        foster::run_package("tests/fixtures/associated_function").unwrap(),
        Value::Integer(42)
    );
}

#[test]
fn rejects_incomplete_and_duplicate_record_initialization() {
    let missing = foster::compile(
        r#"
type Pair = { left: Int, right: Int }
func main() { Pair { left: 1 } }
"#,
    )
    .unwrap_err();
    assert!(missing.message.contains("missing field(s): right"));

    let duplicate = foster::compile(
        r#"
type Pair = { left: Int, right: Int }
func main() { Pair { left: 1, left: 2, right: 3 } }
"#,
    )
    .unwrap_err();
    assert!(
        duplicate
            .message
            .contains("field `left` is initialized twice")
    );
}

#[test]
fn enforces_record_and_field_visibility_across_modules() {
    let error = foster::check_package("tests/fixtures/record_privacy").unwrap_err();
    assert!(error.message.contains("field `Person.secret` is private"));
    foster::check_package("tests/fixtures/public_record").unwrap();
}

#[test]
fn rejects_private_types_in_public_signatures() {
    let source = r#"
type Secret = { value: Int }
pub func expose() -> Secret { Secret { value: 1 } }
"#;
    let error = foster::compile(source).unwrap_err();
    assert!(
        error
            .message
            .contains("public function `expose` exposes private type `Secret`")
    );
}

#[test]
fn mutable_ref_capture_can_update_record_fields() {
    let source = r#"
type Counter = { value: Int }

func main() -> Int {
    let counter = Counter { value: 0 }
    let increment = [ref counter] () -> {
        counter.value = counter.value + 1
    }
    increment()
    increment()
    counter.value
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(2));
}

#[test]
fn rejects_storing_a_borrower_into_its_own_origin() {
    let source = r#"
type Counter = {
    value: Int,
    callback: func() -> Int
}

func main() -> Int {
    let counter = Counter {
        value: 1,
        callback: () -> 0
    }
    let callback = [ref counter] () -> counter.value
    counter.callback = callback
    counter.value
}
"#;

    let error = foster::compile(source).unwrap_err();
    assert!(
        error
            .message
            .contains("cannot store a value borrowing `counter` into its own origin")
    );
}

#[test]
fn permits_storing_a_value_derived_from_a_borrower() {
    let source = r#"
type Counter = { value: Int }

func main() -> Int {
    let counter = Counter { value: 1 }
    let observe = [ref counter] () -> counter.value
    counter.value = observe()
    counter.value
}
"#;

    assert_eq!(foster::run(source).unwrap(), Value::Integer(1));
}

#[test]
fn constructs_and_exhaustively_matches_closed_variants() {
    let source = r#"
enum Result<T> = Ok(T)
    | Error(String)

func unwrap(result: Result<Int>) -> Int {
    branch result {
        Result.Ok(value) -> value
        Result.Error(message) -> 0
    }
}

func main() -> Int { unwrap(Result.Ok(42)) }
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));
}

#[test]
fn matches_payloadless_variants_and_wildcards() {
    let source = r#"
enum Option<T> = Some(T)
    | None

func present(value: Option<Int>) -> Bool {
    branch value {
        Option.Some(_) -> true
        Option.None -> false
    }
}

func main() -> Bool { present(Option.None) }
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Bool(false));
}

#[test]
fn rejects_non_exhaustive_variant_branches() {
    let error = foster::compile(
        r#"
enum Choice = Left(Int)
    | Right(Int)
func main() -> Int {
    let value = Choice.Left(1)
    branch value { Choice.Left(number) -> number }
}
"#,
    )
    .unwrap_err();
    assert!(error.message.contains("non-exhaustive branch on `Choice`"));
}

#[test]
fn refutable_payload_patterns_do_not_cover_an_entire_alternative() {
    let error = foster::compile(
        r#"
enum Option = Some(Int)
    | None

func main() -> Int {
    let value = Some(1)
    branch value {
        Some(0) -> 0
        Option.None -> -1
    }
}
"#,
    )
    .unwrap_err();
    assert!(error.message.contains("non-exhaustive branch on `Option`"));
}

#[test]
fn rejects_private_variants_in_public_apis() {
    let signature = foster::compile(
        r#"
type Hidden = { value: Int }
type Secret = Hidden
pub func expose() -> Secret { Hidden(1) }
"#,
    )
    .unwrap_err();
    assert!(
        signature
            .message
            .contains("public function `expose` exposes private type `Hidden`")
    );

    let payload = foster::compile(
        r#"
type Secret = { value: Int }
pub type Message = Secret
func main() { 0 }
"#,
    )
    .unwrap_err();
    assert!(
        payload
            .message
            .contains("public type alias `Message` exposes private type `Secret`"),
        "{}",
        payload.message
    );
}

fn json_parser_with_main(expression: &str) -> String {
    let parser = include_str!("../examples/pima/json_parser/parser.fos");
    format!("{parser}\nfunc main() {{ {expression} }}")
}

#[test]
fn runs_the_foster_json_parser() {
    let value = foster::run(&json_parser_with_main(
        r#"parse_json("{\"text\":\"\\uD83D\\uDE00\",\"values\":[true,null,2.5e1]}")"#,
    ))
    .unwrap();
    assert!(
        matches!(value, Value::Variant { ref type_name, ref alternative, .. } if type_name.as_ref() == "ParseResult" && alternative.as_ref() == "ParseOk")
    );
    assert!(value.to_string().contains("Json.JsonString("));
    assert!(value.to_string().contains("Json.Number(25)"));
}

#[test]
fn runs_the_json_actor_pipeline() {
    let value = foster::run_package("examples/pima/json_parser").unwrap();
    let Value::Record { name, fields, .. } = value else {
        panic!("pipeline should return a report record")
    };
    assert_eq!(name, "PipelineReport");
    assert_eq!(fields.get("processed"), Some(&Value::Integer(2)));
    assert_eq!(fields.get("failed"), Some(&Value::Integer(1)));
}

#[test]
fn json_parser_returns_typed_errors_for_malformed_input() {
    for document in [
        r#"[1,2,]"#,
        r#"{\"a\":1,}"#,
        r#"01"#,
        r#"-"#,
        r#"\"\\uDE00\""#,
    ] {
        let expression = format!("parse_json({document:?})");
        let value = foster::run(&json_parser_with_main(&expression)).unwrap();
        assert!(
            matches!(value, Value::Variant { ref type_name, ref alternative, .. } if type_name.as_ref() == "ParseResult" && alternative.as_ref() == "ParseError"),
            "expected typed error for {document}, got {value}"
        );
    }
}

#[test]
fn generic_functions_are_rigid_and_instantiate_per_call() {
    let source = r#"
func identity<T>(value: T) -> T [consume value] { value }

func main() -> String {
    let number = identity(42)
    identity("Foster")
}
"#;
    foster::compile(source).unwrap();
    assert_string(foster::run(source).unwrap(), "Foster");

    let error = foster::compile(
        r#"
func invalid<T>(value: T) -> T { value + 1 }
func main() { invalid(1) }
"#,
    )
    .unwrap_err();
    assert!(error.message.contains("type mismatch"));
}

#[test]
fn generic_syntax_uses_angles_while_indexing_uses_brackets() {
    let source = r#"
func first<T>(values: List<T>) -> T {
    values[0]
}

func main() -> Int {
    first([42])
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));

    let legacy = foster::compile(
        r#"
func identity[T](value: T) -> T { value }
func main() -> Int { identity(42) }
"#,
    );
    assert!(
        legacy.is_err(),
        "square-bracketed generics must be rejected"
    );
}

#[test]
fn rejects_duplicate_and_colliding_function_parameters() {
    let duplicate = foster::compile(
        r#"
func invalid<T, T>(value: T) -> T { value }
func main() { 0 }
"#,
    )
    .unwrap_err();
    assert!(
        duplicate
            .message
            .contains("declares type parameter `T` more than once")
    );

    let collision = foster::compile(
        r#"
func invalid<T>[T: group Int](value: T) -> T { value }
func main() { 0 }
"#,
    )
    .unwrap_err();
    assert!(
        collision
            .message
            .contains("uses `T` as both a type parameter and a group parameter")
    );
}

#[test]
fn checks_explicit_import_core_library_usage() {
    let compilation = foster::check_package("tests/fixtures/core_consumer").unwrap();
    assert!(compilation.package.module("core").unwrap().is_implicit());
    assert!(compilation.package.module("core.list").is_some());
    assert!(compilation.package.module("core.string").is_some());
    let string = compilation.hir.module_named("core.string").unwrap();
    let list = compilation.hir.module_named("core.list").unwrap();
    assert!(compilation.hir.function_named(string, "trim").is_some());
    assert!(compilation.hir.function_named(list, "flat_map").is_some());
}

#[test]
fn requires_qualification_for_ambiguous_imported_names() {
    let error = foster::check_package("tests/fixtures/import_ambiguity").unwrap_err();
    assert!(
        error
            .message
            .contains("imported name `map` is ambiguous; qualify it with its module")
    );
}

#[test]
fn borrows_arguments_by_default_and_requires_explicit_moves_for_consuming_calls() {
    foster::compile(
        r#"
func take(value: String) -> () { println() }
func main() -> String {
    let value = "owned"
    take(value)
    value
}
"#,
    )
    .unwrap();

    let missing_move = foster::compile(
        r#"
func take(value: String) -> () [consume value] { println() }
func main() -> () {
    let value = "owned"
    take(value)
}
"#,
    )
    .unwrap_err();
    assert!(
        missing_move
            .message
            .contains("pass this argument with `move`")
    );

    let moved = foster::compile(
        r#"
func take(value: String) -> () [consume value] { println() }
func main() -> String {
    let value = "owned"
    take(move value)
    value
}
"#,
    )
    .unwrap_err();
    assert!(
        moved
            .message
            .contains("value `value` is used after it was moved")
    );
    assert_eq!(moved.code.as_deref(), Some("E0382"));
    assert_eq!(moved.labels.len(), 3);
    assert!(moved.labels[0].primary);
    assert!(moved.labels[2].message.contains("ownership was moved"));
    assert!(
        moved
            .help
            .as_deref()
            .is_some_and(|help| help.contains("borrow"))
    );

    foster::compile(
        r#"
func take(value: Int) -> () [consume value] { println() }
func main() -> Int {
    let value = 42
    take(value)
    value
}
"#,
    )
    .unwrap();
}

#[test]
fn preserves_consuming_parameters_through_callable_values() {
    let missing_move = r#"
func main() -> () {
    let action = (message: String) -> [consume message] { println(message) }
    let message = "owned"
    action(message)
}
"#;
    let error = foster::compile(missing_move).unwrap_err();
    assert!(error.message.contains("pass this argument with `move`"));

    foster::compile(&missing_move.replace("action(message)", "action(move message)")).unwrap();
}

#[test]
fn preserves_consuming_parameters_through_partial_application() {
    let missing_move = r#"
func submit(message: String) -> () [consume message] {
    println(message)
}

func main() -> () {
    let action = submit(_)
    let message = "owned"
    action(message)
}
"#;
    let error = foster::compile(missing_move).unwrap_err();
    assert!(error.message.contains("pass this argument with `move`"));

    foster::compile(&missing_move.replace("action(message)", "action(move message)")).unwrap();

    let indirect = missing_move.replace(
        "let action = submit(_)",
        "let consumer = submit\n    let action = consumer(_)",
    );
    let error = foster::compile(&indirect).unwrap_err();
    assert!(error.message.contains("pass this argument with `move`"));
    foster::compile(&indirect.replace("action(message)", "action(move message)")).unwrap();
}

#[test]
fn expresses_consuming_parameters_in_callable_types() {
    let source = r#"
func sink(message: String) -> () [consume message] {
    println(message)
}

func invoke(action: func(consume String) -> (), message: String) -> () [consume message] {
    action(move message)
}

func main() -> () {
    invoke(sink, "owned")
}
"#;
    let compilation = foster::compile(source).unwrap();
    let main = compilation.hir.module_named("main").unwrap();
    let sink = compilation.hir.function_named(main, "sink").unwrap();
    assert_eq!(
        compilation
            .types
            .function_type(sink)
            .unwrap()
            .parameter_modes,
        vec![foster::ast::ParameterMode::Consume]
    );

    let incompatible = source.replace("func(consume String) -> ()", "func(String) -> ()");
    let error = foster::compile(&incompatible).unwrap_err();
    assert!(error.message.contains("callable contract is incompatible"));
}

#[test]
fn any_is_an_ordinary_identifier_not_a_language_keyword() {
    let source = r#"
func main() -> Int {
    let any = 42
    any
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));
}

#[test]
fn declared_type_composition_conforms_without_runtime_conversion() {
    let source = r#"
import core.string as strings

type TextSlice = & Sequence<CodePoint> & {
    text: String
}

func TextSlice.empty?(self: TextSlice) -> Bool { self.text.empty? }
func TextSlice.length(self: TextSlice) -> Int { self.text.length }
func TextSlice.head(self: TextSlice) -> CodePoint { self.text.head }
func TextSlice.rest(self: TextSlice) -> String { strings.slice(self.text, 1, self.text.length) }

func first(values: Sequence<CodePoint>) -> CodePoint {
    values.head()
}

func main() -> CodePoint {
    let value = TextSlice { text: "OK" }
    first(value)
    value.head()
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::CodePoint('O'));
}

#[test]
fn callable_contract_members_dispatch_through_structural_types() {
    let source = r#"
type Identified = {
    pub func id(self) -> Int [read self]
    pub func offset(self, amount: Int) -> Int [read self]
}

type User = & Identified & {
    value: Int
}

func User.id(self: User) -> Int {
    self.value
}

func User.offset(self: User, amount: Int) -> Int {
    self.value + amount
}

func increment_id(value: Identified) -> Int {
    value.id() + value.offset(2)
}

func main() -> Int {
    increment_id(User { value: 20 })
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));

    let missing = source
        .replace("type User = & Identified &", "type User =")
        .replace(
            "func User.id(self: User) -> Int {\n    self.value\n}\n\nfunc User.offset(self: User, amount: Int) -> Int {\n    self.value + amount\n}\n",
            "",
        );
    let error = foster::compile(&missing).unwrap_err();
    assert!(error.message.contains("missing accessible method `id`"));
}

#[test]
fn type_definitions_use_equals_and_aligned_composition() {
    let source = r#"
type Named = {
    pub name: String
}

type Person =
    & Named

func main() -> Int {
    Person { name: "Foster" }.name.length
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(6));

    let legacy = foster::compile("type Legacy {}\nfunc main() { 0 }").unwrap_err();
    assert!(legacy.message.contains("expected `=` after type name"));
}

#[test]
fn iterator_and_iterable_contracts_dispatch_stateful_iteration() {
    let source = r#"
import std.iter
import core.option

type Counter = & Iterator<Int> & {
    current: Int
    end: Int
}

func Counter.next(self: Counter) -> Option<Int> {
    let value = self.current
    self.current = self.current + 1
    branch {
        value >= self.end -> Option.None
        _ -> Option.Some(value)
    }
}

type Range = & Iterable<Int> & {
    start: Int
    end: Int
}

func Range.iterator(self: Range) -> Iterator<Int> {
    Counter { current: self.start, end: self.end }
}

func value_or(candidate: Option<Int>, fallback: Int) -> Int {
    branch candidate {
        Option.Some(value) -> value
        Option.None -> fallback
    }
}

func main() -> Int {
    let values = Range { start: 3, end: 5 }.iterator()
    let first = value_or(values.next(), -1)
    let second = value_or(values.next(), -1)
    let exhausted = value_or(values.next(), -1)
    first + second + exhausted
}
"#;

    let compilation = foster::compile(source).unwrap();
    for optimize in [false, true] {
        assert_eq!(
            foster::vm::run_with_options(&compilation, foster::vm::CompileOptions { optimize })
                .unwrap(),
            Value::Integer(6)
        );
    }
}

#[test]
fn core_iterator_adapts_sequences_and_advances_in_place() {
    let source = r#"
import std.iter
import core.option

func value_or(candidate: Option<Int>, fallback: Int) -> Int {
    branch candidate {
        Option.Some(value) -> value
        Option.None -> fallback
    }
}

func main() -> Int {
    let values = Iterator.from_sequence([7, 8])
    let first = value_or(values.next(), -1)
    let second = value_or(values.next(), -1)
    let exhausted = value_or(values.next(), -1)
    first + second + exhausted
}
"#;

    let compilation = foster::compile(source).unwrap();
    for optimize in [false, true] {
        assert_eq!(
            foster::vm::run_with_options(&compilation, foster::vm::CompileOptions { optimize })
                .unwrap(),
            Value::Integer(14)
        );
    }
}

#[test]
fn foster_written_iterator_consumers_process_remaining_elements() {
    let source = r#"
import std.iter
import core.option

func add(total: Int, value: Int) -> Int [consume total, consume value] {
    total + value
}

func two?(value: Int) -> Bool { value == 2 }
func positive?(value: Int) -> Bool { value > 0 }

func option_or(value: Option<Int>, fallback: Int) -> Int {
    branch value {
        Option.Some(item) -> item
        Option.None -> fallback
    }
}

func main() -> Int {
    let total = [1, 2, 3, 4].iterator().fold(0, add)
    let found = option_or([1, 2, 3, 4].iterator().find(two?), 0)
    let queried = [1, 2, 3, 4].iterator()
    let any = branch { queried.any?(two?) -> 10 _ -> 0 }
    let remaining = option_or(queried.next(), 0)
    let all = branch { [1, 2, 3].iterator().all?(positive?) -> 100 _ -> 0 }
    let count = [1, 2, 3, 4].iterator().count()
    [1, 2].iterator().for_each((value: Int) -> {})
    total + found + any + all + count + remaining
}
"#;

    assert_eq!(foster::run(source).unwrap(), Value::Integer(129));
}

#[test]
fn foster_written_iterator_adaptors_build_lazy_pipelines() {
    let source = r#"
import std.iter
import std.iter.map as mapping
import std.iter.filter as filtering
import std.iter.skip as skipping
import std.iter.take as taking

func double(value: Int) -> Int [consume value] { value * 2 }
func greater_than_four?(value: Int) -> Bool { value > 4 }

func main() -> Int {
    let result = [1, 2, 3, 4, 5].iterator().map(double).filter(greater_than_four?).skip(1).take(2).collect()
    result.head + result.rest.head + result.length
}
"#;

    let compilation = foster::compile(source).unwrap();
    for optimize in [false, true] {
        assert_eq!(
            foster::vm::run_with_options(&compilation, foster::vm::CompileOptions { optimize })
                .unwrap(),
            Value::Integer(20)
        );
    }
}

#[test]
fn builtin_sequences_adapt_to_collection_and_iterable() {
    let source = r#"
import std.collections
import core.option

func size<T>(values: Collection<T>) -> Int {
    values.length()
}

func value_or(candidate: Option<Int>, fallback: Int) -> Int {
    branch candidate {
        Option.Some(value) -> value
        Option.None -> fallback
    }
}

func main() -> Int {
    let values = [4, 5]
    let cursor = values.iterator()
    size(values) + size("abc") + value_or(cursor.next(), -10) + value_or(cursor.next(), -10)
}
"#;

    assert_eq!(foster::run(source).unwrap(), Value::Integer(14));
}

#[test]
fn map_is_an_iterable_collection_of_public_entries() {
    let source = r#"
import std.collections.map
import core.option

func first_value(candidate: Option<Entry<String, Int>>) -> Int {
    branch candidate {
        Option.Some(entry) -> entry.value
        Option.None -> -1
    }
}

func main() -> Int {
    let state = Map.empty()
    let values = (move state).put("answer", 42)
    let cursor = values.iterator()
    values.length() + first_value(cursor.next())
}
"#;

    assert_eq!(foster::run(source).unwrap(), Value::Integer(43));
}

#[test]
fn foster_collections_and_range_share_collection_contract() {
    let source = r#"
import std.collections
import core.range
import std.collections.set

func size<T>(values: Collection<T>) -> Int {
    values.length()
}

func main() -> Int {
    let distinct = Set.from([1, 1, 2])
    let span = Range.from([3, 4, 5])
    size(distinct) * 10 + size(span)
}
"#;

    assert_eq!(foster::run(source).unwrap(), Value::Integer(23));
}

#[test]
fn mutable_effect_allows_extracting_children_but_not_consuming_the_owner() {
    let source = r#"
type Resource = { value: String }

func Resource.invalid(self: Resource) -> Resource [mut self] {
    move self
}

func main() -> Int { 0 }
"#;

    let error = foster::compile(source).unwrap_err();
    assert!(error.message.contains("consume self"), "{}", error.message);
}

#[test]
fn equality_ordering_and_hashing_contracts_compose_and_dispatch() {
    let source = r#"
import core.ordering

type Key = & Ordered<Key> & Hashing & {
    value: Int
}

func Key.equal?(self: Key, other: Key) -> Bool {
    self.value == other.value
}

func Key.compare(self: Key, other: Key) -> Ordering {
    branch {
        self.value < other.value -> Ordering.Less
        self.value > other.value -> Ordering.Greater
        _ -> Ordering.Equal
    }
}

func Key.hash(self: Key) -> Int {
    self.value * 31
}

func equality_score(left: Equality<Key>, right: Key) -> Int {
    branch {
        left.equal?(right) -> 1
        _ -> 0
    }
}

func ordering_score(left: Ordered<Key>, right: Key) -> Int {
    branch left.compare(right) {
        Ordering.Less -> 10
        Ordering.Equal -> 20
        Ordering.Greater -> 30
    }
}

func hash_score(value: Hashing) -> Int {
    value.hash()
}

func main() -> Int {
    let key = Key { value: 7 }
    equality_score(key, Key { value: 7 }) + ordering_score(key, Key { value: 8 }) + hash_score(key)
}
"#;

    let compilation = foster::compile(source).unwrap();
    for optimize in [false, true] {
        assert_eq!(
            foster::vm::run_with_options(&compilation, foster::vm::CompileOptions { optimize })
                .unwrap(),
            Value::Integer(228)
        );
    }

    let missing_equality = source.replace(
        "func Key.equal?(self: Key, other: Key) -> Bool {\n    self.value == other.value\n}\n\n",
        "",
    );
    let error = foster::compile(&missing_equality).unwrap_err();
    assert!(
        error.message.contains("missing required method `equal?`"),
        "{}",
        error.message
    );
}

#[test]
fn matching_contracts_conform_without_an_explicit_composition_clause() {
    let source = r#"
type TextSlice = {
    pub empty?: Bool
    pub length: Int
    pub head: CodePoint
    pub rest: String
}

func first(values: Sequence<CodePoint>) -> CodePoint {
    values.head()
}

func main() -> CodePoint {
    first(TextSlice { empty?: false, length: 2, head: 'O', rest: "K" })
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::CodePoint('O'));
}

#[test]
fn intersection_parameters_require_every_composed_contract() {
    let source = r#"
import core.string as strings

type Named = {
    pub name: String
}

type TextSlice = & Named & Sequence<CodePoint> & {
    text: String
}

func TextSlice.empty?(self: TextSlice) -> Bool { self.text.empty? }
func TextSlice.length(self: TextSlice) -> Int { self.text.length }
func TextSlice.head(self: TextSlice) -> CodePoint { self.text.head }
func TextSlice.rest(self: TextSlice) -> String { strings.slice(self.text, 1, self.text.length) }

func describe(value: Named & Sequence<CodePoint>) -> String {
    value.name + value.head().string
}

func main() -> String {
    describe(TextSlice {
        name: "answer: "
        text: "Y"
    })
}
"#;
    assert_string(foster::run(source).unwrap(), "answer: Y");
}

#[test]
fn bare_intersection_parameters_unify_independent_of_member_order() {
    let source = r#"
type A = {
    pub first: Int
}

type B = {
    pub second: Int
}

func takes_ab(value: A & B) -> Int { value.first }
func takes_ba(value: B & A) -> Int { takes_ab(value) }
func returns_ab(value: A & B) -> B & A { value }

func main() -> Int { 0 }
"#;

    foster::compile(source).unwrap();
}

#[test]
fn declared_composition_requires_callable_members() {
    let error = foster::compile(
        r#"
type Broken = & Sequence<CodePoint> & {}

func main() { 0 }
"#,
    )
    .unwrap_err();
    assert!(error.message.contains("missing required method `empty?`"));
}

#[test]
fn declared_composition_rejects_incompatible_contract_members() {
    let error = foster::compile(
        r#"
type TextNamed = {
    pub name: String
}

type NumericNamed = {
    pub name: Int
}

type Broken = & TextNamed & NumericNamed & {}

func main() { 0 }
"#,
    )
    .unwrap_err();
    assert!(
        error
            .message
            .contains("composes incompatible definitions of field `name`")
    );
}

#[test]
fn variants_support_shared_contract_bodies_and_instance_methods() {
    let source = r#"
enum Choice = Number(Int)
    | Empty
    & {
        pub func score(self) -> Int
    }
func Choice.score(self: Choice) -> Int {
    branch self {
        Choice.Number(value) -> value
        Choice.Empty -> 0
    }
}

func main() -> Int { Choice.Number(42).score() }
"#;

    let compilation = foster::compile(source).unwrap();
    for optimize in [false, true] {
        assert_eq!(
            foster::vm::run_with_options(&compilation, foster::vm::CompileOptions { optimize })
                .unwrap(),
            Value::Integer(42)
        );
    }
}

#[test]
fn variants_compose_method_only_contracts() {
    let source = r#"
type Scored = {
    pub func score(self) -> Int
}

enum Choice = Number(Int)
    | Empty
    & Scored

func Choice.score(self: Choice) -> Int { 42 }
func score_of(value: Scored) -> Int { value.score() }
func main() -> Int { score_of(Choice.Empty) }
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));

    let missing = source.replace("func Choice.score(self: Choice) -> Int { 42 }\n", "");
    let error = foster::compile(&missing).unwrap_err();
    assert!(error.message.contains("missing required method `score`"));
}

#[test]
fn enum_shared_bodies_reject_stored_fields() {
    let error = foster::compile(
        r#"
enum Choice = Empty
    & { value: Int }
func main() { 0 }
"#,
    )
    .unwrap_err();
    assert!(
        error
            .message
            .contains("enum and union shared bodies may only declare required methods")
    );
}

#[test]
fn assignment_reinitializes_a_moved_local() {
    foster::compile(
        r#"
func take(value: String) -> () [consume value] { println() }
func main() -> String {
    let value = "first"
    take(move value)
    value = "second"
    value
}
"#,
    )
    .unwrap();
}

#[test]
fn local_creation_uses_let_and_assignment_requires_an_existing_local() {
    let value = foster::run(
        r#"
func main() -> Int {
    let value = 1
    value = 42
    value
}
"#,
    )
    .unwrap();
    assert_eq!(value, Value::Integer(42));

    let undeclared = foster::compile("func main() { value = 1 }").unwrap_err();
    assert!(
        undeclared
            .message
            .contains("cannot assign to undeclared local `value`")
    );

    let duplicate = foster::compile("func main() { let value = 1\nlet value = 2 }").unwrap_err();
    assert!(
        duplicate
            .message
            .contains("local `value` is already declared")
    );
}

#[test]
fn joins_move_state_across_branch_arms() {
    let error = foster::compile(
        r#"
func take(value: String) -> () [consume value] { println() }
func choose(flag: Bool) -> String {
    let value = "owned"
    branch {
        flag -> take(move value)
        _ -> println()
    }
    value
}
func main() -> () { println() }
"#,
    )
    .unwrap_err();
    assert!(
        error
            .message
            .contains("value `value` is used after it was moved")
    );
}

#[test]
fn permits_disjoint_field_use_after_a_partial_move() {
    foster::compile(
        r#"
type Pair = {
    left: String
    right: String
}
func take(value: String) -> () [consume value] { println() }
func remaining(pair: Pair) -> String [consume pair] {
    take(move pair.left)
    pair.right
}
func main() -> () { println() }
"#,
    )
    .unwrap();
}

#[test]
fn runs_the_live_inventory_pipeline() {
    assert_eq!(
        foster::run(include_str!("../examples/live_inventory_pipeline.fos")).unwrap(),
        Value::Integer(1242)
    );
}

#[test]
fn supports_line_block_and_documentation_comments() {
    let source = r#"
//! Public values and documentation-comment behavior used by this test.

/// A named value used by the public API.
/**
 * The second paragraph is retained as Markdown.
 */
pub type Named = {
    pub value: Int
}

/// Returns the value.
///
/// This text is available to language tooling.
pub func value(named: Named) -> Int {
    /* Comments can appear between tokens and /* can be nested. */ */
    named.value // ordinary comments are discarded
}

func main() -> Int { value(Named { value: 7 }) }
"#;
    let program = foster::parse(source).unwrap();
    assert_eq!(
        program.documentation.as_deref(),
        Some("Public values and documentation-comment behavior used by this test.")
    );
    assert_eq!(
        program.records[0].documentation.as_deref(),
        Some(
            "A named value used by the public API.\nThe second paragraph is retained as Markdown."
        )
    );
    assert_eq!(
        program.functions[0].documentation.as_deref(),
        Some("Returns the value.\n\nThis text is available to language tooling.")
    );
    assert_eq!(foster::run(source).unwrap(), Value::Integer(7));

    let error = foster::parse("func main() { /* never closed").unwrap_err();
    assert!(error.message.contains("unterminated block comment"));
}

#[test]
fn uses_empty_parentheses_as_the_only_unit_type_and_value_syntax() {
    assert_eq!(
        foster::run("func main() -> () { () }").unwrap(),
        Value::Unit
    );

    let removed_name = foster::compile("func main() -> Unit { () }").unwrap_err();
    assert!(
        removed_name.message.contains("unknown type `Unit`"),
        "{}",
        removed_name.message
    );
}

#[test]
fn structurally_adapts_records_with_additional_public_fields() {
    let source = r#"
type Named = {
    pub name: String
}

type Located = {
    pub location: String
}

type User = {
    pub name: String
    pub location: String
    pub email: String
}

func label_size(value: Named & Located) -> Int {
    value.name.length + value.location.length
}

func name_size(value: Named) -> Int {
    value.name.length
}

func main() -> Int {
    let user = User {
        name: "Jason"
        location: "Boston"
        email: "jason@example.com"
    }
    name_size(user) * 100 + label_size(user)
}
"#;
    let compilation = foster::compile(source).unwrap();
    let module = compilation.hir.module_named("main").unwrap();
    let label = compilation
        .hir
        .function_named(module, "label_size")
        .unwrap();
    let signature = compilation.types.function_type(label).unwrap();
    assert_eq!(
        compilation.types.display(signature.parameters[0]),
        "Named & Located"
    );
    assert_eq!(foster::run(source).unwrap(), Value::Integer(511));
}

#[test]
fn structural_adaptation_reports_missing_and_incompatible_fields() {
    let missing = foster::compile(
        r#"
type Named = { pub name: String }
type Product = { pub title: String }
func name(value: Named) -> String { value.name }
func main() -> String { name(Product { title: "Book" }) }
"#,
    )
    .unwrap_err();
    assert!(missing.message.contains("missing accessible field `name`"));

    let incompatible = foster::compile(
        r#"
type Named = { pub name: String }
type NumericName = { pub name: Int }
func name(value: Named) -> String { value.name }
func main() -> String { name(NumericName { name: 42 }) }
"#,
    )
    .unwrap_err();
    assert!(
        incompatible
            .message
            .contains("expected `String`, found `Int`")
    );
}

#[test]
fn consuming_a_structural_view_moves_the_original_value() {
    let source = r#"
type Named = { pub name: String }
type User = {
    pub name: String
    pub email: String
}
func take(value: Named) -> String [consume value] { value.name }
func main() -> String {
    let user = User { name: "Jason", email: "jason@example.com" }
    take(move user)
}
"#;
    assert_string(foster::run(source).unwrap(), "Jason");

    let invalid = source.replace("take(move user)\n}", "take(move user)\n    user.email\n}");
    let error = foster::compile(&invalid).unwrap_err();
    assert!(error.message.contains("used after it was moved"));
}

#[test]
fn private_record_fields_prevent_cross_module_structural_adaptation() {
    let error = foster::check_package("tests/fixtures/structural_privacy").unwrap_err();
    assert!(error.message.contains("field `value` is private"));
}

#[test]
fn structural_return_conversion_moves_and_narrows_the_value() {
    let source = r#"
type Named = { pub name: String }
type User = {
    pub name: String
    pub email: String
}
func as_named(user: User) -> Named [consume user] { user }
func main() -> Int {
    let user = User { name: "Jason", email: "jason@example.com" }
    let named = as_named(move user)
    named.name.length
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(5));
}

#[test]
fn structurally_adapts_records_containing_the_required_value() {
    let source = r#"
type Bar = { pub value: Int }
type Foo = { pub bar: Bar }
type Container = {
    pub bar: Bar
    pub label: String
}
func extract(value: Foo) -> Int { value.bar.value }
func main() -> Int {
    extract(Container { bar: Bar { value: 42 }, label: "answer" })
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));
}

#[test]
fn strings_and_lists_implement_sequence_without_conversion() {
    let source = r#"
import std.sequence

func main() -> Int {
    let letters = sequence.count("banana", (value: CodePoint) -> value == 'a')
    let evens = sequence.count([1, 2, 3, 4], (value: Int) -> value / 2 * 2 == value)
    letters * 10 + evens
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(32));
}

#[test]
fn code_point_literals_are_distinct_copy_values() {
    let source = r#"
func main() -> String {
    let value = 'λ'
    let render = [copy value] () -> value.string
    branch {
        value.whitespace? -> "space"
        _ -> render()
    }
}
"#;
    assert_string(foster::run(source).unwrap(), "λ");
}

#[test]
fn code_points_promote_through_integer_operators() {
    let source = r#"
func main() -> Int {
    let digit = '9' - '0'
    branch {
        'A' == 65 -> digit * 10 + ('C' - 'A')
        _ -> 0
    }
}
"#;

    let compilation = foster::compile(source).unwrap();
    for optimize in [false, true] {
        let result =
            foster::vm::run_with_options(&compilation, foster::vm::CompileOptions { optimize })
                .unwrap();
        assert_eq!(result, Value::Integer(92));
    }
}

#[test]
fn code_point_core_methods_support_instance_and_associated_calls() {
    let source = r#"
import core.code_point

func main() -> Int {
    let first = 'A'
    first.as_int() + CodePoint.as_int('B')
}
"#;

    assert_eq!(foster::run(source).unwrap(), Value::Integer(131));
}

#[test]
fn byte_and_code_point_widen_to_int_in_expected_type_contexts() {
    let source = r#"
import core.byte

func accept(value: Int) -> Int { value }

func code_point_value() -> Int { 'A' }

func choose(use_code_point: Bool) -> Int {
    branch {
        use_code_point -> 'B'
        _ -> Byte.unchecked(3)
    }
}

func main() -> Int {
    let assigned = 0
    assigned = 'C'
    accept('A') + accept(Byte.unchecked(2)) + code_point_value() + choose(false) + assigned
}
"#;

    let compilation = foster::compile(source).unwrap();
    for optimize in [false, true] {
        let result =
            foster::vm::run_with_options(&compilation, foster::vm::CompileOptions { optimize })
                .unwrap();
        assert_eq!(result, Value::Integer(202));
    }
}

#[test]
fn integer_widening_does_not_apply_in_reverse() {
    let code_point = foster::compile("func main() -> CodePoint { 65 }").unwrap_err();
    assert!(
        code_point.message.contains("CodePoint") && code_point.message.contains("Int"),
        "{code_point:?}"
    );

    let byte = foster::compile(
        r#"
import core.byte
func receive(value: Byte) -> Byte { value }
func main() -> Byte { receive(65) }
"#,
    )
    .unwrap_err();
    assert!(
        byte.message.contains("Byte") && byte.message.contains("Int"),
        "{byte:?}"
    );
}

#[test]
fn integer_widening_preserves_generic_inference_and_container_invariance() {
    let generic = r#"
func identity<T>(value: T) -> T [consume value] { value }
func main() -> CodePoint { identity('A') }
"#;
    assert_eq!(foster::run(generic).unwrap(), Value::CodePoint('A'));

    let container = foster::compile(
        r#"
func first(values: List<Int>) -> Int { values.head }
func main() -> Int {
    let characters = ['A']
    first(characters)
}
"#,
    )
    .unwrap_err();
    assert!(
        container.message.contains("CodePoint") && container.message.contains("Int"),
        "{container:?}"
    );
}

#[test]
fn code_points_do_not_expose_a_value_member() {
    let error = foster::compile("func main() -> Int { 'A'.value }").unwrap_err();
    assert!(error.message.contains("has no member `value`"));
}

#[test]
fn bytes_and_byte_buffers_enforce_bounds_and_round_trip_utf8() {
    let source = r#"
import core.byte
import core.bytes.buffer as byte_buffer
import core.bytes
import core.result

func byte_or(value: Result<Byte, ByteError>, fallback: Byte) -> Byte {
    branch value {
        Result.Ok(item) -> item
        Result.Error(_) -> fallback
    }
}

func text_or(value: Result<String, Utf8Error>) -> String {
    branch value {
        Result.Ok(text) -> text
        Result.Error(_) -> "invalid"
    }
}

func main() -> String {
    let zero = byte_or(Byte.from(0), Byte.unchecked(0))
    let capital_a = byte_or(Byte.from(65), zero)
    let lower_x = byte_or(Byte.from(120), zero)

    let buffer = ByteBuffer.with_capacity(4)
    buffer.push(capital_a)
    buffer.extend("BC".utf8)
    buffer[1] = lower_x

    let data = buffer.snapshot()
    text_or(String.from_utf8(data)) + ":" + data.hex()
}
"#;

    let compilation = foster::compile(source).unwrap();
    for optimize in [false, true] {
        assert_string(
            foster::vm::run_with_options(&compilation, foster::vm::CompileOptions { optimize })
                .unwrap(),
            "AxC:417843",
        );
    }
}

#[test]
fn byte_construction_rejects_out_of_range_integers() {
    let source = r#"
import core.byte
import core.result

func main() -> Int {
    branch Byte.from(256) {
        Result.Ok(value) -> value.int
        Result.Error(error) -> error.value
    }
}

"#;

    assert_eq!(foster::run(source).unwrap(), Value::Integer(256));
}

#[test]
fn byte_bitwise_operators_preserve_byte_values() {
    let source = r#"
import core.byte

func main() -> Int {
    let high = Byte.unchecked(240)
    let low = Byte.unchecked(15)
    let mixed = (high & ~low) | (low ^ Byte.unchecked(3))
    let shifted = mixed >> 2
    shifted.int + (Byte.unchecked(1) << 7).int
}

"#;

    assert_eq!(foster::run(source).unwrap(), Value::Integer(191));
}

#[test]
fn bytes_decode_hex_and_report_invalid_utf8() {
    let source = r#"
import core.bytes
import core.result

func decode(value: Result<Bytes, HexError>) -> String {
    branch value {
        Result.Error(error) -> error.message
        Result.Ok(data) -> branch String.from_utf8(data) {
            Result.Ok(text) -> text
            Result.Error(_) -> data.hex()
        }
    }
}

func main() -> String {
    decode(Bytes.from_hex("4869")) + ":" + decode(Bytes.from_hex("ff"))
}
"#;

    assert_string(foster::run(source).unwrap(), "Hi:ff");
}

#[test]
fn bytes_are_iterable_collections() {
    let source = r#"
import core.bytes
import std.collections
import core.option
import core.result

func size(values: Collection<Byte>) -> Int {
    values.length()
}

func first(value: Option<Byte>) -> Int {
    branch value {
        Option.Some(item) -> item.int
        Option.None -> -1
    }
}

func unpack(value: Result<Bytes, HexError>) -> Int {
    branch value {
        Result.Error(_) -> -1
        Result.Ok(data) -> size(data) * 100 + first(data.iterator().next())
    }
}

func main() -> Int {
    unpack(Bytes.from_hex("2a2b"))
}
"#;

    assert_eq!(foster::run(source).unwrap(), Value::Integer(242));
}

#[test]
fn freezing_a_byte_buffer_produces_bytes_and_consumes_the_buffer() {
    let source = r#"
import core.bytes.buffer as byte_buffer
import core.byte

func main() -> String {
    let buffer = ByteBuffer.empty()
    buffer.push(Byte.unchecked(42))
    let data = (move buffer).freeze()
    data.hex()
}
"#;

    assert_string(foster::run(source).unwrap(), "2a");

    let invalid = source.replace("    data.hex()", "    buffer.length\n    data.hex()");
    let error = foster::compile(&invalid).unwrap_err();
    assert!(
        error.message.contains("used after it was moved"),
        "{}",
        error.message
    );
}

#[test]
fn structural_byte_buffer_mutation_invalidates_element_loans() {
    let source = r#"
import core.bytes.buffer as byte_buffer
import core.byte

func main() -> Int {
    let buffer = ByteBuffer.empty()
    buffer.push(Byte.unchecked(1))
    let item = ref buffer[0]
    buffer.extend("more".utf8)
    item.int
}
"#;

    let error = foster::compile(source).unwrap_err();
    assert!(error.message.contains("invalidated"), "{}", error.message);
}

#[test]
fn generic_stream_contracts_handle_partial_io_and_eof() {
    let source = r#"
import core.bytes.buffer as byte_buffer
import core.bytes
import core.int
import core.result
import std.io as stream

type StreamError = {
    message: String
}

type ChunkReader = & Reader<StreamError> & {
    remaining: Bytes
    chunk_size: Int
}

type CollectWriter = & Writer<StreamError> & {
    contents: Bytes
    chunk_size: Int
}

func ChunkReader.read(self: ChunkReader, maximum: Int) -> Result<Bytes, StreamError> [mut self.remaining, read self.chunk_size] {
    let limit = smaller(maximum, self.chunk_size)
    let amount = smaller(limit, self.remaining.length)
    let chunk = self.remaining.slice(0, amount)
    self.remaining = self.remaining.slice(amount, self.remaining.length)
    Result.Ok(chunk)
}

func CollectWriter.write(self: CollectWriter, contents: Bytes) -> Result<Int, StreamError> [mut self.contents, read self.chunk_size] {
    let amount = smaller(self.chunk_size, contents.length)
    self.contents = self.contents.concat(contents.slice(0, amount))
    Result.Ok(amount)
}

func CollectWriter.flush(self: CollectWriter) -> Result<(), StreamError> {
    let scratch = ByteBuffer.empty()
    Result.Ok(scratch.reserve(0))
}

func smaller(left: Int, right: Int) -> Int {
    branch {
        left < right -> left
        _ -> right
    }
}

func decoded(outcome: Result<Bytes, HexError>) -> Bytes {
    branch outcome {
        Result.Error(_) -> Bytes.empty()
        Result.Ok(contents) -> contents
    }
}

func rendered(outcome: Result<Bytes, StreamError>) -> String {
    branch outcome {
        Result.Error(error) -> error.message
        Result.Ok(contents) -> contents.hex()
    }
}

func copied(outcome: Result<Int, StreamError>) -> String {
    branch outcome {
        Result.Error(error) -> error.message
        Result.Ok(count) -> int.to_string(count)
    }
}

func main() -> String {
    let all_contents = decoded(Bytes.from_hex("00010203040506"))
    let all_reader = ChunkReader { remaining: all_contents, chunk_size: 2 }
    let all = rendered(read_all(all_reader))

    let copy_contents = decoded(Bytes.from_hex("00010203040506"))
    let copy_reader = ChunkReader { remaining: copy_contents, chunk_size: 2 }
    let writer = CollectWriter { contents: Bytes.empty(), chunk_size: 3 }
    let count = copied(stream.copy(copy_reader, writer))
    all + ":" + writer.contents.hex() + ":" + count
}
"#;

    for optimize in [false, true] {
        assert_string(
            foster::run_with_options(source, foster::vm::CompileOptions { optimize }).unwrap(),
            "00010203040506:00010203040506:7",
        );
    }
}

#[test]
fn runs_the_generic_recursive_linked_list_example() {
    let source = include_str!("../examples/linked_list.fos");
    for optimize in [false, true] {
        assert_eq!(
            foster::run_with_options(source, foster::vm::CompileOptions { optimize }).unwrap(),
            Value::Integer(13)
        );
    }
}
