use foster::vm::Value;

const PRELUDE: &str = r#"
func describe(value: Int) -> String { "number" }
func other(value: Int) -> String { "second" }
type Holder = { callback: func(Int) -> String }
func invoke(callback: func(Int) -> String, value: Int) -> String { callback(value) }
func make() -> func(Int) -> String { describe }
"#;

fn dynamic_source(setup: &str, call: &str, changed: &str) -> String {
    format!(
        r#"
type Holder<T> = {{ value: T }}
func first[g: group Int](left: ref[g] Int, right: ref[g] Int) -> ref[g] Int {{ ref left }}
func also_first[g: group Int](left: ref[g] Int, right: ref[g] Int) -> ref[g] Int {{ ref left }}
func second[g: group Int](left: ref[g] Int, right: ref[g] Int) -> ref[g] Int {{ ref right }}
func choose(index: Int, flag: Bool) -> Int {{
    let left = [10]
    let right = [32]
    {setup}
    let selected = {call}(ref left[0], ref right[0])
    {changed}
    println(selected)
    0
}}
func main() -> Int {{ choose(0, true) + choose(1, false) }}
"#
    )
}

#[test]
fn bounded_dynamic_targets_preserve_shared_result_dependencies() {
    for (setup, call) in [
        ("let callbacks = [first, also_first]", "callbacks[index]"),
        (
            "let callbacks = [first, also_first]\nlet callback = callbacks[index]",
            "callback",
        ),
        (
            "let callbacks = [first, also_first]\nlet moved = move callbacks",
            "moved[index]",
        ),
        (
            "let held = Holder { value: [first, also_first] }",
            "held.value[index]",
        ),
        (
            "let callbacks = branch flag { true -> [first, also_first]\n_ -> [also_first, first] }",
            "callbacks[index]",
        ),
        (
            "let callback = branch flag { true -> first\n_ -> also_first }",
            "callback",
        ),
    ] {
        let source = dynamic_source(setup, call, "right.push(99)");
        assert_eq!(
            foster::run(&source).unwrap_or_else(|error| panic!("{setup}: {error:?}")),
            Value::Integer(0)
        );
        assert_eq!(
            foster::compile(&source.replace("right.push(99)", "left.push(99)"))
                .unwrap_err()
                .code
                .as_deref(),
            Some("E0401")
        );
    }
}

#[test]
fn dynamic_target_unions_keep_all_origins_and_forget_mutated_lists() {
    for setup in [
        "let callbacks = [first, second]",
        "let callbacks = [first, also_first]\ncallbacks[index] = second",
        "let callbacks = [first, also_first]\ncallbacks[0] = second",
        "let callbacks = [first, also_first]\ncallbacks.push(second)",
        "let callbacks = [first, also_first]\nlet alias = ref callbacks[0]\nalias = second",
        "let callbacks = [first, also_first]\nlet change = [ref callbacks] () -> { callbacks[0] = second\n() }\nchange()",
        "let callbacks = branch flag { true -> [first, also_first]\n_ -> [first, second] }",
    ] {
        let source = dynamic_source(setup, "callbacks[index]", "right.push(99)");
        foster::compile(&source.replace("right.push(99)", "()"))
            .unwrap_or_else(|error| panic!("{setup}: {error:?}"));
        assert_eq!(
            foster::compile(&source).unwrap_err().code.as_deref(),
            Some("E0401"),
            "{setup}"
        );
    }
}

