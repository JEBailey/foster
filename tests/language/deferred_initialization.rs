use foster::vm::{CompileOptions, Value};

#[test]
fn deferred_fields_initialize_on_all_paths() {
    let source = r#"
type Pair<T> = { first: T, second: Int }
func build(flag: Bool) -> Pair<Int> {
    let pair = Pair { first: ??, second: ?? }
    branch {
        flag -> { pair.first = 20 }
        _ -> { pair.first = 21 }
    }
    pair.second = 42 - pair.first
    pair
}
func main() -> Int {
    let result = build(true)
    result.first + result.second
}
"#;
    for optimize in [false, true] {
        assert_eq!(
            foster::run_with_options(source, CompileOptions { optimize }).unwrap(),
            Value::Integer(42)
        );
    }
    assert!(foster::formatter::format(source).unwrap().contains("??"));
}

#[test]
fn incomplete_records_cannot_be_observed_or_hidden() {
    for body in [
        "let p = Pair { first: ??, second: 2 }\np.first",
        "let p = Pair { first: ??, second: 2 }\np",
        "let p = Pair { first: ??, second: 2 }\nobserve(p)",
        "let p = Pair { first: ??, second: 2 }\nlet q = move p\n0",
        "let p = Pair { first: ??, second: 2 }\nlet q = ref p\n0",
        "let p = Pair { first: ??, second: 2 }\nwhile flag { p.first = 40 }\np.first",
        "let p = Pair { first: ??, second: 2 }\nlet q = [p]\n0",
        "let p = Pair { first: ??, second: 2 }\nlet f = [move p] () -> p.first\n0",
        "let p = Pair { first: ??, second: 2 }\nbranch { flag -> { p.first = 40 }\n_ -> {} }\np.first",
        "observe(Pair { first: ??, second: 2 })",
        "Pair { first: ??, second: 2 }",
        "let nested = Box { value: Pair { first: ??, second: 2 } }\n0",
    ] {
        let source = format!(
            "type Pair = {{ first: Int, second: Int }}\ntype Box = {{ value: Pair }}\nfunc observe(p: Pair) -> Int {{ p.second }}\nfunc main(flag: Bool) {{ {body} }}"
        );
        let error = foster::compile(&source).expect_err(body).to_string();
        assert!(error.contains("deferred"), "{body}: {error}");
    }
}

#[test]
fn deferred_marker_is_only_a_record_initializer() {
    for source in [
        "func main() { ?? }",
        "func main() { let x = ??\nx }",
        "func main() { [??] }",
    ] {
        assert!(foster::parse(source).is_err(), "{source}");
    }
    assert!(
        foster::compile(
            "type Pair = { first: Int, second: Int }\nfunc main() { Pair { first: 1 } }"
        )
        .is_err()
    );
}

#[test]
fn deferred_fields_are_not_default_values() {
    let source = "type Item = { value: Int }\nfunc main() -> Int { let item = Item { value: ?? }\nitem.value = 42\nitem.value }";
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));
}

#[test]
fn initialized_fields_remain_accessible_and_assignment_can_be_repeated() {
    let source = r#"
type Pair = { first: Int, second: Int }
func main() -> Int {
    let pair = Pair { first: 1, second: ?? }
    pair.first = pair.first + 1
    let total = 0
    for value in [20, 40] {
        pair.second = value
        total = pair.first + pair.second
    }
    total
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));
}

#[test]
fn cannot_initialize_inside_an_uninitialized_parent() {
    let error = foster::compile("type Inner = { value: Int }\ntype Outer = { inner: Inner }\nfunc main() { let outer = Outer { inner: ?? }\nouter.inner.value = 42\n0 }").unwrap_err().to_string();
    assert!(error.contains("deferred field `inner`"), "{error}");
}
