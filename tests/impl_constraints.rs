//! Implementation bounds use ordinary structural member conformance.

fn runs(source: &str, expected: i64) {
    let compilation = foster::compile(source).unwrap();
    assert_eq!(
        foster::vm::run(&compilation).unwrap(),
        foster::vm::Value::Integer(expected)
    );
    foster::native::prepare(&compilation).unwrap();
}

#[test]
fn symbolic_constraints_are_order_independent_and_deduplicated() {
    let mut descriptors = Vec::new();
    for requirements in ["A & B", "B & A", "A & B & A"] {
        let source = format!(
            "type A = {{ pub value: Int }}\ntype B = {{ pub flag: Bool }}\ntype Box<T> = {{ value: T }}\nimpl Box<T & {requirements}> {{ func answer(self) -> Int {{ 42 }} }}\nfunc main() -> Int {{ 42 }}"
        );
        let compilation = foster::compile(&source).unwrap();
        let program = foster::vm::compile(&compilation).unwrap();
        let descriptor = program
            .metadata
            .symbols
            .modules
            .iter()
            .flat_map(|module| &module.definitions)
            .find(|definition| definition.symbol.name.name == "Box.answer")
            .unwrap()
            .descriptor
            .clone();
        assert_eq!(descriptor.constraints.len(), 2);
        descriptors.push(descriptor);
    }
    assert!(descriptors.windows(2).all(|pair| pair[0] == pair[1]));
}

#[test]
fn constrained_bodies_keep_requirements_in_closures_and_reference_calls() {
    runs(
        r#"
import core.copy
type Box<T> = { value: T }
impl Box<T & Copy> {
    func copied(self) -> T [read self] {
        let duplicate = (value: T) -> value.copy()
        duplicate(self.value)
    }
}
func inspect[g: group Box<Int>](value: ref[g] Box<Int>) -> Int [read g] { value.copied() }
func main() -> Int {
    let container = Box { value: 42 }
    inspect(ref container)
}
"#,
        42,
    );
}

#[test]
fn constraints_expose_required_fields_without_erasing_values() {
    runs(
        r#"
type HasValue = { pub value: Int }
type Item = { pub value: Int, extra: Bool }
type Box<T> = { value: T }
impl Box<T & HasValue> { func answer(self) -> Int [read self] { self.value.value } }
func main() -> Int { Box { value: Item { value: 42, extra: true } }.answer() }
"#,
        42,
    );
}

#[test]
fn parameterized_requirements_keep_argument_identity() {
    runs(
        r#"
type Reads<U> = { pub func read(self) -> U [read self] }
type Item = { value: Int }
impl Item { func read(self) -> Int [read self] { self.value } }
type Pair<K, V> = { key: K, value: V }
impl Pair<K, V & Reads<K>> {
    func read(self) -> K [read self] { self.value.read() }
}
func main() -> Int { Pair { key: 0, value: Item { value: 42 } }.read() }
"#,
        42,
    );
}

#[test]
fn structural_arguments_check_conditional_method_availability() {
    let source = r#"
import core.copy
type Answer = { pub func answer(self) -> Int }
pub type Box<T> = { value: T }
impl Box<T & Copy> { pub func answer(self) -> Int { 42 } }
type Item = { value: Int }
func answer(value: Answer) -> Int { value.answer() }
func main() -> Int { answer(Box { value: 1 }) }
"#;
    runs(source, 42);
    assert!(
        foster::compile(&source.replace("value: 1 })", "value: Item { value: 1 } })")).is_err()
    );
}

#[test]
fn unconstrained_generic_cannot_forward_to_constrained_method() {
    let source = r#"
import core.copy
type Box<T> = { value: T }
impl Box<T & Copy> { func answer(self) -> Int { 42 } }
func answer<T>(value: Box<T>) -> Int { value.answer() }
func main() -> Int { answer(Box { value: 1 }) }
"#;
    let error = foster::compile(source).err().unwrap();
    assert!(error.message.contains("constraint"), "{}", error.message);
}

#[test]
fn invalid_requirement_is_rejected_even_without_a_call() {
    let error = foster::compile("type Box<T> = { value: T }\nimpl Box<T & Int> { func answer(self) -> Int { 42 } }\nfunc main() -> Int { 42 }").err().unwrap();
    assert!(
        error.message.contains("structural record requirements"),
        "{}",
        error.message
    );
}

#[test]
fn conditional_runtime_hooks_are_rejected() {
    for member in [
        "copy(self) -> self [read self] { self }",
        "deinit(self) -> () { () }",
    ] {
        let source = format!(
            "import core.copy\ntype Box<T> = {{ value: T }}\nimpl Box<T & Copy> {{ func {member} }}\nfunc main() -> Int {{ 42 }}"
        );
        let error = foster::compile(&source).err().unwrap();
        assert!(
            error.message.contains("runtime generic evidence"),
            "{}",
            error.message
        );
    }
}

#[test]
fn overload_selection_filters_unsatisfied_constraints() {
    let source = r#"
import core.copy
type Item = { value: Int }
type Box<T> = { value: T }
impl Box<T & Copy> { func answer(self, value: Int) -> Int { value } }
impl Box<T> { func answer(self, value: Bool) -> Int { 42 } }
func main() -> Int { Box { value: Item { value: 1 } }.answer(true) }
"#;
    runs(source, 42);
    let error = foster::compile(&source.replace(".answer(true)", ".answer(42)"))
        .err()
        .unwrap();
    assert!(error.message.contains("no overload"), "{}", error.message);
}

