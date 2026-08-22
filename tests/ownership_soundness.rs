use std::panic::{AssertUnwindSafe, catch_unwind};

#[test]
fn ownership_rules_have_indexed_compile_pass_and_compile_fail_witnesses() {
    let pass = [
        (
            "rule-2-parent-read-and-last-use",
            r#"
func main() -> Int {
    values = [10, 20]
    parent = ref values[0]
    child = ref parent
    parent
    child
    moved = move parent
    moved
}
"#,
        ),
        (
            "rule-4-mutually-exclusive-paths",
            r#"
func observe(value: Int) { }
func main() -> Int {
    values = [10, 20]
    selected = ref values[0]
    branch {
        true -> values.push(30)
        _ -> observe(selected)
    }
    0
}
"#,
        ),
        (
            "rule-5-frame-loan-across-await",
            r#"
type Worker = {}
func value(self: Worker) -> Int { 1 }
func wait(worker: Remote<Worker>) -> Int {
    values = [10]
    selected = ref values[0]
    waited = await worker.value()
    selected + waited
}
func main() { 0 }
"#,
        ),
        (
            "rule-9-disjoint-fields",
            r#"
type Pair = {
    left: List<Int>
    right: List<Int>
}
func main() -> Int {
    pair = Pair { left: [10], right: [20] }
    selected = ref pair.left[0]
    pair.right.push(30)
    selected
}
"#,
        ),
        (
            "rule-9-constant-indices",
            r#"
func main() -> Int {
    values = [10, 20]
    first = ref values[0]
    second = move values[1]
    first + second
}
"#,
        ),
    ];
    for (rule, source) in pass {
        foster::compile(source).unwrap_or_else(|error| panic!("{rule} should pass: {error:?}"));
    }

    let fail = [
        (
            "rule-1-owner-invalidation",
            r#"
func main() -> Int {
    values = [10]
    selected = ref values[0]
    values.push(20)
    selected
}
"#,
        ),
        (
            "rule-2-child-outlives-source",
            r#"
func main() -> Int {
    values = [10]
    parent = ref values[0]
    child = ref parent
    moved = move parent
    child
}
"#,
        ),
        (
            "rule-3-local-return-escape",
            r#"
func invalid() -> Int {
    values = [10]
    ref values[0]
}
func main() { 0 }
"#,
        ),
        (
            "rule-4-invalid-on-one-incoming-path",
            r#"
func main() -> Int {
    values = [10]
    selected = ref values[0]
    branch {
        true -> values.push(20)
        _ -> ()
    }
    selected
}
"#,
        ),
    ];
    for (rule, source) in fail {
        assert!(
            foster::compile(source).is_err(),
            "{rule} should be rejected"
        );
    }
}

#[test]
fn equivalent_cfg_rewrites_keep_the_same_ownership_decision() {
    let linear = r#"
func main() -> Int {
    values = [10]
    selected = ref values[0]
    values.push(20)
    selected
}
"#;
    let split = r#"
func main() -> Int {
    values = [10]
    selected = ref values[0]
    branch {
        true -> values.push(20)
        _ -> values.push(20)
    }
    selected
}
"#;
    assert_eq!(
        foster::compile(linear).is_ok(),
        foster::compile(split).is_ok()
    );
}

#[test]
fn bounded_compiler_input_fuzzing_never_panics() {
    const TOKENS: &[&str] = &[
        "func", "main", "(", ")", "{", "}", "[", "]", "ref", "move", "await", "branch", "->", "=",
        ":", ",", ".", "Int", "List", "0", "1", "true", "_", "value", "\n",
    ];
    let mut state = 0x4d59_5df4_d0f3_3173u64;
    for case in 0..256 {
        let mut source = String::new();
        for _ in 0..32 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            source.push_str(TOKENS[state as usize % TOKENS.len()]);
            source.push(' ');
        }
        assert!(
            catch_unwind(AssertUnwindSafe(|| foster::compile(&source).is_ok())).is_ok(),
            "compiler panicked for generated case {case}: {source}"
        );
    }
}

#[test]
fn ownership_dump_and_diagnostics_are_deterministic() {
    let source = r#"
func preserve[g: group Int](value: ref[g] Int) -> ref[g] Int { ref value }
func main() { 0 }
"#;
    let first = foster::compile(source).unwrap();
    let second = foster::compile(source).unwrap();
    let first_dump = first.ownership.debug_dump(&first.hir);
    let second_dump = second.ownership.debug_dump(&second.hir);
    assert_eq!(first_dump, second_dump);
    assert!(first_dump.contains("foster-language=1 ownership-model=1"));
    assert!(first_dump.contains("loan L"));
    assert!(first_dump.contains("region L"));

    let invalid = r#"
func main() -> Int {
    values = [10]
    selected = ref values[0]
    values.push(20)
    selected
}
"#;
    assert_eq!(
        format!("{:?}", foster::compile(invalid).unwrap_err()),
        format!("{:?}", foster::compile(invalid).unwrap_err())
    );
}

#[test]
fn ownership_compatibility_surface_is_stable() {
    assert_eq!(foster::ownership::LANGUAGE_VERSION, 1);
    assert_eq!(foster::ownership::MODEL_VERSION, 1);
    assert_eq!(
        foster::ownership::diagnostics::CATALOG
            .iter()
            .map(|(code, _)| *code)
            .collect::<Vec<_>>(),
        vec!["E0382", "E0401", "E0402", "E0403", "E0507", "E0728"]
    );
}
