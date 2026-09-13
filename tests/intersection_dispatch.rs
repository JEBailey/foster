use foster::vm::{self, Value};

#[test]
fn reordered_intersection_requirements_share_dispatch_identity() {
    let template = r#"
type A = { pub first: Int }
type B = { pub second: Int }
type Item = { pub first: Int, pub second: Int }
type First = { pub func apply(self, value: A & B) -> Int }
type Second = { pub func apply(self, value: B & A) -> Int }
type Implementation = & First & {}
impl Implementation {
    func apply(self: Implementation, value: ORDER) -> Int { value.first }
}
func first(receiver: First, value: Item) -> Int { receiver.apply(value) }
func second(receiver: Second, value: Item) -> Int { receiver.apply(value) }
func main() -> Int {
    let receiver = Implementation {}
    let value = Item { first: 21, second: 7 }
    first(receiver, value) + second(receiver, value)
}
"#;
    for order in ["A & B", "B & A"] {
        check(&template.replace("ORDER", order));
    }
}

#[test]
fn reordered_generic_intersections_preserve_later_parameter_relationships() {
    let template = r#"
type A<T> = { pub first: T }
type B<T> = { pub second: T }
type Item = { pub first: Int, pub second: Bool }
type First = { pub func apply<T, U>(self, value: A<T> & B<U>, marker: T) -> Int }
type Second = { pub func apply<X, Y>(self, value: B<Y> & A<X>, marker: X) -> Int }
type Implementation = & First & {}
impl Implementation {
    func apply<T, U>(self: Implementation, value: ORDER, marker: T) -> Int { 21 }
}
func first(receiver: First, value: Item) -> Int { receiver.apply(value, 5) }
func second(receiver: Second, value: Item) -> Int { receiver.apply(value, 5) }
func main() -> Int {
    let receiver = Implementation {}
    let value = Item { first: 42, second: true }
    first(receiver, value) + second(receiver, value)
}
"#;
    for order in ["A<T> & B<U>", "B<U> & A<T>"] {
        check(&template.replace("ORDER", order));
    }
}

fn check(source: &str) {
    let compilation = foster::compile(source).unwrap();
    let slots = compilation
        .types
        .dispatch_keys
        .iter()
        .filter(|key| key.name == "apply")
        .collect::<Vec<_>>();
    assert_eq!(slots.len(), 1, "equivalent requirements must share a slot");
    let implementation = compilation
        .hir
        .functions
        .iter()
        .find(|(_, function)| function.name == "Implementation.apply")
        .unwrap()
        .0;
    assert_eq!(
        &compilation
            .types
            .method_dispatch_key(implementation, "apply")
            .unwrap(),
        slots[0]
    );
    for optimize in [false, true] {
        let program =
            vm::compile_with_options(&compilation, vm::CompileOptions { optimize }).unwrap();
        let program = vm::decode_program(&vm::encode_program(&program).unwrap()).unwrap();
        assert_eq!(
            vm::Machine::new(&program).run_main().unwrap(),
            Value::Integer(42)
        );
    }
}
