use foster::vm::Value;

#[test]
fn named_scopes_yield_values_and_keep_names_local() {
    assert_eq!(
        foster::run(
            r#"
func main() -> Int {
    let answer = :build {
        let intermediate = 40
        :finish { intermediate + 2 }
    }
    let intermediate = 1
    assert(:empty {} == ())
    assert(:binding { let hidden = 1 } == ())
    assert(:request == :request)
    assert(:repeat { :repeat { 42 } } == 42)
    assert(:text { "hello".length } == 5)
    branch :request {
        :request -> answer
        _ -> intermediate
    }
}
"#
        )
        .unwrap(),
        Value::Integer(42)
    );
    for source in [
        "func main() { :work { let hidden = 42 }\nhidden }",
        "func main() { :work {}\nwork }",
        "func main() { :work { break } }",
    ] {
        assert!(foster::compile(source).is_err(), "{source}");
    }
}

#[test]
fn named_scopes_reject_borrowers_of_expired_locals() {
    for (result, usage) in [
        ("ref local", "escaped.value"),
        ("[(ref local)]", "escaped[0].value"),
        ("[ref local] () -> local.value", "escaped()"),
    ] {
        let source = format!(
            "type Box = {{ value: Int }}\nfunc main() {{\nlet escaped = :work {{\nlet local = Box {{ value: 42 }}\n{result}\n}}\n{usage}\n}}"
        );
        let error =
            foster::compile(&source).expect_err(&format!("accepted escaped borrower: {source}"));
        assert_eq!(error.code.as_deref(), Some("E0401"), "{error:?}");
    }
    let source = r#"
type Box = { value: Int }
func main() -> Int {
    let outer = Box { value: 1 }
    let escaped = [(ref outer)]
    :work {
        let local = Box { value: 42 }
        escaped = [(ref local)]
    }
    escaped[0].value
}
"#;
    let error = foster::compile(source).unwrap_err();
    assert_eq!(error.code.as_deref(), Some("E0401"), "{error:?}");
}

#[test]
fn named_scopes_keep_effects_and_symbol_headers() {
    let source = r#"
type Box = { value: Int }
func update(value: ref[value] Box) -> () [mut value.value] {
    :work { value.value = 42 }
}
func main() -> Int {
    let value = Box { value: 0 }
    update(ref value)
    while :stop == :go { break }
    branch (:result { value.value }) {
        42 -> 42
        _ -> 0
    }
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));
    assert!(foster::compile(&source.replace("[mut value.value]", "[read value.value]")).is_err());
}

#[test]
fn named_scopes_preserve_outer_borrows_and_moves() {
    assert_eq!(
        foster::run(
            r#"
type Box = { value: Int }
func main() -> Int {
    let outer = Box { value: 42 }
    let borrowed = :work { ref outer }
    let text = :build {
        let local = "hello"
        move local
    }
    assert(text.length == 5)
    borrowed.value
}
"#
        )
        .unwrap(),
        Value::Integer(42)
    );
}

#[test]
fn named_scopes_format_without_losing_labels() {
    let source = "func main() {\n:request {\n// resource scope\n:child { 42 }\n}\n}\n";
    let formatted = foster::formatter::format(source).unwrap();
    assert_eq!(
        formatted,
        "func main() {\n    :request {\n        // resource scope\n        :child { 42 }\n    }\n}\n"
    );
    assert_eq!(foster::formatter::format(&formatted).unwrap(), formatted);
}
