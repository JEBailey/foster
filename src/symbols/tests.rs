use super::*;
use crate::vm::{self, CompileOptions, Machine, Value};

fn compile(source: &str) -> Program {
    vm::compile_with_options(
        &crate::compile(source).unwrap(),
        CompileOptions { optimize: false },
    )
    .unwrap()
}

fn definition<'a>(program: &'a Program, name: &str) -> &'a Definition {
    program
        .symbols
        .modules
        .iter()
        .flat_map(|m| &m.definitions)
        .find(|d| d.symbol.name.module.path == "main" && d.symbol.name.name == name)
        .unwrap()
}

fn modules() -> Program {
    modules_with_sources(
        "import arithmetic\nfunc main() -> Int { arithmetic::answer() }",
        "pub func answer() -> Int { 42 }",
    )
}

fn modules_with_sources(main: &str, library: &str) -> Program {
    let mut package = crate::package::Package::from_program_with_core(
        "main",
        crate::parse("func main() -> Int { 0 }").unwrap(),
    )
    .unwrap();
    package.modules.get_mut("main").unwrap().program = Some(crate::parse(main).unwrap());
    package.modules.insert(
        "arithmetic".into(),
        crate::package::Module {
            name: "arithmetic".into(),
            source_path: None,
            source: None,
            origin: crate::package::ModuleOrigin::Dependency,
            program: Some(crate::parse(library).unwrap()),
        },
    );
    package.symbol_modules.insert(
        "arithmetic".into(),
        ("example.math".into(), "numbers".into()),
    );
    let compilation = crate::compiler::check(package).unwrap();
    vm::compile_with_options(&compilation, CompileOptions { optimize: false }).unwrap()
}

#[test]
fn symbolic_identity_survives_arena_parameter_and_generic_renaming() {
    let first = compile("pub func identity<T>(value: T) -> T { value }");
    let second = compile(
        "func unrelated() -> Bool { false }\npub func identity<U>(renamed: U) -> U { renamed }",
    );
    let first = definition(&first, "identity");
    let second = definition(&second, "identity");
    assert_ne!(first.function, second.function);
    assert_eq!(first.symbol, second.symbol);
    assert_eq!(first.descriptor, second.descriptor);
}

#[test]
fn groups_and_reference_result_dependencies_are_retained() {
    let first = compile(
        "pub func first[g: group Int](left: ref[g] Int, right: ref[g] Int) -> ref[g] Int { ref left }",
    );
    let renamed = compile(
        "pub func first[h: group Int](a: ref[h] Int, b: ref[h] Int) -> ref[h] Int { ref a }",
    );
    let descriptor = &definition(&first, "first").descriptor;
    assert_eq!(descriptor, &definition(&renamed, "first").descriptor);
    assert_eq!(descriptor.result_dependencies, vec![0]);
    assert!(!descriptor.fresh_result);
    assert_eq!(
        descriptor.groups,
        vec![("g0".into(), SymbolType::Primitive("int".into()))]
    );
}

#[test]
fn effects_and_return_types_are_checked_after_identity_lookup() {
    let program = compile("pub func identity(value: Int) -> Int { value }");
    let definition = definition(&program, "identity");
    let mut required = definition.descriptor.clone();
    required.effects.push(Effect {
        kind: "mut".into(),
        root: "p0".into(),
        path: vec![],
    });
    assert_eq!(
        program
            .symbols
            .resolve(&definition.symbol, &required)
            .unwrap(),
        function_id(definition.function)
    );
    let mut stronger = definition.descriptor.clone();
    stronger.effects = required.effects.clone();
    assert!(!definition.descriptor.accepts(&stronger));
    let mut wrong_result = required;
    wrong_result.result = SymbolType::Primitive("bool".into());
    assert!(
        program
            .symbols
            .resolve(&definition.symbol, &wrong_result)
            .is_err()
    );
    let mut wrong_mode = definition.descriptor.clone();
    wrong_mode.parameters[0].mode = Mode::Consume;
    assert!(
        program
            .symbols
            .resolve(&definition.symbol, &wrong_mode)
            .is_err()
    );
}

#[test]
fn module_imports_round_trip_and_link_to_new_implementation_ids() {
    let mut program = modules();
    let module = program
        .symbols
        .modules
        .iter()
        .find(|m| m.name.package == "example.math")
        .unwrap();
    assert_eq!(module.name.path, "numbers");
    let old = function_id(module.definitions[0].function);
    let new = function_id(program.functions.keys().map(|id| raw(*id)).max().unwrap() + 1);
    let body = program.functions.remove(&old).unwrap();
    program.functions.insert(new, body);
    program
        .symbols
        .modules
        .iter_mut()
        .find(|m| m.name.package == "example.math")
        .unwrap()
        .definitions[0]
        .function = raw(new);
    assert!(vm::verify(&program).is_err());
    link(&mut program).unwrap();
    assert_eq!(
        Machine::new(&program).run_main().unwrap(),
        Value::Integer(42)
    );
    let bytes = vm::encode_program(&program).unwrap();
    let decoded = vm::decode_program(&bytes).unwrap();
    assert_eq!(decoded, program);
    assert_eq!(vm::encode_program(&decoded).unwrap(), bytes);
}

