use foster::{
    ast::{Expr, Stmt, UnaryOp},
    vm::Value,
};

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