#[test]
fn multiple_requirements_and_generic_forwarding() {
    runs(
        r#"
import core.copy
type Number = { pub func number(self) -> Int [read self] }
pub type Item = { value: Int }
impl Item {
    pub func copy(self) -> self [read self] { Item { value: self.value } }
    pub func number(self) -> Int [read self] { self.value }
}
type Box<T> = { value: T }
impl Box<T & Number & Copy> {
    func copied(self) -> T [read self] { self.value.copy() }
    func number(self) -> Int [read self] { self.copied().number() }
}
func main() -> Int { Box { value: Item { value: 42 } }.number() }
"#,
        42,
    );
}

#[test]
fn constrained_associated_function_value_is_checked() {
    let source = r#"
import core.copy
type Box<T> = { value: T }
impl Box<T & Copy> { func answer(value: T) -> Int { 42 } }
type Item = { value: Int }
func main() -> Int {
    let answer = Box.answer
    answer(Item { value: 1 })
}
"#;
    assert!(foster::compile(source).is_err());
    runs(
        &source.replace("answer(Item { value: 1 })", "answer(1)"),
        42,
    );
}

#[test]
fn conditional_methods_do_not_discharge_unconditional_contracts() {
    let error = foster::compile(
        r#"
import core.copy
pub type Box<T> = { value: T, pub func answer(self) -> Int }
impl Box<T & Copy> { pub func answer(self) -> Int { 42 } }
func main() -> Int { 42 }
"#,
    )
    .err()
    .unwrap();
    assert!(error.message.contains("constraint"), "{}", error.message);
}

#[test]
fn requirement_checks_result_and_effects() {
    for implementation in [
        "pub func copy(self) -> Int [read self] { self.value }",
        "pub func copy(self) -> self [mut self] { self.value = 1\n Item { value: self.value } }",
    ] {
        let source = format!(
            r#"
import core.copy
pub type Item = {{ value: Int }}
impl Item {{ {implementation} }}
type Box<T> = {{ value: T }}
impl Box<T & Copy> {{ func answer(self) -> Int {{ 42 }} }}
func main() -> Int {{ Box {{ value: Item {{ value: 1 }} }}.answer() }}
"#
        );
        let error = foster::compile(&source).err().unwrap();
        assert!(error.message.contains("constraint"), "{}", error.message);
    }
}

#[test]
fn runtime_conformance_cannot_erase_conditional_methods() {
    let error = foster::compile(
        r#"
import core.copy
type Answer = { pub func answer(self) -> Int }
pub type Box<T> = { value: T }
impl Box<T & Copy> { pub func answer(self) -> Int { 42 } }
type Item = { value: Int }
func inspect<T>(value: T) -> Int {
    branch value {
        is Answer -> value.answer()
        _ -> 0
    }
}
func main() -> Int {
    inspect(Box { value: 1 }) + inspect(Box { value: Item { value: 1 } })
}
"#,
    )
    .err()
    .unwrap();
    assert!(
        error
            .message
            .contains("runtime `is` cannot test conditional method"),
        "{}",
        error.message
    );
}

#[test]
fn constraints_do_not_create_ordered_specialization() {
    for methods in [
        "impl Box<T> { func answer(self) -> Int { 0 } }\nimpl Box<T & Copy> { func answer(self) -> Int { 42 } }",
        "impl Box<T & Copy> { func answer(self) -> Int { 42 } }\nimpl Box<T> { func answer(self) -> Int { 0 } }",
    ] {
        let source = format!(
            "import core.copy\ntype Box<T> = {{ value: T }}\n{methods}\nfunc main() -> Int {{ 42 }}"
        );
        assert!(foster::compile(&source).is_err());
    }
}

#[test]
fn copy_constraint_preserves_the_concrete_result_type() {
    let compilation = foster::compile(
        r#"
import core.copy
type Box<T> = { value: T }
impl Box<T & Copy> {
    func copied(self) -> T [read self] { self.value.copy() }
}
func main() -> Int { Box { value: 42 }.copied() }
"#,
    )
    .unwrap();
    assert_eq!(
        foster::vm::run(&compilation).unwrap(),
        foster::vm::Value::Integer(42)
    );
    foster::native::prepare(&compilation).unwrap();
}

#[test]
fn missing_copy_is_rejected_even_when_the_method_body_does_not_copy() {
    let error = foster::compile(
        r#"
import core.copy
type NoCopy = { value: Int }
type Box<T> = { value: T }
impl Box<T & Copy> { func answer(self) -> Int { 42 } }
func main() -> Int { Box { value: NoCopy { value: 1 } }.answer() }
"#,
    )
    .err()
    .unwrap();
    assert!(error.message.contains("constraint"), "{}", error.message);
}

#[test]
fn structural_method_satisfies_copy_without_declaring_composition() {
    let compilation = foster::compile(
        r#"
import core.copy
pub type Item = { value: Int }
impl Item { pub func copy(self) -> self [read self] { Item { value: self.value } } }
type Box<T> = { value: T }
impl Box<T & Copy> { func copied(self) -> T [read self] { self.value.copy() } }
func main() -> Int { Box { value: Item { value: 42 } }.copied().value }
"#,
    )
    .unwrap();
    assert_eq!(
        foster::vm::run(&compilation).unwrap(),
        foster::vm::Value::Integer(42)
    );
    foster::native::prepare(&compilation).unwrap();
}