#[test]
fn linking_translates_generic_specialization_keys_after_renaming() {
    let main = "import arithmetic\nfunc main() -> Int { arithmetic::echo(42) }";
    let mut program = modules_with_sources(main, "pub func echo<T>(value: T) -> T { value }");
    let updated = modules_with_sources(main, "pub func echo<U>(value: U) -> U { value }");
    let replacement = updated
        .symbols
        .modules
        .iter()
        .find(|m| m.name.package == "example.math")
        .unwrap()
        .definitions[0]
        .clone();
    let definition = &mut program
        .symbols
        .modules
        .iter_mut()
        .find(|m| m.name.package == "example.math")
        .unwrap()
        .definitions[0];
    assert_eq!(definition.symbol, replacement.symbol);
    let body = updated.functions[&function_id(replacement.function)].clone();
    program
        .functions
        .insert(function_id(definition.function), body);
    definition.generic_names = replacement.generic_names;
    link(&mut program).unwrap();
    let call = program.functions[&program.main.unwrap()]
        .instructions
        .iter()
        .find_map(|instruction| {
            if let Instruction::Call { specialization, .. } = instruction {
                Some(specialization)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(call[0].0, "U");
    assert_eq!(
        Machine::new(&program).run_main().unwrap(),
        Value::Integer(42)
    );
}

#[test]
fn bad_imports_fail_transactionally_and_private_symbols_are_not_exported() {
    let mut program = modules();
    let main = definition(&program, "main");
    assert!(
        program
            .symbols
            .resolve(&main.symbol, &main.descriptor)
            .is_err()
    );
    program
        .symbols
        .modules
        .iter_mut()
        .find(|m| m.name.path == "main")
        .unwrap()
        .imports[0]
        .required
        .suspends = false;
    let exporter = program
        .symbols
        .modules
        .iter_mut()
        .find(|m| m.name.package == "example.math")
        .unwrap();
    exporter.definitions[0].descriptor.suspends = true;
    let before = program.clone();
    assert!(
        link(&mut program)
            .unwrap_err()
            .message
            .contains("incompatible")
    );
    assert_eq!(program, before);
}

#[test]
fn duplicate_definitions_missing_imports_and_unknown_versions_are_rejected() {
    let base = modules();
    let mut program = base.clone();
    let duplicate = program.symbols.modules[0].definitions[0].clone();
    program.symbols.modules[0].definitions.push(duplicate);
    assert!(vm::verify(&program).is_err());
    let mut program = base.clone();
    program
        .symbols
        .modules
        .iter_mut()
        .find(|m| m.name.path == "main")
        .unwrap()
        .imports
        .clear();
    assert!(vm::verify(&program).is_err());
    let mut program = base;
    program.symbols.version += 1;
    assert!(vm::encode_program(&program).is_err());
}

#[test]
fn forged_descriptor_types_are_rejected_even_when_the_symbol_key_agrees() {
    let mut program = compile("pub func identity(value: Int) -> Int { value }");
    let module = program
        .symbols
        .modules
        .iter_mut()
        .find(|m| m.name.path == "main")
        .unwrap();
    let definition = module
        .definitions
        .iter_mut()
        .find(|d| d.symbol.name.name == "identity")
        .unwrap();
    definition.descriptor.result = SymbolType::Primitive("bool".into());
    assert!(
        vm::encode_program(&program)
            .unwrap_err()
            .to_string()
            .contains("descriptor type")
    );
}

#[test]
fn symbol_container_order_does_not_change_the_binary() {
    let mut program = modules();
    let bytes = vm::encode_program(&program).unwrap();
    program.symbols.modules.reverse();
    for module in &mut program.symbols.modules {
        module.definitions.reverse();
        module.types.reverse();
        module.imports.reverse();
    }
    assert_eq!(bytes, vm::encode_program(&program).unwrap());
}

#[test]
fn overloads_and_multiple_closures_have_distinct_symbols() {
    let program = compile(
        "pub func value(x: Int) -> Int { x }\npub func value(x: Bool) -> Bool { x }\nfunc main() -> Int { let a = (x: Int) -> x\nlet b = (x: Int) -> x + 1\na(b(41)) }",
    );
    assert_eq!(
        Machine::new(&program).run_main().unwrap(),
        Value::Integer(42)
    );
    let overloads = program
        .symbols
        .modules
        .iter()
        .flat_map(|m| &m.definitions)
        .filter(|d| d.symbol.name.name == "value")
        .count();
    assert_eq!(overloads, 2);
}
