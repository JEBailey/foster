#[test]
fn numeric_conversions_survives_bytecode_round_trip() {
    let compilation = foster_compiler::compile(include_str!(
        "../../tests/fixtures/programs/numeric_conversions.fos"
    ))
    .unwrap();
    foster_compiler::native::prepare(&compilation).unwrap();
    for optimize in [false, true] {
        let program = foster_compiler::vm::compile_with_options(
            &compilation,
            foster_compiler::vm::CompileOptions { optimize },
        )
        .unwrap();
        let program =
            foster_vm::decode_program(&foster_compiler::vm::encode_program(&program).unwrap())
                .unwrap();
        assert_eq!(
            foster_vm::Machine::new(&program.into_verified().unwrap())
                .run_main()
                .unwrap()
                .to_string(),
            "42"
        );
    }
}

#[test]
fn numeric_conversions_checks_argument_types_and_counts() {
    for expression in [
        "Float.from(1.0)",
        "Int.from(1)",
        "1.0.round(2.0)",
        "1.0.truncate(true)",
    ] {
        let source =
            format!("import core.float\nimport core.int\nfunc main() -> () {{ {expression} }}");
        assert!(foster_compiler::compile(&source).is_err(), "{expression}");
    }
}
