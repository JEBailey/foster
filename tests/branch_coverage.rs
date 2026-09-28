#[test]
fn expanded_branch_coverage_executes_all_cases() {
    let compilation =
        foster::compile(include_str!("fixtures/programs/branch_coverage.fos")).unwrap();
    for optimize in [false, true] {
        let program =
            foster::vm::compile_with_options(&compilation, foster::vm::CompileOptions { optimize })
                .unwrap();
        assert_eq!(
            foster::vm::Machine::new(&program)
                .run_main()
                .unwrap()
                .to_string(),
            "42"
        );
    }
}

#[test]
fn expanded_branch_coverage_reports_missing_boolean_and_nested_case() {
    for (source, missing) in [
        (
            "func f(value: Bool) -> Int { branch value { true -> 1 } }",
            "false",
        ),
        (
            "enum Maybe<T> = Some(T) | None\nfunc f(value: Maybe<Bool>) -> Int { branch value { Maybe.Some(true) -> 1\nMaybe.None -> 0 } }",
            "Maybe.Some(false)",
        ),
    ] {
        let error = foster::compile(source).unwrap_err();
        assert!(
            error
                .message
                .contains(&format!("missing pattern: {missing}")),
            "{error}"
        );
    }
}

#[test]
fn expanded_branch_coverage_preserves_field_correlations() {
    let error = foster::compile(
        r#"
type Flags = { first: Bool, second: Bool }
func f(value: Flags) -> Int {
    branch value {
        { first: true, second: true } -> 1
        { first: false, second: false } -> 0
    }
}
"#,
    )
    .unwrap_err();
    assert!(
        error.message.contains("{ first: false, second: true }"),
        "{error}"
    );
}

#[test]
fn expanded_branch_coverage_keeps_open_domains_and_type_tests_conservative() {
    for source in [
        "func f(value: Int) -> Int { branch value { 0 -> 0\n1 -> 1 } }",
        "enum Maybe<T> = Some(T) | None\nfunc f(value: Maybe<Int>) -> Int { branch value { Maybe.Some(0) -> 1\nMaybe.None -> 0 } }",
        "func f(value: Bool) -> Int { branch value { is Bool -> 1 } }",
    ] {
        let error = foster::compile(source).unwrap_err();
        assert!(error.message.contains("missing pattern:"), "{error}");
    }
}

#[test]
fn expanded_branch_coverage_handles_recursive_payloads_without_expanding_wildcards() {
    foster::compile(
        r#"
enum Chain = End | Next(Link)
type Link = { child: Chain, flag: Bool }
func f(value: Chain) -> Int {
    branch value {
        Chain.End -> 0
        Chain.Next({ child: _, flag: true }) -> 1
        Chain.Next({ flag: false }) -> 2
    }
}
"#,
    )
    .unwrap();
}

#[test]
fn expanded_branch_coverage_checks_initialization_in_every_payload_arm() {
    let source = r#"
enum Choice = Yes(Bool) | No
type Output = { value: Int }
func f(choice: Choice) -> Int {
    let output = Output { value: ?? }
    branch choice {
        Choice.Yes(true) -> { output.value = 1 }
        Choice.Yes(false) -> { output.value = 2 }
        Choice.No -> { output.value = 3 }
    }
    output.value
}
func main() -> Int { f(Choice.Yes(false)) }
"#;
    assert_eq!(foster::run(source).unwrap().to_string(), "2");
    let invalid = source.replace("output.value = 2", "()");
    assert!(
        foster::compile(&invalid).is_err(),
        "uninitialized payload arm must remain reachable"
    );
}

#[test]
fn expanded_branch_coverage_accepts_only_complete_boolean_truth_tables() {
    for mask in 0u8..16 {
        let mut arms = String::new();
        for index in 0..4 {
            if mask & (1 << index) != 0 {
                arms.push_str(&format!(
                    "{{ first: {}, second: {} }} -> 1\n",
                    index & 2 != 0,
                    index & 1 != 0
                ));
            }
        }
        let source = format!(
            "type Flags = {{ first: Bool, second: Bool }}\nfunc f(value: Flags) -> Int {{ branch value {{ {arms} }} }}"
        );
        assert_eq!(
            foster::compile(&source).is_ok(),
            mask == 15,
            "truth table {mask:04b}"
        );
    }
}
