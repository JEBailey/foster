use foster::vm::Value;

#[test]
fn impl_only_methods_do_not_become_requirements() {
    let source = r#"
type Original = {
    pub value: Int
    pub func required(self) -> Int
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
    pub func apply<T>(self, value: T) -> T [consume value]
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
                assert!(
                    definition.methods.iter().any(|method| method.name == name),
                    "{}.{} has no required declaration",
                    compilation.hir.modules[function.module].name,
                    function.name
                );
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
        assert!(
            compilation.types.record_methods[&record].contains(name),
            "{}.{} has no required declaration",
            compilation.hir.modules[function.module].name,
            function.name
        );
        audited += 1;
    }
    assert!(audited > 150, "audited only {audited} instance methods");
}

#[test]
fn list_requirements_are_callable_through_a_composed_view() {
    assert_eq!(
        foster::run(
            r#"
import core.result
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
    let Some(Some(foster::ast::TypeExpr::Named(name, arguments))) =
        function.parameter_types.first()
    else {
        return false;
    };
    name == owner
        && arguments.len() == parameters.len()
        && arguments.iter().zip(parameters).all(|(argument, parameter)| {
            matches!(argument, foster::ast::TypeExpr::Named(name, nested) if name == parameter && nested.is_empty())
        })
}
