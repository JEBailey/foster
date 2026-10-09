use foster::vm::{CompileOptions, Value};

#[test]
fn borrowed_slices_iterate_map_stored_noncopyable_elements() {
    let compilation = foster::compile(include_str!("fixtures/programs/list_slices.fos")).unwrap();
    for optimize in [false, true] {
        assert_eq!(
            foster::vm::run_with_options(&compilation, CompileOptions { optimize }).unwrap(),
            Value::Integer(42)
        );
    }
    foster::native::prepare(&compilation).unwrap();
}

#[test]
fn copy_slice_requires_copy_even_for_empty_ranges() {
    let error = foster::compile(
        "import core.list.List\ntype Item = { value: Int }\nfunc main() -> Int {\nlet values = [Item { value: 42 }]\nvalues.copy_slice(0, 0).length\n}",
    )
    .unwrap_err();
    assert!(error.message.contains("constraint"), "{error}");
    assert!(error.message.contains("Copy"), "{error}");
}

#[test]
fn slices_and_their_cursors_cannot_escape_or_survive_source_reshaping() {
    for body in [
        "let values = [Item { value: 42 }]\nlet view = values.slice(0, 1)\nvalues.push(Item { value: 1 })\nview.length",
        "let view = :local { let values = [Item { value: 42 }]\nvalues.slice(0, 1) }\nview.length",
        "let values = [Item { value: 42 }]\nlet view = values.slice(0, 1)\nlet cursor = view.iterator()\nvalues.push(Item { value: 1 })\nbranch cursor.next() { Option.Some(item) -> item.value\nOption.None -> 0 }",
    ] {
        let source = format!(
            "import core.list.List\nimport core.option.Option\ntype Item = {{ value: Int }}\nfunc main() -> Int {{\n{body}\n}}"
        );
        let error = foster::compile(&source).unwrap_err();
        assert!(
            error.message.contains("borrow") || error.message.contains("reference"),
            "{error}"
        );
    }
}

#[test]
fn slice_bounds_are_checked_before_borrowing() {
    for (method, expected) in [
        ("slice", "List.slice requires 0 <= start <= end <= length"),
        (
            "copy_slice",
            "List.copy_slice requires 0 <= start <= end <= length",
        ),
    ] {
        for (start, end) in [(-1, 1), (1, 0), (0, 2)] {
            let source = format!(
                "import core.list.List\nfunc main() -> Int {{ let values = [42]\nvalues.{method}({start}, {end}).length }}"
            );
            let error = foster::run(&source).unwrap_err();
            assert!(error.message.contains(expected), "{error}");
        }
    }
}
