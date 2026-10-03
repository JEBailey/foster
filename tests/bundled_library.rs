use foster::{package::Package, vm};

#[test]
fn bundled_interfaces_replace_library_bodies_and_share_decoded_artifacts() {
    let load = |source: &str| {
        Package::from_program_with_core("main", foster::parse(source).unwrap()).unwrap()
    };
    let minimal = load("func main() -> Int { 42.copy() }");
    let repeated = load("func main() -> Int { 7 }");
    assert_eq!(minimal.libraries.len(), 1);
    assert!(std::sync::Arc::ptr_eq(
        &minimal.libraries[0],
        &repeated.libraries[0]
    ));
    assert!(!minimal.modules.contains_key("std.json"));
    let full = load("import std.json\nfunc main() -> Int { 42 }");
    assert_eq!(full.libraries.len(), 1);
    assert!(full.modules.contains_key("std.json"));
    assert!(!std::sync::Arc::ptr_eq(
        &minimal.libraries[0],
        &full.libraries[0]
    ));
    for package in [&minimal, &full] {
        let library = &package.libraries[0];
        assert_eq!(library.interface.package, "foster");
        assert!(library.interface.embedded.is_empty());
        for module in &library.interface.modules {
            assert!(module.declarations.tests.is_empty());
            for function in &module.declarations.functions {
                assert!(function.body.is_empty());
                assert!(function.body_is_recovery_stub);
            }
        }
        let int = &package.modules["core.int"];
        assert!(int.source.as_deref().unwrap().contains("func copy"));
        let copy = int
            .program
            .as_ref()
            .unwrap()
            .functions
            .iter()
            .find(|f| f.name == "Int.copy")
            .unwrap();
        assert!(copy.span.end > copy.span.start);
    }
    assert_eq!(
        vm::run(&foster::compiler::check(minimal).unwrap()).unwrap(),
        vm::Value::Integer(42)
    );
}

#[test]
fn bundled_generic_code_links_for_vm_and_native_consumers() {
    for (source, expected) in [
        (include_str!("fixtures/programs/hash_collections.fos"), "42"),
        (include_str!("fixtures/programs/json.fos"), "Result.Ok(42)"),
        (
            include_str!("fixtures/programs/library_algorithms.fos"),
            "42",
        ),
    ] {
        let compilation = foster::compile(source).unwrap();
        assert!(!compilation.hir.external_functions.is_empty());
        for optimize in [false, true] {
            let result =
                vm::run_with_options(&compilation, vm::CompileOptions { optimize }).unwrap();
            assert_eq!(result.to_string(), expected);
        }
        foster::native::prepare(&compilation).unwrap();
    }
}
