use foster::vm::{CompileOptions, Value};

#[test]
fn never_arms_do_not_constrain_branch_values() {
    let source = r#"
func die() -> Never { panic("stop") }
func assertion() -> Never { assert(false, "stop") }
func spin() -> Never { loop { continue } }
func answer(flag: Bool) -> Int {
    branch {
        flag -> 42
        _ -> die()
    }
}
func early(flag: Bool) -> Int {
    branch {
        flag -> { return 42 }
        _ -> { assert(false, "stop") }
    }
}
func main() -> Int {
    assert(answer(true) == 42)
    early(true)
}
"#;
    let compilation = foster::compile(source).unwrap();
    for optimize in [false, true] {
        assert_eq!(
            foster::vm::run_with_options(&compilation, CompileOptions { optimize }).unwrap(),
            Value::Integer(42)
        );
    }
}

#[test]
fn never_cannot_be_constructed_or_claimed_by_returning_functions() {
    for source in [
        "func bad() -> Never { () }",
        "func bad() -> Never { 42 }",
        "func bad() -> Never { return 42 }",
        "func bad(flag: Bool) -> Never { assert(flag) }",
        "func bad() -> Never { loop { break } }",
        "func bad() -> Never { loop { loop { break }\nbreak } }",
        "func bad() -> Never { panic(42) }",
        "func bad(values: List<Never>) -> List<Int> { values }",
        "func bad() -> Never { loop { return branch { true -> { break } _ -> panic(\"stop\") } } }",
    ] {
        assert!(foster::compile(source).is_err(), "accepted {source}");
    }
}

#[test]
fn reached_panic_reports_its_message() {
    let error = foster::run(
        "func fail() -> Never { panic(\"deliberate failure\") }\nfunc main() -> Int { fail() }",
    )
    .unwrap_err();
    assert!(error.to_string().contains("deliberate failure"), "{error}");
}

#[test]
fn nested_transfers_and_eager_arguments_diverge() {
    for source in [
        "func fail() -> Never { loop { loop { continue } } }",
        "func fail() -> Never { panic(\"stop\") + 1 }",
        "func fail() -> Never { [panic(\"stop\"), 42] }",
        "func take(value: Int) -> Int { value }\nfunc fail() -> Never { take(panic(\"stop\")) }",
        "func fail() -> Never { loop { branch { true -> { continue } _ -> { break } } } }",
        "func message() -> String { \"stop\" }\nfunc fail() -> Never { assert(false, message()) }",
    ] {
        let compilation =
            foster::compile(source).unwrap_or_else(|error| panic!("{source}: {error}"));
        for optimize in [false, true] {
            foster::vm::compile_with_options(&compilation, CompileOptions { optimize })
                .unwrap_or_else(|error| panic!("{source}: {error}"));
        }
    }
}

#[test]
fn never_survives_bytecode_round_trip() {
    let compilation = foster::compile(
        "func fail() -> Never { panic(\"serialized panic\") }\nfunc main() -> Int { fail() }",
    )
    .unwrap();
    let program = foster::vm::compile(&compilation).unwrap();
    let encoded = foster::vm::encode_program(&program).unwrap();
    let decoded = foster::vm::decode_program(&encoded).unwrap();
    assert!(
        decoded
            .functions
            .values()
            .any(|function| function.name == "fail"
                && function.result_type == foster::codegen::types::ExecutableType::Never)
    );
    let error = foster::vm::Machine::new(&decoded).run_main().unwrap_err();
    assert!(error.to_string().contains("serialized panic"));
}

#[test]
fn never_has_no_runtime_instances() {
    assert_eq!(
        foster::run("func main() -> Int { branch 42 { is Never -> 0 _ -> 42 } }").unwrap(),
        Value::Integer(42)
    );
}
