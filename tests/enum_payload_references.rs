#[test]
fn enum_payload_references_preserve_storage_and_bytecode_round_trips() {
    let compilation = foster::compile(include_str!(
        "fixtures/programs/enum_payload_references.fos"
    ))
    .unwrap();
    for optimize in [false, true] {
        let program =
            foster::vm::compile_with_options(&compilation, foster::vm::CompileOptions { optimize })
                .unwrap();
        assert_eq!(
            foster::vm::Machine::new(&program.clone().into_verified().unwrap())
                .run_main()
                .unwrap()
                .to_string(),
            "42"
        );
        let decoded =
            foster::vm::decode_program(&foster::vm::encode_program(&program).unwrap()).unwrap();
        assert_eq!(
            foster::vm::Machine::new(&decoded.clone().into_verified().unwrap())
                .run_main()
                .unwrap()
                .to_string(),
            "42"
        );
    }
}

#[test]
fn enum_payload_reference_cannot_escape_a_local_owner() {
    let error = foster::compile(
        r#"
type Item = { value: Int }
enum Choice = Stored(Item)
func invalid() {
    let choice = Choice.Stored(Item { value: 42 })
    branch choice { Choice.Stored(item) -> ref item }
}
"#,
    )
    .unwrap_err();
    assert_eq!(error.code.as_deref(), Some("E0402"), "{error}");
}

#[test]
fn enum_payload_reference_cannot_outlive_replacement() {
    let error = foster::compile(
        r#"
enum Choice = Empty | Stored(Int)
func borrow(value: Choice) -> ref[value] Int {
    branch value { Choice.Stored(item) -> ref item
        Choice.Empty -> panic("empty") }
}
func observe(value: ref[value] Int) -> Int { value }
func main() -> Int {
    let choice = Choice.Stored(42)
    let item = borrow(choice)
    choice = Choice.Empty
    observe(item)
}
"#,
    )
    .unwrap_err();
    assert_eq!(error.code.as_deref(), Some("E0401"), "{error}");
}
