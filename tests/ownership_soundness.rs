use std::panic::{AssertUnwindSafe, catch_unwind};

#[test]
fn ownership_rules_have_indexed_compile_pass_and_compile_fail_witnesses() {
    let pass = [
        (
            "rule-2-parent-read-and-last-use",
            r#"
func main() -> Int {
    let values = [10, 20]
    let parent = ref values[0]
    let child = ref parent
    parent
    child
    let moved = move parent
    moved
}
"#,
        ),
        (
            "rule-4-mutually-exclusive-paths",
            r#"
func observe(value: Int) { }
func main() -> Int {
    let values = [10, 20]
    let selected = ref values[0]
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
func Worker.value(self: Worker) -> Int { 1 }
func wait(worker: Remote<Worker>) -> Int {
    let values = [10]
    let selected = ref values[0]
    let waited = await worker.value()
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
    let pair = Pair { left: [10], right: [20] }
    let selected = ref pair.left[0]
    pair.right.push(30)
    selected
}
"#,
        ),
        (
            "rule-9-constant-indices",
            r#"
func main() -> Int {
    let values = [10, 20]
    let first = ref values[0]
    let second = move values[1]
    first + second
}
"#,
        ),
        (
            "model-3-constant-list-aggregate-indices",
            r#"
func main() -> Int {
    let left = [10]
    let right = [20]
    let left_value = ref left[0]
    let right_value = ref right[0]
    let left_callback = [ref left_value] () -> left_value
    let right_callback = [ref right_value] () -> right_value
    let callbacks = [(move left_callback), (move right_callback)]
    left.push(30)
    callbacks[1]()
}
"#,
        ),
        (
            "mutable-ref-parameter-writes-through-to-caller",
            r#"
type Person = { name: Int }
func rename[people: group Person](person: ref[people] Person, name: Int) -> () [mut people.name] {
    person.name = name
    ()
}
func main() -> Int {
    let person = Person { name: 0 }
    rename(ref person, 42)
    person.name
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
    let values = [10]
    let selected = ref values[0]
    values.push(20)
    selected
}
"#,
        ),
        (
            "rule-2-child-outlives-source",
            r#"
func main() -> Int {
    let values = [10]
    let parent = ref values[0]
    let child = ref parent
    let moved = move parent
    child
}
"#,
        ),
        (
            "rule-3-local-return-escape",
            r#"
func invalid() -> Int {
    let values = [10]
    ref values[0]
}
func main() { 0 }
"#,
        ),
        (
            "rule-4-invalid-on-one-incoming-path",
            r#"
func main() -> Int {
    let values = [10]
    let selected = ref values[0]
    branch {
        true -> values.push(20)
        _ -> ()
    }
    selected
}
"#,
        ),
        (
            "replace-invalidates-parentless-loan",
            r#"
func main() -> Int {
    let values = [1]
    let selected = ref values[0]
    values = [9]
    selected
}
"#,
        ),
        (
            "replace-invalidates-captured-call-effect",
            r#"
func make[state: group Int](value: ref[state] Int) -> func() -> Int [read state] {
    [ref value] () -> [read state] { value }
}
func main() -> Int {
    let values = [1]
    let probe = make(ref values[0])
    values = [9]
    probe()
}
"#,
        ),
        (
            "replace-through-parameter-invalidates-derived-loan",
            r#"
func replace[g: group Int](value: ref[g] Int) -> Int [mut g] {
    let first = ref value
    value = 42
    first
}
func main() -> Int {
    let values = [1]
    replace(ref values[0])
}
"#,
        ),
        (
            "model-3-variant-pattern-payload-provenance",
            r#"
enum Wrapped = Callback(func() -> Int)
func main() -> Int {
    let values = [10]
    let selected = ref values[0]
    let wrapped = Wrapped.Callback([ref selected] () -> selected)
    branch wrapped {
        Wrapped.Callback(callback) -> {
            values.push(20)
            callback()
        }
    }
}
"#,
        ),
    ];
    for (rule, source) in fail {
        let error = match foster::compile(source) {
            Err(error) => error,
            Ok(_) => panic!("{rule} should be rejected"),
        };
        if rule.starts_with("replace-") {
            assert_eq!(
                error.code.as_deref(),
                Some("E0401"),
                "{rule} should be rejected as an invalidated loan: {error:?}"
            );
            assert!(
                error
                    .labels
                    .iter()
                    .any(|label| label.message.contains("replaces")),
                "{rule} should identify the replacing operation: {error:?}"
            );
        }
        if rule == "replace-invalidates-captured-call-effect" {
            assert!(
                error
                    .message
                    .contains("closure `probe` is no longer callable"),
                "closure replacement should report the invalid call: {error:?}"
            );
        }
    }
}

#[test]
fn mutable_ref_parameter_runtime_witness_updates_the_caller() {
    let source = r#"
type Person = { name: Int }
func rename[people: group Person](person: ref[people] Person, name: Int) -> () [mut people.name] {
    person.name = name
    ()
}
func main() -> Int {
    let person = Person { name: 0 }
    rename(ref person, 42)
    person.name
}
"#;
    let compilation = foster::compile(source).unwrap();
    for optimize in [false, true] {
        assert_eq!(
            foster::vm::run_with_options(&compilation, foster::vm::CompileOptions { optimize })
                .unwrap(),
            foster::vm::Value::Integer(42)
        );
    }
}

#[test]
fn whole_place_mutable_ref_parameter_survives_optimization() {
    let source = r#"
type Vals = { value: Int }
func set[g: group Vals](box: ref[g] Vals) -> Int [mut g] {
    box = Vals { value: 7 }
    box.value
}
func main() -> Int {
    let box = Vals { value: 1 }
    set(ref box)
    box.value
}
"#;
    let compilation = foster::compile(source).unwrap();
    let unoptimized = foster::vm::compile_with_options(
        &compilation,
        foster::vm::CompileOptions { optimize: false },
    )
    .unwrap();
    let set = unoptimized
        .functions
        .values()
        .find(|function| function.name == "set")
        .unwrap();
    assert_eq!(set.mutable_parameters, [true]);
    for optimize in [false, true] {
        assert_eq!(
            foster::vm::run_with_options(&compilation, foster::vm::CompileOptions { optimize })
                .unwrap(),
            foster::vm::Value::Integer(7),
            "mutable borrow lost its caller-backed place with optimize={optimize}"
        );
    }
}

#[test]
fn equivalent_cfg_rewrites_keep_the_same_ownership_decision() {
    let linear = r#"
func main() -> Int {
    let values = [10]
    let selected = ref values[0]
    values.push(20)
    selected
}
"#;
    let split = r#"
func main() -> Int {
    let values = [10]
    let selected = ref values[0]
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
    assert!(first_dump.contains("foster-language=7 ownership-model=3"));
    assert!(first_dump.contains("loan L"));
    assert!(first_dump.contains("region L"));

    let invalid = r#"
func main() -> Int {
    let values = [10]
    let selected = ref values[0]
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
    assert_eq!(foster::ownership::LANGUAGE_VERSION, 7);
    assert_eq!(foster::ownership::MODEL_VERSION, 3);
    assert_eq!(
        foster::ownership::diagnostics::CATALOG
            .iter()
            .map(|(code, _)| *code)
            .collect::<Vec<_>>(),
        vec!["E0382", "E0401", "E0402", "E0403", "E0507", "E0728"]
    );
}

#[test]
fn ownership_model_two_defines_expression_temporary_lifetimes() {
    let scoped = r#"
func observe[value: group Int](item: ref[value] Int) -> Int { item }
func make() -> Int { 42 }
func main() -> Int { observe(ref (make())) }
"#;
    assert_eq!(foster::run(scoped).unwrap(), foster::vm::Value::Integer(42));

    let escaped = r#"
func preserve[value: group Int](item: ref[value] Int) -> ref[value] Int { ref item }
func make() -> Int { 42 }
func main() -> Int {
    let item = preserve(ref (make()))
    println(item)
    0
}
"#;
    let error = foster::compile(escaped).unwrap_err();
    assert_eq!(
        error.code.as_deref(),
        Some(foster::ownership::diagnostics::INVALIDATED_LOAN)
    );
    assert!(error.message.contains("temporary"), "{error:?}");
}

#[test]
fn language_version_two_reserves_assert_for_immediate_failures() {
    assert!(foster::compile("func assert(value: Bool) -> Bool { value }").is_err());
    assert!(foster::compile("func main() -> () { assert(true) }").is_ok());
}

#[test]
fn language_version_three_reserves_loop_control_transfers() {
    for keyword in ["loop", "break", "continue"] {
        let source = format!("func {keyword}() -> () {{ () }}");
        assert!(foster::compile(&source).is_err());
    }
    assert!(foster::compile("func main() -> () { loop { break } }").is_ok());
}

#[test]
fn language_version_four_reserves_continue_for_loops() {
    let branch_continue = r#"
func main() -> Int {
    branch {
        _ -> { continue }
    }
}
"#;
    assert!(foster::compile(branch_continue).is_err());
    assert!(
        foster::compile("func main() -> () { loop { branch { true -> { continue } _ -> () } } }")
            .is_ok()
    );
}

#[test]
fn language_version_five_reserves_try_for_result_propagation() {
    assert!(foster::compile("func try() -> Int { 1 }").is_err());
    let propagation = r#"
import core.result

func operation() -> Result<Int, String> { Result.Ok(1) }

func main() -> Result<Int, String> {
    let value = try operation()
    Result.Ok(value)
}
"#;
    assert!(foster::compile(propagation).is_ok());
}

#[test]
fn language_version_six_separates_qualification_from_member_access() {
    let old_case =
        foster::compile("enum Choice = Value(Int)\nfunc main() -> Choice { Choice::Value(1) }")
            .unwrap_err();
    assert!(
        old_case.message.contains("type access uses `.`")
            && old_case.message.contains("Choice.Value"),
        "{old_case:?}"
    );

    let new_case = r#"
enum Choice = Value(Int)
type Box = { value: Choice }
func Box.make(value: Choice) -> Box { Box { value } }
func main() -> Choice { Box.make(Choice.Value(1)).value }
"#;
    assert!(foster::compile(new_case).is_ok());
}

#[test]
fn language_version_seven_reserves_not_as_logical_negation() {
    assert!(foster::compile("func not(value: Bool) -> Bool { value }").is_err());
    assert!(foster::compile("func main() -> Bool { not false == !false }").is_ok());
}

#[test]
fn loop_edges_preserve_move_state() {
    let repeated_move = r#"
func take(values: List<Int>) -> () [consume values] { () }
func main() -> () {
    let values = [1]
    loop {
        take(move values)
        continue
    }
}
"#;
    let error = foster::compile(repeated_move).unwrap_err();
    assert!(
        error.message.contains("used after it was moved"),
        "{error:?}"
    );

    let moved_exit = r#"
func take(values: List<Int>) -> () [consume values] { () }
func main() -> List<Int> {
    let values = [1]
    loop {
        take(move values)
        break
    }
    values
}
"#;
    let error = foster::compile(moved_exit).unwrap_err();
    assert!(
        error.message.contains("used after it was moved"),
        "{error:?}"
    );
}

#[test]
fn loop_continue_inside_a_branch_preserves_move_state() {
    let source = r#"
func take(values: List<Int>) -> () [consume values] { () }
func main() -> () {
    let values = [1]
    loop {
        branch {
            true -> {
                take(move values)
                continue
            }
            _ -> ()
        }
    }
}
"#;
    let error = foster::compile(source).unwrap_err();
    assert!(
        error.message.contains("used after it was moved"),
        "{error:?}"
    );
}
