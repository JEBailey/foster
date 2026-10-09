use foster::vm::Value;

#[test]
fn imported_associated_functions_resolve_self_to_the_implementing_type() {
    let source_root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/imported_self");
    let compilation = foster::check_package(&source_root).unwrap();
    assert_eq!(foster::vm::run(&compilation).unwrap(), Value::Integer(42));
}

#[test]
fn self_types_preserve_generic_factories_owned_results_and_reference_groups() {
    let source = include_str!("fixtures/programs/self_type.fos");
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));
}

#[test]
fn self_is_reserved_and_obsolete_receiver_syntax_is_rejected() {
    for source in [
        "type Item = {}\nimpl Item { func get(self) -> Int { 42 } }",
        "type Item = { pub func get(self) -> Int }",
        "type Item = {}\nimpl Item { func get(self: Self) -> self { move self } }",
        "func get(value: Self) -> Int { 42 }",
        "func get() -> Self { 42 }",
        "type Self = {}",
        "type Item<Self> = {}",
        "func get<Self>(value: Self) -> Self { value }",
        "type Item = {}\nimpl Item<Self> {}",
        "type Item = {}\nimpl Item { func get(self: Self<Int>) -> Int { 42 } }",
        "type Item = {}\nimpl Item { func get(self: ref[self] Self) -> Self { self } }",
        "type Item = { pub func combine(self: Self, other: Self) -> Self }",
    ] {
        assert!(foster::compile(source).is_err(), "accepted {source}");
    }
}

#[test]
fn formatter_preserves_self_type_spelling() {
    let source = "type Item = {}\nimpl Item {\nfunc identity(self: Self) -> Self [consume self] { move self }\n}\n";
    let formatted = foster::formatter::format(source).unwrap();
    assert!(formatted.contains("identity(self: Self) -> Self"));
    assert_eq!(formatted, foster::formatter::format(&formatted).unwrap());
}

#[test]
fn implementation_parameters_can_use_the_concrete_self_type() {
    let source = r#"
type Other = {}
type Item = { pub value: Int }
impl Item {
    func add(self: Self, other: Self) -> Self { Item { value: self.value + other.value } }
}
func main() -> Int { Item { value: 20 }.add(Item { value: 22 }).value }
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));
    assert!(
        foster::compile(&source.replace(".add(Item { value: 22 })", ".add(Other {})")).is_err()
    );
}
