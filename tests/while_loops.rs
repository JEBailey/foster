use foster::{
    ast::{Expr, Stmt, UnaryOp},
    vm::Value,
};

#[test]
fn loop_body_bindings_stay_in_the_loop() {
    for header in ["while false", "while true", "loop"] {
        let error = foster::compile(&format!(
            "func main() -> Int {{ {header} {{ let inside = 1\nbreak }}\ninside }}"
        ))
        .unwrap_err();
        assert!(error.message.contains("inside"), "{error:?}");
        let source = format!(
            "func main() -> Int {{
                let total = 0
                {header} {{ let inside = 1\ntotal = total + inside\nbreak }}
                {header} {{ let inside = 2\ntotal = total + inside\nbreak }}
                let inside = 42
                assert(total == {})
                inside
            }}",
            if header == "while false" { 0 } else { 3 }
        );
        assert_eq!(foster::run(&source).unwrap(), Value::Integer(42));
    }
}

#[test]
fn while_headers_allow_records_in_delimited_expressions() {
    let source = "type Flag = { active: Bool }
        type Index = { value: Int }
        func active(flag: Flag) -> Bool { flag.active }
        func main() -> Int {
            while active(Flag { active: false }) { assert(false) }
            while (Flag { active: false }).active { assert(false) }
            while [Flag { active: false }][0].active { assert(false) }
            while [false][Index { value: 0 }.value] { assert(false) }
            42
        }";
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));
    assert!(
        foster::parse_recovering(source)
            .unwrap()
            .diagnostics
            .is_empty()
    );
}

#[test]
fn while_matches_an_explicit_top_of_loop_guard() {
    for header in ["while visits < 5 {", "loop { break if !(visits < 5)\n"] {
        let source = format!(
            "func main() -> Int {{
                let visits = 0
                let total = 0
                {header}
                    visits = visits + 1
                    continue if visits == 2
                    total = total + visits
                }}
                assert(visits == 5)
                assert(total == 13)
                42
            }}"
        );
        let compilation = foster::compile(&source).unwrap();
        for optimize in [false, true] {
            assert_eq!(
                foster::vm::run_with_options(&compilation, foster::vm::CompileOptions { optimize })
                    .unwrap(),
                Value::Integer(42)
            );
        }
    }
}

#[test]
fn while_uses_existing_loop_and_guard_nodes() {
    let parsed = foster::parse("func main() { while false { break } }").unwrap();
    let Stmt::Loop { body } = &parsed.functions[0].body[0] else {
        panic!("while was not desugared")
    };
    let Stmt::Break { guard: Some(guard) } = &body[0] else {
        panic!("missing precondition")
    };
    assert!(
        matches!(guard.unspanned(), Expr::Unary { operator: UnaryOp::Not, operand } if matches!(operand.unspanned(), Expr::Bool(false)))
    );
    assert!(matches!(body[1], Stmt::Break { guard: None }));
}

#[test]
fn while_conditions_and_transfers_execute_in_both_optimization_modes() {
    let compilation = foster::compile(include_str!("fixtures/programs/while_loops.fos")).unwrap();
    for optimize in [false, true] {
        assert_eq!(
            foster::vm::run_with_options(&compilation, foster::vm::CompileOptions { optimize })
                .unwrap(),
            Value::Integer(42)
        );
    }
}

#[test]
fn while_requires_a_boolean_condition_and_a_body() {
    let source = "func main() { while 123 { break } }";
    let error = foster::compile(source).unwrap_err();
    assert!(error.message.contains("Bool"), "{error:?}");
    let offset = source.find("123").unwrap();
    assert!(
        error
            .labels
            .iter()
            .any(|label| label.range.start <= offset && label.range.end >= offset + 3),
        "{error:?}"
    );
    for source in [
        "func main() { while { break } }",
        "func main() { while true }",
        "func while() {}",
    ] {
        assert!(foster::parse(source).is_err(), "{source}");
    }
}

#[test]
fn while_condition_effects_are_checked_like_loop_body_effects() {
    let error = foster::compile("type Counter = { value: Int }\nfunc next(c: Counter) -> Bool { c.value = c.value + 1\nc.value < 3 }\nfunc run(c: Counter) -> () [read c] { while next(c) {} }").unwrap_err();
    assert!(error.message.contains("undeclared effect"), "{error:?}");
}
