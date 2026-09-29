#[test]
fn unit_results_accept_explicit_unit_bodies() {
    let compilation = foster_compiler::compile(include_str!(
        "../../tests/fixtures/programs/explicit_unit_results.fos"
    ))
    .unwrap();
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
fn unit_results_keep_explicit_returns_strict() {
    for source in [
        "func invalid() -> () { return 42 }",
        "func invalid(flag: Bool) -> () { return 42 if flag }",
        "func invalid(flag: Bool) -> () { branch flag { true -> { return 42 }\nfalse -> () } }",
        "func invalid() -> Int { () }",
        "test \"non-unit test\" { 42 }",
    ] {
        assert!(foster_compiler::compile(source).is_err(), "{source}");
    }
}

#[test]
fn unit_results_do_not_skip_body_checks() {
    for source in [
        "func invalid() -> () { missing() }",
        "func invalid() -> () { 1 + true }",
        "type Counter = { value: Int }\nfunc invalid(counter: Counter) -> () [read counter] { counter.value = 1 }",
        "func consume(text: String) -> Int [consume text] { 1 }\nfunc invalid() -> () { let text = \"owned\"\nconsume(move text)\nconsume(move text) }",
        "func needs_unit(action: func() -> ()) -> () { action() }\nfunc invalid() -> () { needs_unit(() -> 42) }",
    ] {
        assert!(foster_compiler::compile(source).is_err(), "{source}");
    }
}

#[test]
fn unit_results_preserve_empty_bodies_and_divergence() {
    for source in [
        "func empty() -> () {}",
        "func stopped() -> () { panic(\"stopped\") }",
        "func stopped() -> Never { panic(\"stopped\") }",
        "func returned() -> () { return () }",
        "func empty() {}\nfunc uses() -> () { empty() }",
    ] {
        foster_compiler::compile(source).unwrap();
    }
}
