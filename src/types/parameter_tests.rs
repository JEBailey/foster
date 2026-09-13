use super::*;

#[test]
fn inference_adapter_rejects_missing_and_extra_modes() {
    let mut types = Arena::new();
    let ty = types.alloc(Type::Int);
    for (parameters, modes) in [
        (vec![ty], vec![]),
        (vec![], vec![ast::ParameterMode::Borrow]),
    ] {
        assert!(std::panic::catch_unwind(|| Parameter::from_parts(parameters, modes)).is_err());
    }
}

#[test]
fn generic_method_partial_application_preserves_consuming_parameter() {
    let source = r#"
type Receiver = {}
impl Receiver {
    func choose<T>(self, label: String, value: T) -> T [consume label, consume value] { value }
}
func main() -> Int {
    let choose = Receiver {}.choose(_, 42)
    let label = "owned"
    choose(move label)
}
"#;
    let compilation = crate::compile(source).unwrap();
    let (partial, _) = compilation
        .hir
        .functions
        .iter()
        .find(|(_, f)| f.name.ends_with("$partial"))
        .unwrap();
    let signature = compilation.types.function_type(partial).unwrap();
    assert_eq!(signature.parameters.len(), 1);
    assert_eq!(signature.parameters[0].mode, ast::ParameterMode::Consume);
    assert_eq!(
        compilation.types.display(signature.parameters[0].ty),
        "String"
    );
    assert_eq!(
        crate::vm::run(&compilation).unwrap(),
        crate::vm::Value::Integer(42)
    );
    let error = crate::compile(&source.replace("choose(move label)", "choose(label)")).unwrap_err();
    assert!(error.to_string().contains("move"), "{error}");
}

#[test]
fn nested_callable_parameters_retain_type_and_ownership_in_order() {
    let compilation = crate::compile(r#"
func sink(label: String, number: Int) -> Int [consume label] { number }
func apply(action: func(consume String, Int) -> Int, label: String, number: Int) -> Int [consume label] {
    action(move label, number)
}
func main() -> Int { apply(sink, "label", 42) }
"#).unwrap();
    let module = compilation.hir.module_named("main").unwrap();
    let apply = compilation.hir.function_named(module, "apply").unwrap();
    let signature = compilation.types.function_type(apply).unwrap();
    let Type::Function(callable) = &compilation.types.types[signature.parameters[0].ty] else {
        panic!("first parameter must be a callable");
    };
    let pairs = callable
        .parameters
        .iter()
        .map(|p| (compilation.types.display(p.ty), p.mode))
        .collect::<Vec<_>>();
    assert_eq!(
        pairs,
        vec![
            ("String".into(), ast::ParameterMode::Consume),
            ("Int".into(), ast::ParameterMode::Borrow)
        ]
    );
    assert_eq!(
        compilation.types.display(signature.parameters[0].ty),
        "func(consume String, Int) -> Int"
    );
    assert_eq!(
        crate::vm::run(&compilation).unwrap(),
        crate::vm::Value::Integer(42)
    );
}

#[test]
fn hir_annotations_and_spans_follow_their_parameter_locals() {
    let source = "func select(first: Int, second: Bool, third: String) -> Int { first }\nfunc main() -> Int { let partial = select(_, true, \"value\")\n partial(42) }";
    let compilation = crate::compile(source).unwrap();
    let module = compilation.hir.module_named("main").unwrap();
    let select = compilation.hir.function_named(module, "select").unwrap();
    assert_eq!(compilation.hir.functions[select].parameters.len(), 3);
    for (parameter, (name, annotation)) in compilation.hir.functions[select]
        .parameters
        .iter()
        .zip([("first", "Int"), ("second", "Bool"), ("third", "String")])
    {
        let local = &compilation.hir.locals[parameter.local];
        assert_eq!(local.function, select);
        assert_eq!(local.name, name);
        assert_eq!(
            parameter.ty,
            Some(ast::TypeExpr::Named(annotation.into(), vec![]))
        );
        assert_eq!(&source[parameter.type_span.clone().unwrap()], annotation);
    }
    let (partial, declaration) = compilation
        .hir
        .functions
        .iter()
        .find(|(_, f)| f.name.ends_with("$partial"))
        .unwrap();
    assert_eq!(declaration.parameters.len(), 1);
    let parameter = &declaration.parameters[0];
    assert_eq!(compilation.hir.locals[parameter.local].function, partial);
    assert!(parameter.ty.is_none());
    assert!(parameter.type_span.is_none());
    assert_eq!(
        crate::vm::run(&compilation).unwrap(),
        crate::vm::Value::Integer(42)
    );
}
