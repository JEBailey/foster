use foster::vm::{CompileOptions, Value};

const PROGRAM: &str = include_str!("fixtures/programs/selected_try.fos");

#[test]
fn selected_try_unwraps_and_propagates_all_other_cases() {
    let compilation = foster::compile(PROGRAM).unwrap();
    for optimize in [false, true] {
        assert_eq!(
            foster::vm::run_with_options(&compilation, CompileOptions { optimize }).unwrap(),
            Value::Integer(42)
        );
    }
}

#[test]
fn selected_try_rejects_invalid_selections_and_returns() {
    for (source, message) in [
        (
            "enum O = Yes(Int) | No\nfunc f() -> O { try<Missing> O.Yes(1)\nO.No }",
            "has no variant",
        ),
        ("func f() -> Int { try<Yes> 1 }", "requires an enum value"),
        (
            "enum O = Yes(Int) | No\nfunc f() -> Int { try<Yes> O.Yes(1) }",
            "enum return type",
        ),
        (
            "enum O = Yes(Int) | No\nenum R = Done(Int)\nfunc f() -> R { let x = try<Yes> O.Yes(1)\nR.Done(x) }",
            "cannot propagate",
        ),
        (
            "enum O = Yes(Int) | No(Int)\nenum R = Done(Int) | No(Bool)\nfunc f() -> R { let x = try<Yes> O.Yes(1)\nR.Done(x) }",
            "matching payload types",
        ),
        (
            "enum O = Yes(Int) | No\nenum R = Done(Int) | No(Int)\nfunc f() -> R { let x = try<Yes> O.Yes(1)\nR.Done(x) }",
            "matching payloads",
        ),
    ] {
        let error = foster::compile(source).unwrap_err().to_string();
        assert!(error.contains(message), "{error}");
    }
}

#[test]
fn selected_try_syntax_round_trips_and_rejects_malformed_selectors() {
    let formatted = foster::formatter::format(PROGRAM).unwrap();
    assert!(formatted.contains("try<Match>"));
    assert_eq!(formatted, foster::formatter::format(&formatted).unwrap());
    for expression in ["try<> value", "try<A,B> value", "try<A value"] {
        assert!(foster::parse(&format!("func f() {{ {expression} }}")).is_err());
    }
}

#[test]
fn selected_try_consumes_owned_operands() {
    let source = r#"
import core.drop
type Item = & Drop & {}
impl Item { func deinit(self) -> () {} }
enum Outcome = Match(Item) | Failed
func pass(input: Outcome) -> Outcome {
    let value = try<Match> input
    let again = try<Match> input
    Outcome.Match(move value)
}
"#;
    assert!(foster::compile(source).is_err());
}

#[test]
fn selected_try_infers_a_closure_return_enum() {
    let source = r#"
enum Outcome<T> = Match(T) | Failed(Int)
func parser() -> func(consume Outcome<Int>) -> Outcome<Bool> {
    (input: Outcome<Int>) -> {
        let value = try<Match> move input
        Outcome.Match(value == 42)
    }
}
func main() -> Int {
    let parse = parser()
    branch parse(Outcome.Match(42)) {
        Outcome.Match(value) -> { assert(value)
            42 }
        Outcome.Failed(_) -> 0
    }
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));
}
