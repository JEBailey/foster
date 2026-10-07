use foster::vm::Value;

const SOURCE: &str = include_str!("fixtures/programs/parameter_groups.fos");

#[test]
fn parameter_names_expose_storage_without_group_declarations() {
    assert_eq!(foster::run(SOURCE).unwrap(), Value::Integer(42));
}

#[test]
fn parameter_groups_reject_wrong_origins_and_expired_storage() {
    for source in [
        "func borrow(value: ref[value] Int) -> ref[value] Int { ref value }\nfunc main() { let values = [1]\nlet selected = borrow(values[0])\nvalues.push(2)\nselected.copy() }",
        "type Item = { value: Int }\nfunc borrow(item: Item) -> ref[item] Item { ref item }\nfunc main() { let callback = borrow\nlet escaped = :work { let local = Item { value: 1 }\ncallback(local) }\nescaped.value }",
        "type Item = { value: Int }\nfunc bad(left: Item, right: Item) -> ref[left] Item { ref right }",
        "type Item = { value: Int }\nfunc bad(value: Item) -> ref[missing] Item { ref value }",
        "type Item = { value: Int }\nfunc bad(value: Item) -> ref[value] Item [consume value] { ref value }",
        "type Item = { value: Int }\nfunc bad() -> ref[work] Item { :work { let item = Item { value: 1 }\nref item } }",
        "type Item = { value: Int }\nfunc borrow(item: Item) -> ref[item] Item { ref item }\nfunc main() { let escaped = :work { let local = Item { value: 1 }\nborrow(local) }\nescaped.value }",
    ] {
        assert!(foster::compile(source).is_err(), "accepted: {source}");
    }
}

#[test]
fn separate_group_declarations_are_rejected() {
    assert!(
        foster::parse("func borrow[g: group Int](value: ref[g] Int) -> ref[g] Int { ref value }")
            .is_err()
    );
}

#[test]
fn ordinary_parameter_origins_survive_callable_storage() {
    foster::compile(
        r#"
type Item = { value: Int }
func first(item: Item) -> ref[item] Item { ref item }
func second(other: Item) -> ref[other] Item { ref other }
func main() -> Int {
    let item = Item { value: 42 }
    let callbacks = [first, second]
    let borrowed = callbacks[0](item)
    borrowed.value
}
"#,
    )
    .unwrap();
}

#[test]
fn returned_closure_effects_follow_renamed_parameters() {
    foster::compile(
        r#"
type Item = { value: Int }
func reader(item: Item) -> func() -> Int [read item] { [ref item] () -> item.value }
func relay(other: Item) -> func() -> Int [read other] { reader(other) }
func main() -> Int { let item = Item { value: 42 }
let callback = relay(item)
callback() }
"#,
    )
    .unwrap();
}

#[test]
fn method_contracts_use_parameter_positions_for_group_names() {
    foster::compile(
        r#"
type Item = { value: Int }
type Reader = { pub func borrow(self: Self, item: Item) -> ref[item] Item }
type Implementation = & Reader & {}
impl Implementation {
    func borrow(self: Self, other: Item) -> ref[other] Item { ref other }
}
func main() -> Int {
    let reader = Implementation {}
    let item = Item { value: 42 }
    reader.borrow(item).value
}
"#,
    )
    .unwrap();
}

#[test]
fn call_substitution_does_not_rebind_an_actual_parameter_name() {
    foster::compile(
        r#"
type Item = { value: Int }
func first(left: Item, right: Item) -> ref[left] Item { ref left }
func relay(right: Item, other: Item) -> ref[right] Item { first(right, other) }
impl Item {
    func choose(self: Self, item: Item) -> ref[item] Item { ref item }
    func forward(self: Self, other: Item) -> ref[self] Item { other.choose(self) }
}
func main() -> Int { let item = Item { value: 42 }
relay(item, item).value }
"#,
    )
    .unwrap();
}
