use foster::vm::Value;

fn program(mutation: &str, usage: &str, between: &str) -> String {
    format!(
        r#"
func choose(a: Bool, b: Bool) -> Int {{
    let values = [10]
    let selected = ref values[0]
    branch {{ {mutation} -> values.push(20)
 _ -> () }}
    {between}
    branch {{ {usage} -> selected
 _ -> 0 }}
}}
func main() -> Int {{ choose(false, true) }}
"#
    )
}

fn formula(mask: u8) -> String {
    let terms = (0..4)
        .filter(|row| mask & (1 << row) != 0)
        .map(|row| {
            format!(
                "{} && {}",
                if row & 1 != 0 { "a" } else { "not a" },
                if row & 2 != 0 { "b" } else { "not b" }
            )
        })
        .collect::<Vec<_>>();
    if terms.is_empty() {
        "false".into()
    } else {
        terms.join(" || ")
    }
}

#[test]
fn exhaustive_two_boolean_truth_tables_match_feasible_conflicts() {
    for mutation in 0..16 {
        for usage in 0..16 {
            let result = foster::compile(&program(&formula(mutation), &formula(usage), ""));
            if mutation & usage == 0 {
                result.unwrap_or_else(|error| panic!("{mutation:04b}/{usage:04b}: {error:?}"));
            } else {
                assert_eq!(
                    result.unwrap_err().code.as_deref(),
                    Some("E0401"),
                    "{mutation:04b}/{usage:04b}"
                );
            }
        }
    }
}

#[test]
fn complements_and_boolean_subjects_accept_safe_paths() {
    let source = program("a && b", "not a || not b", "");
    assert_eq!(foster::run(&source).unwrap(), Value::Integer(10));
    let subject = source
        .replace("branch { a && b ->", "branch (a && b) { true ->")
        .replace(
            "branch { not a || not b ->",
            "branch (not a || not b) { true ->",
        );
    assert_eq!(foster::run(&subject).unwrap(), Value::Integer(10));
}

#[test]
fn changed_operands_do_not_prove_disjointness() {
    for between in [
        "a = false",
        "let alias = ref a\nalias = false",
        "let change = [ref a] () -> { a = false }\nchange()",
    ] {
        let error = foster::compile(&program("a && b", "not a || not b", between)).unwrap_err();
        assert_eq!(error.code.as_deref(), Some("E0401"), "{between}: {error:?}");
    }
}

#[test]
fn scalar_comparisons_and_guarded_exits_share_compound_facts() {
    let source = program("a && b", "not a || not b", "")
        .replace("a: Bool", "a: Int")
        .replace("a && b", "a < 2 && b")
        .replace("not a || not b", "a >= 2 || not b")
        .replace("choose(false, true)", "choose(3, true)");
    assert_eq!(foster::run(&source).unwrap(), Value::Integer(10));
    let error = foster::compile(&source.replace("a >= 2 || not b", "a < 2 && b")).unwrap_err();
    assert_eq!(error.code.as_deref(), Some("E0401"));
    for exit in [
        "return 0 if a && b",
        "loop { break if not a || not b\nreturn 0 }",
        "loop { continue if a && b\nbreak }",
    ] {
        let source = program("a && b", "true", exit);
        assert_eq!(foster::run(&source).unwrap(), Value::Integer(10));
    }
}

#[test]
fn calls_and_short_circuit_operand_effects_forget_changed_facts() {
    let prelude =
        "func clear[g: group Bool](value: ref[g] Bool) -> Bool [mut g] { value = false }\n";
    let source = format!(
        "{prelude}{}",
        program("a && b", "not a || not b", "clear(ref a)")
    );
    assert_eq!(
        foster::compile(&source).unwrap_err().code.as_deref(),
        Some("E0401")
    );
    let source = format!(
        "{prelude}{}",
        program("a && not clear(ref a)", "not a || not b", "")
    );
    assert_eq!(
        foster::compile(&source).unwrap_err().code.as_deref(),
        Some("E0401")
    );
    let unrelated = format!(
        "{prelude}{}",
        program(
            "a && b",
            "not a || not b",
            "let other = true\nclear(ref other)"
        )
    );
    assert_eq!(foster::run(&unrelated).unwrap(), Value::Integer(10));
    // The call is unreachable, so it must not erase a's facts or run.
    let source = format!(
        "{prelude}{}",
        program("a && b", "not a || not b", "false && clear(ref a)")
    );
    assert_eq!(foster::run(&source).unwrap(), Value::Integer(10));
}

#[test]
fn loop_backedges_and_changed_indices_remain_conservative() {
    let source = r#"
func choose(a: Bool, b: Bool) -> Int {
    let values = [10]
    let selected = ref values[0]
    loop {
        branch { a && b -> values.push(20)
 _ -> () }
        branch { not a || not b -> { return selected }
 _ -> () }
        a = false
    }
    0
}
func main() -> Int { choose(true, true) }
"#;
    let error = foster::compile(source).unwrap_err();
    assert_eq!(error.code.as_deref(), Some("E0401"), "{error:?}");
    let source = program("a && b", "not a || not b", "")
        .replace("a: Bool", "a: Int")
        .replace("a && b", "a < 2 && b")
        .replace("not a || not b", "a >= 2 || not b")
        .replace("choose(false, true)", "choose(1, true)");
    let source = source.replace("branch { a >=", "a = 3\nbranch { a >=");
    assert_eq!(
        foster::compile(&source).unwrap_err().code.as_deref(),
        Some("E0401")
    );
}

#[test]
fn computed_index_predicates_do_not_retain_stale_facts() {
    let source = program(
        "flags[index] && b",
        "not flags[index] || not b",
        "index = 1",
    )
    .replace(
        "let values =",
        "let flags = [true, false]\nlet index = 0\nlet values =",
    );
    let error = foster::compile(&source).unwrap_err();
    assert_eq!(error.code.as_deref(), Some("E0401"), "{error:?}");
    foster::compile(&source.replace("-> selected", "-> 0")).unwrap();
}

#[test]
fn mutations_to_any_parameter_in_a_shared_group_forget_facts() {
    let prelude = "func clear_second[g: group Bool](first: ref[g] Bool, second: ref[g] Bool) -> Bool [mut g] { second = false }\n";
    let source = format!(
        "{prelude}{}",
        program(
            "a && b",
            "not a || not b",
            "clear_second(ref other_alias, ref alias)"
        )
        .replace(
            "let values =",
            "let alias = ref a\nlet other = true\nlet other_alias = ref other\nlet values ="
        )
    );
    let error = foster::compile(&source).unwrap_err();
    assert_eq!(error.code.as_deref(), Some("E0401"), "{error:?}");
    foster::compile(&source.replace("-> selected", "-> 0")).unwrap();
}

#[test]
fn existing_aliases_and_reborrows_cannot_preserve_mutated_predicates() {
    for between in [
        "alias = false",
        "nested = false",
        "clear(ref alias)",
        "change()",
    ] {
        let prelude =
            "func clear[g: group Bool](value: ref[g] Bool) -> Bool [mut g] { value = false }\n";
        let source = format!("{prelude}{}", program("a && b", "not a || not b", between)
            .replace("let values =", "let alias = ref a\nlet nested = ref alias\nlet change = [ref a] () -> { a = false }\nlet values ="));
        let error = foster::compile(&source).unwrap_err();
        assert_eq!(error.code.as_deref(), Some("E0401"), "{between}: {error:?}");
        foster::compile(&source.replace("-> selected", "-> 0")).unwrap();
    }
}
