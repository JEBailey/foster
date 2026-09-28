#[test]
fn enum_parameters_execute_and_round_trip() {
    let compilation = foster_compiler::compile(include_str!(
        "../../tests/fixtures/programs/enum_parameters.fos"
    ))
    .unwrap();
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
            foster_vm::Machine::new(&decoded)
                .run_main()
                .unwrap()
                .to_string(),
            "42"
        );
    }
}

#[test]
fn enum_parameters_check_arity_types_and_try() {
    for (source, diagnostic) in [
        (
            "enum E = Pair(Int, Bool)\nfunc f() { E.Pair(1) }",
            "expects 2",
        ),
        (
            "enum E = Pair(Int, Bool)\nfunc f() { E.Pair(1, true, 3) }",
            "expects 2",
        ),
        (
            "enum E = Pair(Int, Bool)\nfunc f(e: E) { branch e { E.Pair(x) -> x } }",
            "expects 2 payload",
        ),
        (
            "enum E = Pair(Int, Bool)\nfunc f(e: E) { branch e { E.Pair(x, y, z) -> x } }",
            "expects 2 payload",
        ),
        (
            "enum E = Pair(Int, Bool)\nfunc f(e: E) -> E [consume e] { try<Pair> move e }",
            "cannot unwrap multiple",
        ),
        (
            "enum E = Pair(Bool, Bool)\nfunc f(e: E) { branch e { E.Pair(true, true) -> 1\nE.Pair(false, false) -> 0 } }",
            "non-exhaustive",
        ),
    ] {
        let error = foster_compiler::compile(source).unwrap_err();
        assert!(error.message.contains(diagnostic), "{error}");
    }
    for source in [
        "enum E = Pair(Int, Bool)\nfunc f() { E.Pair(true, 1) }",
        "enum E = Pair(Int, Bool)\nfunc f(e: E) { branch e { E.Pair(_, 1) -> 0\n_ -> 1 } }",
        "enum E = Good(Int) | Bad(Int, Bool)\nenum F = Good(Int) | Bad(Int)\nfunc f(e: E) -> F [consume e] { F.Good(try<Good> move e) }",
        "enum E = Good(Int) | Bad(Int, Bool)\nenum F = Good(Int) | Bad(Int, Int)\nfunc f(e: E) -> F [consume e] { F.Good(try<Good> move e) }",
    ] {
        assert!(foster_compiler::compile(source).is_err(), "{source}");
    }
}

#[test]
fn enum_parameters_preserve_ownership() {
    for source in [
        "enum E = Pair(String, String)\nfunc f() { let text = \"owned\"\nE.Pair(move text, move text) }",
        "enum E = Pair(Int, Int)\nfunc f() { let e = E.Pair(1, 2)\nbranch e { E.Pair(_, second) -> ref second } }",
        "enum E = Pair(Int, Int)\nfunc second(e: E) -> ref[e] Int { branch e { E.Pair(_, b) -> ref b } }\nfunc read(v: ref[v] Int) -> Int { v }\nfunc main() -> Int { let e = E.Pair(1, 2)\nlet b = second(e)\ne = E.Pair(3, 4)\nread(b) }",
    ] {
        assert!(foster_compiler::compile(source).is_err(), "{source}");
    }
}

#[test]
fn enum_parameters_coverage_preserves_correlations() {
    let patterns = ["false, false", "false, true", "true, false", "true, true"];
    for mask in 1..16 {
        let arms = patterns
            .iter()
            .enumerate()
            .filter(|(index, _)| mask & (1 << index) != 0)
            .map(|(_, pattern)| format!("E.Pair({pattern}) -> 42"))
            .collect::<Vec<_>>()
            .join("\n");
        let source =
            format!("enum E = Pair(Bool, Bool)\nfunc f(e: E) -> Int {{ branch e {{ {arms} }} }}");
        assert_eq!(
            foster_compiler::compile(&source).is_ok(),
            mask == 15,
            "{source}"
        );
    }
}
