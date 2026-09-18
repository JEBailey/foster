use foster::compile;

#[test]
fn reference_payload_assignment_updates_the_original_scalar() {
    let source = r#"
import core.option
type Box = { pub value: Int }
impl Box {
    func borrow(self: Box) -> Option<ref[self] Int> { Option.Some(ref self.value) }
}
func main() -> Int {
    let owner = Box { value: 1 }
    branch owner.borrow() {
        Option.Some(value) -> { value = 42 }
        Option.None -> ()
    }
    owner.value
}
"#;
    let compilation = compile(source).unwrap();
    for optimize in [false, true] {
        let value =
            foster::vm::run_with_options(&compilation, foster::vm::CompileOptions { optimize })
                .unwrap();
        assert_eq!(value.to_string(), "42", "optimize={optimize}");
    }
}

#[test]
fn mutation_through_a_collection_borrow_requires_mutation_effects() {
    for subject in ["values.borrow(0)", "borrowed"] {
        let source = format!(
            r#"
import core.option
import core.list
type Item = {{ pub value: Int }}
func inspect(values: List<Item>) -> () [read values] {{
    let borrowed = values.borrow(0)
    branch {subject} {{
        Option.Some(item) -> {{ item.value = 42 }}
        Option.None -> ()
    }}
    ()
}}
func main() {{ inspect([Item {{ value: 1 }}]) }}
"#
        );
        let error = compile(&source).expect_err("read effect cannot mutate borrowed storage");
        assert!(error.message.contains("effect"), "{error:?}");
    }
}

#[test]
fn collection_borrows_cannot_survive_removal_or_escape_the_owner() {
    for constructor in [
        "Map.empty().put(0, Item { value: 42 })",
        "HashMap.empty((key: Int) -> 0).put(0, Item { value: 42 })",
        "[Item { value: 42 }]",
    ] {
        for body in [
            "let borrowed = values.borrow(0)\nvalues.remove(0)\nbranch borrowed { Option.Some(item) -> item.value\nOption.None -> 0 }",
            "values.borrow(0)",
        ] {
            let source = format!(
                r#"
import core.option
import core.list
import std.collections.map
import std.collections.hash_map
type Item = {{ pub value: Int }}
func main() {{
    let values = {constructor}
    {body}
}}
"#
            );
            let error = compile(&source)
                .err()
                .unwrap_or_else(|| panic!("accepted invalid borrow: {source}"));
            assert!(
                matches!(error.code.as_deref(), Some("E0401" | "E0402")),
                "{error:?}"
            );
        }
    }
}

#[test]
fn collection_get_is_not_available() {
    let error = compile("func main() { [1].get(0) }").unwrap_err();
    assert!(error.message.contains("get"), "{error:?}");
}

#[test]
fn collection_borrow_can_forward_an_exposed_input_group() {
    let source = r#"
import core.option
import core.list
func selected[g: group List<Int>](values: ref[g] List<Int>) -> Option<ref[g] Int> {
    values.borrow(0)
}

func main() -> Int {
    let values = [42]
    branch selected(ref values) {
        Option.Some(value) -> value
        Option.None -> 0
    }
}
"#;
    assert_eq!(foster::run(source).unwrap().to_string(), "42");
}

#[test]
fn consuming_receiver_cannot_return_a_reference_into_itself() {
    let source = r#"
type Box = { value: Int }
impl Box {
    func invalid(self: Box) -> ref[self] Int [consume self] { ref self.value }
}
func main() { Box { value: 42 }.invalid() }
"#;
    assert!(compile(source).is_err());
}
