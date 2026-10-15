#[test]
fn clearing_invalidates_live_slices_and_element_references() {
    for body in [
        "let values = [42]\nlet view = values.slice(0, 1)\nvalues.clear()\nview.length",
        "let values = [42]\nlet element = ref values[0]\nvalues.clear()\nelement + 0",
        "let values = [42]\nlet view = values.slice(0, 1)\nlet cursor = view.iterator()\nvalues.clear()\ncursor.next()\n0",
    ] {
        let source = format!("import core.list.List\nfunc main() -> Int {{\n{body}\n}}");
        let error = foster::compile(&source).unwrap_err();
        assert!(
            error.message.contains("borrow") || error.message.contains("reference"),
            "{error}"
        );
    }
}