#[test]
fn dynamic_target_limits_and_unknown_entries_use_the_full_contract() {
    for count in [8, 9] {
        let functions = (0..count).map(|index| format!(
            "func target{index}[g: group Int](left: ref[g] Int, right: ref[g] Int) -> ref[g] Int {{ ref left }}\n"
        )).collect::<String>();
        let names = (0..count)
            .map(|index| format!("target{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let source = format!(
            "{functions}{}",
            dynamic_source(
                &format!("let callbacks = [{names}]"),
                "callbacks[index]",
                "right.push(99)"
            )
        );
        let checked = foster::compile(&source);
        if count == 8 {
            checked.unwrap();
        } else {
            assert_eq!(checked.unwrap_err().code.as_deref(), Some("E0401"));
        }
    }
    let source = dynamic_source("let callbacks = [unknown, first]", "callbacks[index]", "right.push(99)")
        .replace("func choose(index: Int, flag: Bool)", "func choose[g: group Int](unknown: func(ref[g] Int, ref[g] Int) -> ref[g] Int, index: Int, flag: Bool)")
        .replace("choose(0, true) + choose(1, false)", "choose(second, 0, true)");
    foster::compile(&source.replace("right.push(99)", "()")).unwrap();
    assert_eq!(
        foster::compile(&source).unwrap_err().code.as_deref(),
        Some("E0401")
    );
}

#[test]
fn dynamic_targets_retain_captured_loans() {
    let source = r#"
func choose(index: Int) -> Int {
    let values = [10]
    let selected = ref values[0]
    let callbacks = [[ref selected] () -> selected, [ref selected] () -> selected]
    let callback = callbacks[index]
    values.push(20)
    callback()
}
func main() -> Int { choose(1) }
"#;
    foster::compile(&source.replace("values.push(20)", "()")).unwrap();
    assert_eq!(
        foster::compile(source).unwrap_err().code.as_deref(),
        Some("E0401")
    );
}

#[test]
fn independent_results_survive_callable_storage_adaptation_and_capture() {
    for (setup, call) in [
        ("let callback = describe", "callback(selected)"),
        (
            "let callback = describe\nlet moved = move callback",
            "moved(selected)",
        ),
        (
            "let callback = branch flag { true -> describe\n_ -> other }",
            "callback(selected)",
        ),
        (
            "let held = Holder { callback: describe }",
            "held.callback(selected)",
        ),
        (
            "let callbacks = [describe, other]",
            "callbacks[0](selected)",
        ),
        ("let callback = make()", "callback(selected)"),
        ("let callback = describe", "invoke(callback, selected)"),
        (
            "let callback = [ref selected] (value: Int) -> { selected\n\"number\" }",
            "callback(0)",
        ),
        (
            "let callback = describe\nlet adapter = [move callback] (value: Int) -> callback(value)",
            "adapter(selected)",
        ),
    ] {
        let source = format!(
            "{PRELUDE}\nfunc check(flag: Bool) -> Int {{ let values = [10]\nlet selected = ref values[0]\n{setup}\nlet text = {call}\nvalues.push(20)\ntext.length }}\nfunc main() -> Int {{ check(true) }}"
        );
        assert_eq!(
            foster::run(&source).unwrap_or_else(|error| panic!("{setup}: {error:?}")),
            Value::Integer(6)
        );
    }
}

#[test]
fn known_callable_results_keep_only_the_used_reference_parameter() {
    for setup in [
        "let callback = first",
        "let held = Holder { value: first }\nlet callback = move held.value",
        "let original = first\nlet callback = move original",
        "let callback = branch flag { true -> first\n_ -> first }",
        "let callbacks = [first, second]\nlet callback = callbacks[0]",
    ] {
        let source = format!(
            r#"
type Holder<T> = {{ value: T }}
func first[g: group Int](left: ref[g] Int, right: ref[g] Int) -> ref[g] Int {{ ref left }}
func second[g: group Int](left: ref[g] Int, right: ref[g] Int) -> ref[g] Int {{ ref right }}
func check(flag: Bool) -> Int {{
    let left = [10]
    let right = [20]
    {setup}
    let selected = callback(ref left[0], ref right[0])
    right.push(30)
    println(selected)
    0
}}
func main() -> Int {{ check(true) }}
"#
        );
        foster::compile(&source).unwrap_or_else(|error| panic!("{setup}: {error:?}"));
        let invalid = source.replace("right.push(30)", "left.push(30)");
        assert_eq!(
            foster::compile(&invalid).unwrap_err().code.as_deref(),
            Some("E0401")
        );
    }
}

#[test]
fn constant_index_selection_preserves_each_slots_reference_summary() {
    for (index, changed, valid) in [
        (0, "right", true),
        (1, "left", true),
        (0, "left", false),
        (1, "right", false),
    ] {
        let source = format!(
            r#"
func first[g: group Int](left: ref[g] Int, right: ref[g] Int) -> ref[g] Int {{ ref left }}
func second[g: group Int](left: ref[g] Int, right: ref[g] Int) -> ref[g] Int {{ ref right }}
func main() -> Int {{
    let left = [10]
    let right = [32]
    let callbacks = [first, second]
    let selected = callbacks[{index}](ref left[0], ref right[0])
    {changed}.push(99)
    println(selected)
    0
}}
"#
        );
        if valid {
            assert_eq!(foster::run(&source).unwrap(), Value::Integer(0));
        } else {
            assert_eq!(
                foster::compile(&source).unwrap_err().code.as_deref(),
                Some("E0401")
            );
        }
    }
}

#[test]
fn constant_index_selection_does_not_reuse_replaced_or_dynamic_targets() {
    for (setup, index) in [("callbacks[0] = second", "0"), ("", "index")] {
        let source = format!(
            r#"
func first[g: group Int](left: ref[g] Int, right: ref[g] Int) -> ref[g] Int {{ ref left }}
func second[g: group Int](left: ref[g] Int, right: ref[g] Int) -> ref[g] Int {{ ref right }}
func choose(index: Int) -> Int {{
    let left = [10]
    let right = [32]
    let callbacks = [first, second]
    {setup}
    let selected = callbacks[{index}](ref left[0], ref right[0])
    right.push(99)
    println(selected)
    0
}}
func main() -> Int {{ choose(1) }}
"#
        );
        assert_eq!(
            foster::compile(&source).unwrap_err().code.as_deref(),
            Some("E0401"),
            "{setup}/{index}"
        );
    }
}

#[test]
fn unknown_and_mixed_targets_keep_possible_reference_origins() {
    let source = r#"
func first[g: group Int](left: ref[g] Int, right: ref[g] Int) -> ref[g] Int { ref left }
func second[g: group Int](left: ref[g] Int, right: ref[g] Int) -> ref[g] Int { ref right }
func check(flag: Bool) -> Int {
    let left = [10]
    let right = [20]
    let callback = branch flag { true -> first
        _ -> second }
    let selected = callback(ref left[0], ref right[0])
    right.push(30)
    println(selected)
    0
}
func main() -> Int { check(false) }
"#;
    assert_eq!(
        foster::compile(source).unwrap_err().code.as_deref(),
        Some("E0401")
    );
}

#[test]
fn returned_closures_keep_captured_parameter_origins() {
    let source = r#"
func make[g: group Int](selected: ref[g] Int) -> func() -> Int [read g] {
    [ref selected] () -> selected
}
func main() -> Int {
    let values = [10]
    let factory = make
    let reader = factory(ref values[0])
    values.push(20)
    reader()
}
"#;
    foster::compile(&source.replace("values.push(20)", "()")).unwrap();
    assert_eq!(
        foster::compile(source).unwrap_err().code.as_deref(),
        Some("E0401")
    );
}

#[test]
fn unknown_callable_parameters_use_reference_group_contracts() {
    for (mutation, valid) in [("right.push(30)", true), ("left.push(30)", false)] {
        let source = format!(
            r#"
func first[a: group Int, b: group Int](left: ref[a] Int, right: ref[b] Int) -> ref[a] Int {{ ref left }}
func invoke[a: group Int, b: group Int](callback: func(ref[a] Int, ref[b] Int) -> ref[a] Int, left: ref[a] Int, right: ref[b] Int) -> ref[a] Int {{
    callback(ref left, ref right)
}}
func main() -> Int {{
    let left = [10]
    let right = [20]
    let selected = invoke(first, ref left[0], ref right[0])
    {mutation}
    println(selected)
    0
}}
"#
        );
        let result = foster::compile(&source);
        assert_eq!(result.is_ok(), valid, "{result:?}");
    }
}

#[test]
fn callable_target_replacement_and_unknown_joins_do_not_keep_stale_summaries() {
    for setup in [
        "let callback = first\ncallback = second",
        "let callback = first\nlet replace = [ref callback] () -> { callback = second\n() }\nreplace()",
        "let callback = first\nloop { callback = second\nbreak }",
        "let callback = first\nlet alias = ref callback\nalias = second",
        "let callback = branch flag { true -> unknown\n_ -> first }",
    ] {
        let source = format!(
            r#"
type Holder<T> = {{ value: T }}
func first[g: group Int](left: ref[g] Int, right: ref[g] Int) -> ref[g] Int {{ ref left }}
func second[g: group Int](left: ref[g] Int, right: ref[g] Int) -> ref[g] Int {{ ref right }}
func check[g: group Int](unknown: func(ref[g] Int, ref[g] Int) -> ref[g] Int, flag: Bool) -> Int {{
    let left = [10]
    let right = [20]
    {setup}
    let selected = callback(ref left[0], ref right[0])
    right.push(30)
    println(selected)
    0
}}
func main() -> Int {{ check(second, false) }}
"#
        );
        foster::compile(&source.replace("right.push(30)", "()"))
            .unwrap_or_else(|error| panic!("{setup}: {error:?}"));
        assert_eq!(
            foster::compile(&source)
                .err()
                .unwrap_or_else(|| panic!("accepted invalid target: {setup}"))
                .code
                .as_deref(),
            Some("E0401"),
            "{setup}"
        );
    }
}

#[test]
fn generic_result_fields_are_checked_recursively_for_borrowers() {
    let source = r#"
type Box<T> = { value: T }
func wrap(value: Int) -> Box<String> { Box { value: "number" } }
func main() -> Int {
    let values = [10]
    let selected = ref values[0]
    let callback = wrap
    let held = callback(selected)
    values.push(20)
    held.value.length
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(6));
    let borrowed = r#"
type Box<T> = { value: T }
func make[g: group Int](selected: ref[g] Int) -> Box<func() -> Int [read g]> {
    Box { value: [ref selected] () -> selected }
}
func main() -> Int {
    let values = [10]
    let factory = make
    let held = factory(ref values[0])
    values.push(20)
    held.value()
}
"#;
    foster::compile(&borrowed.replace("values.push(20)", "()")).unwrap();
    assert_eq!(
        foster::compile(borrowed).unwrap_err().code.as_deref(),
        Some("E0401")
    );
}

#[test]
fn recursive_target_summaries_preserve_reborrow_ancestry() {
    let source = r#"
func follow[g: group Int](value: ref[g] Int, count: Int) -> func() -> Int [read g] {
    branch {
        count == 0 -> () -> 1
        _ -> follow(ref value, count - 1)
    }
}
func main() -> Int {
    let values = [10]
    let callback = follow
    let result = callback(ref values[0], 2)
    values.push(20)
    result()
}
"#;
    foster::compile(source).unwrap();
    assert_eq!(
        foster::compile(&source.replace("() -> 1", "[ref value] () -> value"))
            .unwrap_err()
            .code
            .as_deref(),
        Some("E0401")
    );
}

#[test]
fn callable_adaptation_cannot_omit_a_result_group() {
    let source = r#"
func wrong[a: group Int, b: group Int](left: ref[a] Int, right: ref[b] Int) -> ref[b] Int { ref right }
func invoke[a: group Int, b: group Int](callback: func(ref[a] Int, ref[b] Int) -> ref[a] Int, left: ref[a] Int, right: ref[b] Int) -> ref[a] Int {
    callback(ref left, ref right)
}
func main() -> Int {
    let left = [10]
    let right = [20]
    let selected = invoke(wrong, ref left[0], ref right[0])
    println(selected)
    0
}
"#;
    assert!(foster::compile(source).is_err());
}
