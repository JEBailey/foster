use foster::vm::Value;

#[test]
fn generic_blocks_supply_receiver_types_and_preserve_member_generics() {
    let source = r#"
type Box<T> = { value: T }
impl Box<T> {
    func new(value: T) -> Box<T> { Box { value } }
    func get(self) -> T { self.value }
    func apply<U>(self, transform: func(T) -> U) -> U { transform(self.value) }
}
impl Box<T> {
    func again(self) -> T { self.get() }
}
func main() -> Int {
    let box = Box.new(20)
    box.again() + box.apply((value: Int) -> value + 2)
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));
    let program = foster::parse(source).unwrap();
    assert_eq!(program.implementations.len(), 2);
    let apply = program
        .functions
        .iter()
        .find(|function| function.name == "Box.apply")
        .unwrap();
    assert_eq!(apply.type_parameters, ["T", "U"]);
    assert!(apply.receiver);
    assert!(source[apply.span.clone()].starts_with("func apply"));
}

#[test]
fn enum_blocks_support_inferred_receivers() {
    let source = r#"
enum Choice = Value(Int) | Empty
impl Choice {
    func get(self) -> Int {
        branch self {
            Choice.Value(value) -> value
            Choice.Empty -> 0
        }
    }
}
func main() -> Int { Choice.Value(42).get() }
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));
}

#[test]
fn rejects_invalid_block_structure_and_old_member_declarations() {
    for source in [
        "type Box = {}\nfunc Box.new() { Box {} }",
        "type Box = {}\nfunc new(self: Box) {}",
        "type Box = {}\nimpl Box { func Box.new() {} }",
        "type Box = {}\nimpl Box { impl Box {} }",
        "type Box = {}\nimpl Box { type Inner = {} }",
        "type Box = {}\nimpl Box { func new() {}",
        "type Box<T> = {}\nimpl Box<T, T> {}",
        "type Box<T> = {}\nimpl Box<T> { func get<T>(self) {} }",
        "pub impl Box {}",
        "func main() { impl Box {} }",
    ] {
        assert!(foster::parse(source).is_err(), "accepted {source}");
    }
    for source in [
        "impl Missing {}\nfunc main() {}",
        "type Box<T> = {}\nimpl Box<T, U> {}\nfunc main() {}",
        "type Box = {}\nimpl Box { func get(self) {} }\nimpl Box { func get(self) {} }\nfunc main() {}",
    ] {
        assert!(foster::compile(source).is_err(), "accepted {source}");
    }
}

#[test]
fn recovery_does_not_leak_members_from_a_damaged_block() {
    let source = "type Box = {}\nimpl Box {\n    func broken(self) { let x = }\n    func later() {}\n}\nfunc healthy() { 42 }";
    let parsed = foster::parse_recovering(source).unwrap();
    assert_eq!(parsed.diagnostics.len(), 1);
    assert_eq!(parsed.program.functions.len(), 1);
    assert_eq!(parsed.program.functions[0].name, "healthy");
    assert!(parsed.program.implementations.is_empty());
}

#[test]
fn formatter_keeps_member_documentation_inside_the_block() {
    let source = "type Box = {}\nimpl Box {\n/// Creates a box.\npub func new() {\nBox {}\n}\n}\n";
    let formatted = foster::formatter::format(source).unwrap();
    assert!(formatted.contains(
        "impl Box {\n    /// Creates a box.\n    pub func new() {\n        Box {}\n    }\n}"
    ));
    assert_eq!(formatted, foster::formatter::format(&formatted).unwrap());
    let parsed = foster::parse(&formatted).unwrap();
    assert_eq!(
        parsed.functions[0].documentation.as_deref(),
        Some("Creates a box.")
    );
}
