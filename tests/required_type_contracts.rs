use foster::vm::Value;

#[test]
fn composition_rejects_conflicting_modes_for_the_same_parameter_types() {
    for (declaration, value) in [
        (
            "type Combined = & Borrowing & Consuming & {}",
            "Combined {}",
        ),
        (
            "enum Combined = Empty & Borrowing & Consuming",
            "Combined.Empty",
        ),
    ] {
        let source = format!(
            r#"
type Borrowing = {{ pub func accept(self: Self, value: String) -> Int }}
type Consuming = {{ pub func accept(self: Self, value: String) -> Int [consume value] }}
{declaration}
impl Combined {{ func accept(self: Self, value: String) -> Int {{ 42 }} }}
func main() -> Int {{ {value}.accept("test") }}
"#
        );
        let error = foster::compile(&source).unwrap_err();
        assert!(
            error
                .message
                .contains("incompatible definitions of method `accept`"),
            "{error}"
        );
    }
}

#[test]
fn impl_only_methods_do_not_become_requirements() {
    let source = r#"
type Original = {
    pub value: Int
    pub func required(self: Self) -> Int
}
impl Original {
    func required(self: Original) -> Int { self.value }
    func extra(self: Original) -> Int { 100 }
}
type Combined = & Original & {}
impl Combined {
    func required(self: Combined) -> Int { self.value }
}
func main() -> Int { Combined { value: 42 }.required() }
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));
    let missing = source.replace(
        "    func required(self: Combined) -> Int { self.value }",
        "",
    );
    assert!(
        foster::compile(&missing)
            .unwrap_err()
            .message
            .contains("missing required method `required`")
    );
}

#[test]
fn required_methods_have_independent_type_parameters() {
    let source = r#"
type Identity = {
    pub func apply<T>(self: Self, value: T) -> T [consume value]
}
type Implementation = & Identity & {}
impl Implementation {
    func apply<U>(self: Implementation, value: U) -> U [consume value] { value }
}
func through(identity: Identity) -> Int {
    assert(identity.apply(true))
    identity.apply(42)
}
func main() -> Int { through(Implementation {}) }
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));
    let specialized = source.replace(
        "func apply<U>(self: Implementation, value: U) -> U",
        "func apply(self: Implementation, value: Int) -> Int",
    );
    let error = foster::compile(&specialized).unwrap_err();
    assert!(
        error.message.contains("missing required method `apply`"),
        "{error}"
    );
}

#[test]
fn list_composition_carries_explicit_requirements() {
    let error = foster::compile(
        r#"
type Combined = & List<Int> & { pub marker: Int }
func main() -> Int { Combined { marker: 42 }.marker }
"#,
    )
    .unwrap_err();
    assert!(error.message.contains("missing required method"), "{error}");
}

#[test]
fn library_contracts_match_their_implementations() {
    let compilation = foster::check_package("library").unwrap();
    let mut audited = 0;
    let mut missing = Vec::new();
    for (_, function) in compilation.hir.functions.iter() {
        if !function.public || function.receiver.is_none() {
            continue;
        }
        let Some(owner) = &function.owner else {
            continue;
        };
        let Some(record) = compilation.hir.record_named(function.module, owner) else {
            if let Some(variant) = compilation.hir.variant_type_named(function.module, owner) {
                let definition = &compilation.hir.variant_types[variant];
                if !definition.public || !general_receiver(function, owner, &definition.parameters)
                {
                    continue;
                }
                let name = function.name.strip_prefix(&format!("{owner}.")).unwrap();
                if !(definition.methods.iter().any(|method| method.name == name)
                    || definition.compositions.iter().any(|contract| {
                        let foster::ast::TypeExpr::Named(contract, _) = contract else {
                            return false;
                        };
                        compilation
                            .hir
                            .visible_types(function.module, contract)
                            .iter()
                            .any(|ty| {
                                matches!(ty, foster::types::NominalTypeId::Record(record)
                                        if compilation.types.record_methods[record].contains(name))
                            })
                    }))
                {
                    missing.push(format!(
                        "{}.{} has no required declaration",
                        compilation.hir.modules[function.module].name, function.name
                    ));
                }
                audited += 1;
            }
            continue;
        };
        let definition = &compilation.hir.records[record];
        // Capability contracts deliberately leave derived helpers in impl blocks.
        if !definition.public || definition.fields.is_empty() {
            continue;
        }
        if !general_receiver(function, owner, &definition.parameters) {
            continue;
        }
        let name = function.name.strip_prefix(&format!("{owner}.")).unwrap();
        if !compilation.types.record_methods[&record].contains(name) {
            missing.push(format!(
                "{}.{} has no required declaration",
                compilation.hir.modules[function.module].name, function.name
            ));
        }
        audited += 1;
    }
    assert!(audited > 150, "audited only {audited} instance methods");
    assert!(missing.is_empty(), "{}", missing.join("\n"));
}

#[test]
fn list_requirements_are_callable_through_a_composed_view() {
    assert_eq!(
        foster::run(
            r#"
import core.result
import core.result.*
import static core.result.*
type ListView = & List<Int> & {}
func inspect(values: ListView) -> Int { values.at(0).unwrap_or(0) }
func main() -> Int { inspect([42]) }
"#
        )
        .unwrap(),
        Value::Integer(42)
    );
}

fn general_receiver(function: &foster::hir::Function, owner: &str, parameters: &[String]) -> bool {
    // Constrained impl methods apply only to a subset of owner instantiations.
    // They must not become unconditional requirements on the owner's contract.
    if function
        .constraints
        .iter()
        .any(|constraint| parameters.contains(&constraint.parameter))
    {
        return false;
    }
    let Some(Some(foster::ast::TypeExpr::Named(name, arguments))) =
        function.parameters.first().map(|p| &p.ty)
    else {
        return false;
    };
    name == owner
        && arguments.len() == parameters.len()
        && arguments.iter().zip(parameters).all(|(argument, parameter)| {
            matches!(argument, foster::ast::TypeExpr::Named(name, nested) if name == parameter && nested.is_empty())
        })
}

#[test]
fn contract_audit_distinguishes_constrained_impl_methods() {
    let compilation = foster::compile(
        r#"
import core.copy.Copy
pub type Box<T> = { pub value: T }
impl Box<T> {
    pub func ordinary(self: Self) -> Int { 42 }
    pub func generic<U>(self: Self, value: U) -> U [consume value] { move value }
}
impl Box<T & Copy> {
    pub func copied(self: Self) -> T { self.value.copy() }
}
func main() -> Int { Box { value: 42 }.copied() }
"#,
    )
    .unwrap();
    let mut checked = 0;
    for (_, function) in compilation.hir.functions.iter() {
        let expected = match function.name.as_str() {
            "Box.ordinary" | "Box.generic" => true,
            "Box.copied" => false,
            _ => continue,
        };
        assert_eq!(general_receiver(function, "Box", &["T".into()]), expected);
        checked += 1;
    }
    assert_eq!(checked, 3);
}
