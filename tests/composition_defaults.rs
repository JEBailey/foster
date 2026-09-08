fn run(source: &str) -> String {
    let compilation = foster::compile(source).unwrap();
    foster::vm::run(&compilation).unwrap().to_string()
}

#[test]
fn rightmost_defaults_and_local_implementations_win() {
    assert_eq!(
        run(include_str!("fixtures/programs/ordered_composition.fos")),
        "42"
    );
}

#[test]
fn conflicting_fields_and_return_types_are_rejected() {
    for source in [
        "type A = { pub value: Int } type B = { pub value: String } type C = & A & B & {} func main() {}",
        "pub type A = {} pub type B = {} impl A { pub func value(self) -> Int { 1 } } impl B { pub func value(self) -> String { \"bad\" } } type C = & A & B & {} func main() { C {}.value() }",
    ] {
        let error = foster::compile(source).unwrap_err().to_string();
        assert!(error.contains("incompatible"), "{error}");
    }
}

#[test]
fn defaults_specialize_generics_and_preserve_overloads() {
    assert_eq!(
        run(include_str!(
            "fixtures/programs/ordered_generic_defaults.fos"
        )),
        "42"
    );
}

#[test]
fn incompatible_effects_and_ownership_are_rejected() {
    for implementation in [
        "pub func action(self, value: Int) -> Int [mut self.value] { self.value = value\nself.value }",
        "pub func action(self, value: Int) -> Int [consume value] { value }",
    ] {
        let source = format!(
            r#"
pub type A = {{ pub value: Int }}
pub type B = {{ pub value: Int }}
impl A {{ pub func action(self, value: Int) -> Int [read self.value, read value] {{ self.value + value }} }}
impl B {{ {implementation} }}
type C = & A & B & {{}}
func main() {{ C {{ value: 1 }}.action(2) }}
"#
        );
        let error = foster::compile(&source).unwrap_err().to_string();
        assert!(error.contains("incompatible"), "{error}");
    }
}

#[test]
fn defaults_keep_lexical_scope_across_modules() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/composition_defaults");
    assert_eq!(foster::run_package(path).unwrap().to_string(), "42");
}

#[test]
fn a_purer_default_does_not_narrow_a_declared_contract() {
    assert_eq!(
        run(r#"
pub type A = { pub value: Int, pub func get(self) -> Int [read self] }
pub type B = { pub value: Int, pub func get(self) -> Int [read self] }
impl A { pub func get(self) -> Int { 0 } }
impl B { pub func get(self) -> Int { self.value } }
type C = & A & B & {}
func main() -> Int { C { value: 42 }.get() }
"#),
        "42"
    );
}
