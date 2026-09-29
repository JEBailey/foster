const SOURCE: &str = include_str!("../../tests/fixtures/programs/list_constants.fos");

#[test]
fn list_constants_preserve_native_call_layouts() {
    let compilation = foster_compiler::compile(SOURCE).unwrap();
    foster_compiler::native::prepare(&compilation).unwrap();
}

#[test]
fn list_constants_execute_after_bytecode_round_trip() {
    let compilation = foster_compiler::compile(SOURCE).unwrap();
    for optimize in [false, true] {
        let program = foster_compiler::vm::compile_with_options(
            &compilation,
            foster_compiler::vm::CompileOptions { optimize },
        )
        .unwrap();
        let decoded =
            foster_vm::decode_program(&foster_compiler::vm::encode_program(&program).unwrap())
                .unwrap();
        assert_eq!(
            foster_vm::Machine::new(&decoded.into_verified().unwrap())
                .run_main()
                .unwrap()
                .to_string(),
            "42"
        );
    }
}
