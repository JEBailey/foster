#[test]
fn type_branch_scope_and_exhaustiveness() {
    for source in [
        "import core.copy\nfunc bad<T>(value: T) -> T { branch value { is Copy -> value.copy() } }",
        "import core.copy\nfunc bad<T>(value: T) -> T { branch value { is Copy -> value.copy()\n_ -> value.copy() } }",
        "import core.copy\nfunc bad<T>(value: T) -> T { branch value { is Copy -> ()\n_ -> () }\nvalue.copy() }",
        "func bad<T>(value: T) -> Int { branch value { is Missing -> 1\n_ -> 0 } }",
        "func bad<T>(value: T) -> Int { branch value { is List<Int> -> 1\n_ -> 0 } }",
        "import core.copy\nimport core.option\nfunc bad<T>(value: Option<T>) -> Int { branch value { Option.Some(is Copy) -> 1\n_ -> 0 } }",
    ] {
        assert!(
            foster::compile(source).is_err(),
            "unexpectedly accepted: {source}"
        );
    }
}

#[test]
fn type_branch_does_not_grant_ownership_of_a_borrowed_parameter() {
    let source = "type Item = { value: Int }\nfunc bad<T>(value: T) -> Item [read value] { branch value { is Item -> value\n_ -> Item { value: 0 } } }";
    assert!(foster::compile(source).is_err());
}

#[test]
fn type_branch_replacement_invalidates_copy_evidence() {
    let source = "import core.copy\nfunc bad<T>(value: T, other: T) -> T { branch value { is Copy -> { value = other\nvalue.copy() }\n_ -> other } }";
    assert!(foster::compile(source).is_err());
}

#[test]
fn type_branch_preserves_the_original_place() {
    let source = include_str!("fixtures/programs/type_conformance.fos");
    let compilation = foster::compile(source).unwrap();
    let program = foster::vm::compile_with_options(
        &compilation,
        foster::vm::CompileOptions { optimize: false },
    )
    .unwrap();
    assert_eq!(
        foster::vm::Machine::new(&program).run_main().unwrap(),
        foster::vm::Value::Integer(42)
    );
}
#[test]
fn type_branch_qualified_copy_resolves_in_the_type_namespace() {
    let source = "import core.copy as copying\nfunc can<T>(value: T) -> Bool { branch value { is copying::Copy -> true\n_ -> false } }\nfunc main() -> Bool { can(42) }";
    assert_eq!(foster::run(source).unwrap(), foster::vm::Value::Bool(true));
}

#[test]
fn type_branch_format_and_bytecode_round_trip() {
    let source = include_str!("fixtures/programs/type_branch.fos");
    let formatted = foster::formatter::format(source).unwrap();
    assert_eq!(foster::formatter::format(&formatted).unwrap(), formatted);
    let compilation = foster::compile(&formatted).unwrap();
    for optimize in [false, true] {
        let program =
            foster::vm::compile_with_options(&compilation, foster::vm::CompileOptions { optimize })
                .unwrap();
        let decoded =
            foster::vm::decode_program(&foster::vm::encode_program(&program).unwrap()).unwrap();
        assert_eq!(program, decoded);
        assert_eq!(
            foster::vm::Machine::new(&decoded).run_main().unwrap(),
            foster::vm::Value::Integer(42)
        );
    }
}
