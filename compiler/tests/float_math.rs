#[test]
fn float_math_survives_bytecode_round_trip() {
    let compilation =
        foster_compiler::compile(include_str!("../../tests/fixtures/programs/float_math.fos"))
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
fn float_math_checks_argument_types_and_counts() {
    for expression in [
        "1.0.atan2()",
        "1.0.atan2(1)",
        "1.0.sqrt(2.0)",
        "1.0.sin(true)",
    ] {
        let source = format!("import core.float\nfunc main() -> Float {{ {expression} }}");
        assert!(foster_compiler::compile(&source).is_err(), "{expression}");
    }
}
