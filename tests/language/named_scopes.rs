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
        "func main() {\n    :request {\n        // resource scope\n        :child {\n            42\n        }\n    }\n}\n"
    );
    assert_eq!(foster::formatter::format(&formatted).unwrap(), formatted);
}

#[test]
fn labelled_exits_resolve_the_nearest_scope_and_leave_nested_loops() {
    let source = r#"
func main() -> Int {
    let result = 0
    let unit = :outer {
        :outer {
            break outer if false
            result = 1
            break outer
            result = 100
        }
        result = result + 1
        loop {
            :inner {
                loop {
                    break outer if result == 2
                    break
                }
            }
            result = 100
            break
        }
        result = 100
    }
    assert(unit == ())
    result + 40
}
"#;
    let compilation = foster::compile(source).unwrap();
    for optimize in [false, true] {
        assert_eq!(
            foster::vm::run_with_options(&compilation, foster::vm::CompileOptions { optimize })
                .unwrap(),
            Value::Integer(42)
        );
    }
    let formatted = foster::formatter::format(source).unwrap();
    assert!(formatted.contains("break outer if result == 2"));
    assert_eq!(foster::run(&formatted).unwrap(), Value::Integer(42));
    assert_eq!(foster::formatter::format(&formatted).unwrap(), formatted);
}

#[test]
fn labelled_exits_reject_unknown_labels_and_crossing_function_boundaries() {
    for source in [
        "func main() { break missing }",
        "func main() { :work { break missing } }",
        "func main() { :work {}\nbreak work }",
        "func main() { :work { let callback = [] () -> { break work } } }",
        "func main() { :work { func inner() { break work } } }",
        "func main() { let result = :work { break work if true\n42 }\nresult }",
    ] {
        assert!(foster::compile(source).is_err(), "{source}");
    }
}

#[test]
fn labelled_exit_destroys_borrow_origins_and_retains_outer_storage() {
    let source = r#"
type Box = { value: Int }
func main() -> Int {
    let outer = Box { value: 42 }
    let borrower = [(ref outer)]
    :work {
        let inner = Box { value: 1 }
        borrower = [(ref inner)]
        break work
    }
    borrower[0].value
}
"#;
    let error = foster::compile(source).unwrap_err();
    assert_eq!(error.code.as_deref(), Some("E0401"), "{error:?}");
    assert_eq!(
        foster::run(&source.replace(
            "borrower = [(ref inner)]",
            "assert(borrower[0].value == 42)"
        ))
        .unwrap(),
        Value::Integer(42)
    );
}

#[test]
fn guarded_scope_exits_evaluate_once_and_bare_transfers_keep_their_loop() {
    assert_eq!(
        foster::run(
            r#"
func main() -> Int {
    let count = 0
    :work {
        break work if (:condition { count = count + 1
false })
        let iteration = 0
        loop {
            iteration = iteration + 1
            :inner {
                continue if iteration == 1
                break
            }
        }
        assert(iteration == 2)
        count = count + 41
    }
    count
}
"#
        )
        .unwrap(),
        Value::Integer(42)
    );
}

#[test]
fn labelled_exit_preserves_temporaries_of_the_enclosing_expression() {
    let source = r#"
type Box = { value: Int }
func observe(value: Box, marker: ()) -> Int { value.value }
func main() -> Int {
    observe(Box { value: 42 }, :work {
        let inner = Box { value: 1 }
        break work
    })
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));
}
