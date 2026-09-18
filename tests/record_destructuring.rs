use foster::vm::{CompileOptions, Value};

fn check(source: &str, expected: i64) {
    let compilation = foster::compile(source).unwrap();
    for optimize in [false, true] {
        let program =
            foster::vm::compile_with_options(&compilation, CompileOptions { optimize }).unwrap();
        assert_eq!(
            foster::vm::Machine::new(&program)
                .run_main()
                .unwrap_or_else(|e| panic!("direct optimize={optimize}: {e}")),
            Value::Integer(expected)
        );
        let bytes = foster::vm::encode_program(&program).unwrap();
        let decoded = foster::vm::decode_program(&bytes).unwrap();
        assert_eq!(
            foster::vm::Machine::new(&decoded)
                .run_main()
                .unwrap_or_else(|e| panic!("optimize={optimize}: {e}")),
            Value::Integer(expected)
        );
    }
}

#[test]
fn bindings_select_and_rename_fields() {
    check(
        r#"
type Pair<T> = { first: T, second: Int }
func main() -> Int {
    let pair = Pair { first: 20, second: 22 }
    let { first } = pair
    let { second: other } = pair
    assert(pair.second == 22)
    first + other
}
"#,
        42,
    );
}

#[test]
fn nested_patterns_in_branches() {
    check(
        r#"
type Pair = { first: Int, second: Int }
enum Outcome = Match(Pair) | Failed
func main() -> Int {
    let result = Outcome.Match(Pair { first: 20, second: 22 })
    branch result {
        Outcome.Match({ first: 0 }) -> 0
        Outcome.Match({ first, second }) -> first + second
        Outcome.Failed -> 0
    }
}
"#,
        42,
    );
}

#[test]
fn computed_source_is_evaluated_once() {
    check(
        r#"
type Counter = { value: Int }
type Pair = { first: Int, second: Int }
func make[g: group Counter](counter: ref[g] Counter) -> Pair [mut g] {
    counter.value = counter.value + 1
    Pair { first: 20, second: 22 }
}

func main() -> Int {
    let counter = Counter { value: 0 }
    let { first, second } = make(ref counter)
    assert(counter.value == 1)
    first + second
}
"#,
        42,
    );
}

#[test]
fn ordinary_field_access_mutation_comparison() {
    check(
        r#"
type Counter = { value: Int }
type Pair = { first: Int, second: Int }
func make(counter: Counter) -> Pair [mut counter] {
    counter.value = counter.value + 1
    Pair { first: 20, second: 22 }
}
func main() -> Int {
    let counter = Counter { value: 0 }
    let pair = make(counter)
    assert(counter.value == 1)
    pair.first + pair.second
}
"#,
        42,
    );
}
#[test]
fn nested_binding_and_scalar_reassignment() {
    check(
        r#"
type Inner = { value: Int }
type Outer = { inner: Inner, other: Int }
func main() -> Int {
    let outer = Outer { inner: Inner { value: 20 }, other: 22 }
    let { inner: { value }, other: extra } = outer
    value = 40
    assert(outer.inner.value == 20)
    value + extra - 20
}
"#,
        42,
    );
}

#[test]
fn managed_fields_follow_ordinary_moves() {
    check(
        r#"
type Item = { value: Int }
type Pair = { first: Item, second: Item }
func main() -> Int {
    let pair = Pair { first: Item { value: 20 }, second: Item { value: 22 } }
    let { first } = pair
    first.value + pair.second.value
}
"#,
        42,
    );
    for binding in ["let first = pair.first", "let { first } = pair"] {
        let source = format!(
            "type Item = {{ value: Int }}\ntype Pair = {{ first: Item, second: Int }}\nfunc main() -> Int {{\nlet pair = Pair {{ first: Item {{ value: 20 }}, second: 22 }}\n{binding}\npair.first.value\n}}"
        );
        assert!(foster::compile(&source).is_err(), "accepted {binding}");
    }
}

