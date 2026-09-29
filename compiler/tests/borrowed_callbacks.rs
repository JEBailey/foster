#[test]
fn borrowed_map_compiles() {
    foster_compiler::compile(include_str!(
        "../../tests/fixtures/programs/borrowed_map_callback.fos"
    ))
    .unwrap();
}

#[test]
fn callbacks_stored_in_captured_containers_keep_their_effects() {
    let error = foster_compiler::compile(
        r#"
import core.list
import std.iter.map
func invalid(value: ref[value] Int) -> List<Int> [read value] {
    let actions = []
    actions.push([ref value] () -> { value = value + 1
        value })
    [1].iterator().map([ref actions] (item: Int) -> actions[0]() + item).collect()
}
"#,
    )
    .err()
    .expect("container hid callback mutation");
    assert!(error.message.contains("effect"), "{error:?}");
}

#[test]
fn capture_capability_does_not_grant_argument_mutation() {
    let error = foster_compiler::compile(
        r#"
import core.list
func apply(action: func(List<Int>) -> Int [reshape captures], values: List<Int>) -> Int {
    action(values)
}
func mutate(values: List<Int>) -> Int { values.push(2)
    0 }
func main() -> Int { apply(mutate, [1]) }
"#,
    )
    .err()
    .expect("capture capability granted argument mutation");
    assert!(error.message.contains("callable contract"), "{error:?}");
}

#[test]
fn borrowed_map_rejects_invalidated_capture_before_iteration() {
    let error = foster_compiler::compile(
        r#"
import core.list
import std.iter.map
func main() -> Int {
    let values = [1]
    let selected = ref values[0]
    let pending = [1].iterator().map([ref selected] (value: Int) -> selected + value)
    values.push(2)
    pending.collect()[0]
}
"#,
    )
    .err()
    .expect("iterator used an invalidated capture");
    assert!(
        error.message.contains("invalid") || error.message.contains("borrow"),
        "{error:?}"
    );
}

#[test]
fn borrowed_map_string_results_run() {
    let compiled = foster_compiler::compile(include_str!(
        "../../tests/fixtures/programs/borrowed_map_callback_strings.fos"
    ))
    .unwrap();
    let program = foster_compiler::vm::compile(&compiled).unwrap();
    assert_eq!(
        foster_vm::run(&program.into_verified().unwrap()).unwrap(),
        foster_vm::Value::Integer(42)
    );
}

#[test]
fn immediate_capture_callback_obeys_caller_permissions() {
    let error = foster_compiler::compile(
        r#"
func apply(action: func() -> Int [mut captures]) -> Int { action() }
func invalid(value: ref[value] Int) -> Int [read value] {
    apply([ref value] () -> { value = value + 1
        value })
}
"#,
    )
    .err()
    .expect("immediate callback hid mutation");
    assert!(error.message.contains("effect"), "{error:?}");
}

#[test]
fn immediate_capture_callback_invalidates_borrowed_elements() {
    let error = foster_compiler::compile(
        r#"
import core.list
func apply(action: func() -> Int [reshape captures]) -> Int { action() }
func main() -> Int {
    let values = [1]
    let element = ref values[0]
    apply([ref values] () -> { values.push(2)
        0 })
    element
}
"#,
    )
    .err()
    .expect("temporary callback hid reshaping");
    assert!(
        error.message.contains("invalid") || error.message.contains("borrow"),
        "{error:?}"
    );
}

#[test]
fn borrowed_callbacks_preserve_effects_through_aliases_and_branches() {
    for selected in [
        "pending",
        "branch flag { true -> move pending _ -> move other }",
    ] {
        let source = format!(
            r#"
import core.list
import std.iter.map
func invalid(visited: List<Int>, flag: Bool) -> List<Int> [read visited, read flag] {{
    let alias = ref visited
    let pending = [1].iterator().map([ref alias] (value: Int) -> {{
        alias.push(value)
        value
    }})
    let other = [2].iterator()
    let chosen = {selected}
    chosen.collect()
}}
"#
        );
        let error = foster_compiler::compile(&source)
            .err()
            .expect("alias hid callback mutation");
        assert!(error.message.contains("effect"), "{error:?}");
    }
}

#[test]
fn borrowed_map_construction_is_lazy() {
    foster_compiler::compile(
        r#"
import core.list
import std.iter.map
func build(visited: List<Int>) -> () [read visited] {
    let pending = [1].iterator().map([ref visited] (value: Int) -> {
        visited.push(value)
        value
    })
    ()
}
"#,
    )
    .unwrap();
}

#[test]
fn borrowed_map_keeps_named_iterator_captures_live() {
    foster_compiler::compile(
        r#"
import core.list
import std.iter.map
func main() -> Int {
    let visited = []
    let pending = [20, 22].iterator().map([ref visited] (value: Int) -> {
        visited.push(value)
        value
    })
    assert(visited.empty?)
    let values = pending.collect()
    assert(visited.length == 2)
    values[0] + values[1]
}
"#,
    )
    .unwrap();
}

#[test]
fn pure_callbacks_still_reject_mutation() {
    let error = foster_compiler::compile(
        r#"
func apply(action: func() -> Int) -> Int { action() }
func main() -> Int {
    let count = 0
    apply([ref count] () -> { count = count + 1
        count })
}
"#,
    )
    .err()
    .expect("mutation erased into pure callback");
    assert!(error.message.contains("callable contract"), "{error:?}");
}

#[test]
fn borrowed_map_returned_by_helper_preserves_effects() {
    let error = foster_compiler::compile(
        r#"
import core.list
import std.iter.map
func make(visited: List<Int>) {
    [1].iterator().map([ref visited] (value: Int) -> {
        visited.push(value)
        value
    })
}
func invalid(visited: List<Int>) -> List<Int> [read visited] {
    make(visited).collect()
}
"#,
    )
    .err()
    .expect("returned iterator hid mutation");
    assert!(error.message.contains("effect"), "{error:?}");
}
#[test]
fn borrowed_map_cannot_escape_its_origin() {
    let error = foster_compiler::compile(
        r#"
import core.list
import std.iter.map
func invalid() {
    let visited = []
    [1].iterator().map([ref visited] (value: Int) -> {
        visited.push(value)
        value
    })
}
"#,
    )
    .err()
    .expect("invalid callback accepted");
    assert!(error.message.contains("borrow"), "{error:?}");
}

#[test]
fn borrowed_map_cannot_hide_mutation_from_read_contract() {
    let error = foster_compiler::compile(
        r#"
import core.list
import std.iter.map
func invalid(visited: List<Int>) -> List<Int> [read visited] {
    [1].iterator().map([ref visited] (value: Int) -> {
        visited.push(value)
        value
    }).collect()
}
"#,
    )
    .err()
    .expect("invalid callback accepted");
    assert!(error.message.contains("effect"), "{error:?}");
}

#[test]
fn borrowed_map_invalidates_element_borrows_when_collected() {
    let error = foster_compiler::compile(
        r#"
import core.list
import std.iter.map
func main() -> Int {
    let visited = [0]
    let selected = ref visited[0]
    let values = [1].iterator().map([ref visited] (value: Int) -> {
        visited.push(value)
        value
    }).collect()
    selected
}
"#,
    )
    .err()
    .expect("invalid callback accepted");
    assert!(
        error.message.contains("invalid") || error.message.contains("borrow"),
        "{error:?}"
    );
}
