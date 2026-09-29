const SOURCE: &str = include_str!("../../tests/fixtures/programs/core_bytes.fos");

#[test]
fn core_bytes_execute_after_bytecode_round_trip() {
    let compilation = foster_compiler::compile(SOURCE).unwrap();
    foster_compiler::native::prepare(&compilation).unwrap();
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

#[test]
fn core_bytes_are_read_only_computed_values() {
    let error = foster_compiler::compile(
        "import core.int\nfunc main() -> Int { let value = 42\nvalue.bytes = 1.bytes\nvalue }",
    )
    .unwrap_err();
    assert!(error.to_string().contains("place"), "{error}");
}

#[test]
fn scalar_byte_properties_do_not_satisfy_method_contracts() {
    let result = foster_compiler::compile(
        "import core.int\nimport core.bytes\ntype Encodable = { pub func bytes(self) -> Bytes [read self] }\nfunc encode<T & Encodable>(value: T) -> Bytes [read value] { value.bytes() }\nfunc main() -> Int { encode(42).length }",
    );
    assert!(
        result.is_err(),
        "a computed property is not a method contract"
    );
}