#[test]
fn invalid_patterns_are_rejected() {
    for (pattern, message) in [
        ("{ missing }", "missing"),
        ("{ first, first }", "duplicate"),
        ("{ first: x, second: x }", "more than once"),
        ("{ first: 1 }", "conditional pattern"),
    ] {
        let source = format!(
            "type Pair = {{ first: Int, second: Int }}\nfunc main() -> Int {{\nlet pair = Pair {{ first: 20, second: 22 }}\nlet {pattern} = pair\n42\n}}"
        );
        let error = foster::compile(&source).unwrap_err().to_string();
        assert!(error.contains(message), "{pattern}: {error}");
    }
    assert!(foster::compile("func main() -> Int { let { value } = 42\nvalue }").is_err());
    assert!(foster::compile("type P = { value: Int }\nfunc main() -> Int { let p = P { value: 42 }\nbranch p { { value: 0 } -> 0 } }").is_err());
}

#[test]
fn formatting_preserves_record_patterns() {
    let source = "type P = { value: Int }\nfunc main() -> Int { let { value: answer } = P { value: 42 }\nanswer }";
    let formatted = foster::formatter::format(source).unwrap();
    assert_eq!(formatted, foster::formatter::format(&formatted).unwrap());
    check(&formatted, 42);
}

#[test]
fn disjoint_fields_can_be_extracted_in_separate_bindings() {
    check(
        r#"
type Item = { value: Int }
type Pair = { first: Item, second: Item }
func main() -> Int {
    let pair = Pair { first: Item { value: 20 }, second: Item { value: 22 } }
    let { first } = pair
    let { second } = pair
    first.value + second.value
}
"#,
        42,
    );
}

#[test]
fn privacy_and_method_access_are_not_bypassed() {
    let error =
        foster::compile("func main() -> Int { let { value } = \"private\"\n42 }").unwrap_err();
    assert!(error.message.contains("private"), "{error}");
    let error = foster::compile("type P = { x: Int }\nimpl P { func value(self) -> Int { self.x } }\nfunc main() -> Int { let { value } = P { x: 42 }\nvalue }").unwrap_err();
    assert!(error.message.contains("has no field"), "{error}");
}

#[test]
fn pattern_bindings_keep_source_spans() {
    let source = "type P = { value: Int }\nfunc main() -> Int { let { value: answer } = P { value: 42 }\nanswer }";
    let compilation = foster::compile(source).unwrap();
    let (_, local) = compilation
        .hir
        .locals
        .iter()
        .find(|(_, local)| local.name == "answer")
        .unwrap();
    assert_eq!(&source[local.span.clone()], "answer");
}

#[test]
fn malformed_record_pattern_metadata_is_rejected() {
    let compilation = foster::compile("type P = { value: Int }\nfunc main() -> Int { let p = P { value: 42 }\nbranch p { { value } -> value } }").unwrap();
    let mut program =
        foster::vm::compile_with_options(&compilation, CompileOptions { optimize: false }).unwrap();
    let pattern = program
        .functions
        .values_mut()
        .flat_map(|f| &mut f.instructions)
        .find_map(|instruction| {
            if let foster::vm::Instruction::MatchPattern { pattern, .. } = instruction {
                Some(pattern)
            } else {
                None
            }
        })
        .unwrap();
    fn corrupt(pattern: &mut foster::hir::Pattern) {
        match pattern {
            foster::hir::Pattern::Spanned { pattern, .. } => corrupt(pattern),
            foster::hir::Pattern::Record { fields } => fields[0].0 = "missing".into(),
            _ => panic!("expected record pattern"),
        }
    }
    corrupt(pattern);
    let error = foster::vm::encode_program(&program).unwrap_err();
    assert!(error.to_string().contains("missing field"), "{error}");
}

#[test]
fn destructuring_cannot_move_fields_out_of_drop_owners() {
    let source = r#"
import core.drop
type Item = { value: Int }
type Parent = { item: Item }
impl Parent { func deinit(self) -> () { () } }
func main() -> Int {
    let parent = Parent { item: Item { value: 42 } }
    let { item } = parent
    item.value
}
"#;
    let error = foster::compile(source).unwrap_err();
    assert!(error.message.contains("deinit"), "{error}");
}

#[test]
fn record_patterns_follow_references() {
    check(
        include_str!("fixtures/programs/record_destructuring.fos"),
        42,
    );
}
