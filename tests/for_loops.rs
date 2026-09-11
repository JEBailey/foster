use foster::vm::Value;

#[test]
fn for_headers_allow_records_in_delimited_expressions() {
    let source = "type Item = { value: Int }
        func values(item: Item) -> List<Int> { [item.value] }
        func main() -> Int {
            let total = 0
            for item in [Item { value: 10 }] { total = total + item.value }
            for item in values(Item { value: 12 }) { total = total + item }
            for item in [(Item { value: 20 }).value] { total = total + item }
            total
        }";
    let compilation = foster::compile(source).unwrap();
    for optimize in [false, true] {
        assert_eq!(
            foster::vm::run_with_options(&compilation, foster::vm::CompileOptions { optimize })
                .unwrap(),
            Value::Integer(42)
        );
    }
    assert!(
        foster::parse_recovering(source)
            .unwrap()
            .diagnostics
            .is_empty()
    );
}

#[test]
fn for_matches_an_explicit_iterator_and_option_loop() {
    let body =
        "visits = visits + 1\ncontinue if item == 2\nbreak if item == 4\ntotal = total + item\n()";
    for iteration in [
        format!("for item in values(ref calls) {{ {body} }}"),
        format!(
            "let cursor = values(ref calls).iterator()
            loop {{
                branch cursor.next() {{
                    Option.None -> {{ break }}
                    Option.Some(item) -> {{ {body} }}
                }}
            }}"
        ),
    ] {
        let source = format!(
            "import core.option
            type Calls = {{ count: Int }}
            func values[g: group Calls](calls: ref[g] Calls) -> List<Int> {{
                calls.count = calls.count + 1
                [1, 2, 3, 4, 5]
            }}
            func main() -> Int {{
                let calls = Calls {{ count: 0 }}
                let visits = 0
                let total = 0
                {iteration}
                assert(calls.count == 1)
                assert(visits == 4)
                assert(total == 4)
                42
            }}"
        );
        let compilation = foster::compile(&source).unwrap();
        for optimize in [false, true] {
            assert_eq!(
                foster::vm::run_with_options(&compilation, foster::vm::CompileOptions { optimize })
                    .unwrap(),
                Value::Integer(42)
            );
        }
    }
}

#[test]
fn for_loops_run_with_both_optimization_modes() {
    let compilation = foster::compile(include_str!("fixtures/programs/for_loops.fos")).unwrap();
    for optimize in [false, true] {
        assert_eq!(
            foster::vm::run_with_options(&compilation, foster::vm::CompileOptions { optimize })
                .unwrap(),
            Value::Integer(42)
        );
    }
}

#[test]
fn for_loops_need_no_imports_and_do_not_capture_option_names() {
    let source = "enum Option = Some(Int) | None\nfunc main() -> Int { let total = 0\nfor item in [20, 22] { total = total + item }\ntotal }";
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));
    let compilation = foster::compile(source).unwrap();
    let module = compilation.hir.module_named("main").unwrap();
    assert!(compilation.hir.modules[module].imports.is_empty());
    assert!(
        compilation.hir.modules[module]
            .imports_with_spans
            .is_empty()
    );
}

#[test]
fn for_loop_bindings_do_not_leak() {
    let error =
        foster::compile("func main() { for item in [1] { let inside = item }\nprintln(item) }")
            .unwrap_err();
    assert!(error.message.contains("item"), "{error:?}");
    assert!(
        foster::compile("func main() { for item in [1] { let inside = item }\nprintln(inside) }")
            .is_err()
    );
    assert_eq!(
        foster::run("func main() -> Int { for item in [1] { item }\nlet item = 42\nitem }")
            .unwrap(),
        Value::Integer(42)
    );
}

#[test]
fn for_loops_reject_invalid_headers_and_non_iterable_values() {
    for source in [
        "func main() { for in [1] {} }",
        "func main() { for item [1] {} }",
        "func main() { for item in {} }",
        "func main() { for item in [1] }",
    ] {
        assert!(foster::parse(source).is_err(), "{source}");
    }
    let error = foster::compile("func main() { for item in 42 {} }").unwrap_err();
    assert!(error.message.contains("iterator"), "{error:?}");
}

#[test]
fn for_loops_work_in_recovering_parser_and_nested_functions() {
    let source = "func main() -> Int { func sum() -> Int { let n = 0\nfor item in [20, 22] { n = n + item }\nn }\nsum() }";
    let parsed = foster::parse_recovering(source).unwrap();
    assert!(parsed.diagnostics.is_empty());
    let package = foster::package::Package::from_program_with_core("main", parsed.program).unwrap();
    assert_eq!(
        foster::vm::run(&foster::compiler::check(package).unwrap()).unwrap(),
        Value::Integer(42)
    );
}

#[test]
fn for_loops_allow_unconditional_control_transfers() {
    let source = "func first() -> Int { for item in [42] { return item }\n0 }\nfunc main() -> Int { let count = 0\nfor item in [1, 2, 3] { count = count + 1\ncontinue }\nassert(count == 3)\nfirst() }";
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));
}
