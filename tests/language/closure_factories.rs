use foster::vm::Value;

fn source(factory: &str, setup: &str, mutation: &str, result: &str) -> String {
    format!(
        r#"
{factory}
func main() -> Int {{
    let left = [21]
    let right = [99]
    let selected = ref left[0]
    let ignored = ref right[0]
    let keep = [ref selected] () -> selected
    let discard = [ref ignored] () -> ignored
    {setup}
    {mutation}
    {result}
}}
"#
    )
}

#[test]
fn factories_preserve_only_the_captured_callable_parameter() {
    let factory = "func wrap(keep: func() -> Int, discard: func() -> Int) -> func() -> Int { [move keep] () -> keep() }";
    for setup in [
        "let reader = wrap(move keep, discard)",
        "let factory = wrap\nlet reader = factory(move keep, discard)",
    ] {
        let valid = source(factory, setup, "right.push(100)", "reader()");
        assert_eq!(foster::run(&valid).unwrap(), Value::Integer(21));
        let invalid = valid.replace("right.push(100)", "left.push(100)");
        assert_eq!(
            foster::compile(&invalid).unwrap_err().code.as_deref(),
            Some("E0401")
        );
    }
}

#[test]
fn nested_factories_forward_hidden_borrowers() {
    let factory = r#"
func wrap(keep: func() -> Int, discard: func() -> Int) -> func() -> Int { [move keep] () -> keep() }
func relay(keep: func() -> Int, discard: func() -> Int) -> func() -> Int { wrap(move keep, discard) }
func nested(keep: func() -> Int, discard: func() -> Int) -> func() -> func() -> Int {
    [move keep] () -> { let inner = move keep
        [move inner] () -> inner() }
}
"#;
    for setup in [
        "let reader = relay(move keep, discard)",
        "let outer = nested(move keep, discard)\nlet reader = outer()",
    ] {
        let valid = source(factory, setup, "right.push(100)", "reader()");
        assert_eq!(foster::run(&valid).unwrap(), Value::Integer(21));
        assert_eq!(
            foster::compile(&valid.replace("right.push(100)", "left.push(100)"))
                .unwrap_err()
                .code
                .as_deref(),
            Some("E0401")
        );
    }
}

#[test]
fn callable_parameter_slot_borrows_cannot_escape_as_contents() {
    let factory = "func wrap(keep: func() -> Int, discard: func() -> Int) -> func() -> Int { [ref keep] () -> keep() }";
    assert!(
        foster::compile(&source(
            factory,
            "let reader = wrap(keep, discard)",
            "",
            "reader()"
        ))
        .is_err()
    );
}

#[test]
fn returned_record_and_mutating_aliases_preserve_hidden_dependencies() {
    let factory = r#"
type Box<T> = { value: T }
func wrap(keep: func() -> Int, discard: func() -> Int) -> Box<func() -> Int> {
    Box { value: [move keep] () -> keep() }
}
"#;
    let valid = source(
        factory,
        "let reader = wrap(move keep, discard)",
        "right.push(100)",
        "reader.value()",
    );
    assert_eq!(foster::run(&valid).unwrap(), Value::Integer(21));
    for mutation in [
        "left.push(100)",
        "let alias = ref left\nalias.push(100)",
        "let change = [ref left] () -> left.push(100)\nchange()",
    ] {
        assert_eq!(
            foster::compile(&valid.replace("right.push(100)", mutation))
                .err()
                .unwrap_or_else(|| panic!("accepted mutation: {mutation}"))
                .code
                .as_deref(),
            Some("E0401")
        );
    }
}

#[test]
fn returned_factory_targets_preserve_precise_dependencies() {
    let factory = r#"
func wrap(keep: func() -> Int, discard: func() -> Int) -> func() -> Int { [move keep] () -> keep() }
func get_factory() -> func(consume func() -> Int, func() -> Int) -> func() -> Int { wrap }
"#;
    let valid = source(
        factory,
        "let factory = get_factory()\nlet reader = factory(move keep, discard)",
        "",
        "reader()",
    );
    foster::compile(&valid).unwrap();
    assert_eq!(
        foster::run(&valid.replace("    reader()", "    right.push(100)\nreader()")).unwrap(),
        Value::Integer(21)
    );
    let invalid = valid.replace("    reader()", "    left.push(100)\nreader()");
    assert_eq!(
        foster::compile(&invalid).unwrap_err().code.as_deref(),
        Some("E0401")
    );
}

#[test]
fn unknown_factory_parameters_keep_all_possible_dependencies() {
    let factory = r#"
func wrap(keep: func() -> Int, discard: func() -> Int) -> func() -> Int { [move keep] () -> keep() }
func invoke(factory: func(consume func() -> Int, func() -> Int) -> func() -> Int, keep: func() -> Int, discard: func() -> Int) -> func() -> Int {
    factory(move keep, discard)
}
"#;
    let valid = source(
        factory,
        "let reader = invoke(wrap, move keep, discard)",
        "",
        "reader()",
    );
    foster::compile(&valid).unwrap();
    for mutation in ["left.push(100)", "right.push(100)"] {
        let invalid = valid.replace("    reader()", &format!("    {mutation}\nreader()"));
        assert_eq!(
            foster::compile(&invalid)
                .err()
                .unwrap_or_else(|| panic!("accepted {mutation}"))
                .code
                .as_deref(),
            Some("E0401")
        );
    }
}

#[test]
fn generic_and_aggregate_parameters_preserve_hidden_borrowers() {
    let factory = r#"
type Box<T> = { value: T }
func choose<T>(keep: T, discard: T) -> T { keep }
func boxed(keep: Box<func() -> Int>, discard: Box<func() -> Int>) -> Box<func() -> Int> { keep }
"#;
    for (setup, result) in [
        ("let reader = choose(move keep, discard)", "reader()"),
        (
            "let chosen = boxed(Box { value: move keep }, Box { value: move discard })",
            "chosen.value()",
        ),
        (
            "let chosen = choose(Box { value: move keep }, Box { value: move discard })",
            "chosen.value()",
        ),
    ] {
        let valid = source(factory, setup, "right.push(100)", result);
        assert_eq!(foster::run(&valid).unwrap(), Value::Integer(21));
        assert_eq!(
            foster::compile(&valid.replace("right.push(100)", "left.push(100)"))
                .expect_err("captured origin must survive")
                .code
                .as_deref(),
            Some("E0401")
        );
    }
}
