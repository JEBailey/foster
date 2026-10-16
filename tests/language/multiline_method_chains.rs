#[test]
fn multiline_method_chains_preserve_declarations_and_report_unknown_members() {
    let source = "import core.list.*\nfunc apply() {\n    [1]\n        .missing_method()\n}\nfunc main() { apply() }\n";
    let parsed = foster::parse_recovering(source).unwrap();
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    assert_eq!(parsed.program.functions.len(), 2);
    let error = foster::compile(source).unwrap_err();
    assert!(error.message.contains("missing_method"), "{error}");
    assert!(!error.message.contains("expected expression"), "{error}");
}

#[test]
fn multiline_iterator_pipeline_runs_with_imports() {
    let source = "import std.iter\nimport std.iter.*\nimport static std.iter.*\nimport std.iter.map\nimport std.iter.map.*\nimport static std.iter.map.*\nfunc main() -> Int {\n    let values = [20, 21]\n        // Continue the same expression across a comment and a blank line.\n\n        .iterator()\n        .map((value: Int) -> { value + 1 })\n        .collect()\n    values[1]\n}\n";
    assert_eq!(foster::run(source).unwrap(), foster::vm::Value::Integer(22));
}

#[test]
fn newline_before_parentheses_does_not_continue_a_call() {
    let source = "func main() -> Int {\n    let value = 20\n    (value + 22)\n}\n";
    assert_eq!(foster::run(source).unwrap(), foster::vm::Value::Integer(42));
}
