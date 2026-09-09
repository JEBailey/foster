use foster::ast::{TypeExpr, VariantKind};

#[test]
fn library_declarations_use_current_type_forms_and_explicit_public_signatures() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("library");
    let mut modules = 0;
    for entry in walkdir::WalkDir::new(root) {
        let entry = entry.unwrap();
        if !entry.file_type().is_file() || entry.path().extension().is_none_or(|ext| ext != "fos") {
            continue;
        }
        let source = std::fs::read_to_string(entry.path()).unwrap();
        let program = foster::parse(&source).unwrap();
        modules += 1;
        for declaration in &program.variants {
            if declaration.kind == VariantKind::Alias {
                assert_eq!(declaration.alternatives.len(), 1, "{}", declaration.name);
            }
        }
        for (owner, methods) in program
            .records
            .iter()
            .map(|record| (&record.name, &record.methods))
            .chain(
                program
                    .variants
                    .iter()
                    .map(|variant| (&variant.name, &variant.methods)),
            )
        {
            for required in methods {
                assert!(
                    required.return_type.is_some(),
                    "{owner}.{} needs an explicit result",
                    required.name
                );
                // A required method's `self` type is supplied by its enclosing declaration.
                for parameter in required
                    .parameters
                    .iter()
                    .skip(usize::from(required.receiver))
                {
                    assert!(
                        parameter.ty.is_some(),
                        "{owner}.{} needs an explicit parameter type",
                        required.name
                    );
                }
            }
        }
        for function in program.functions.iter().filter(|f| f.public) {
            assert!(
                function.return_type.is_some(),
                "{}: {} needs a public result type",
                entry.path().display(),
                function.name
            );
            for parameter in &function.parameters {
                assert!(
                    parameter.ty.is_some(),
                    "{}: {} needs a public parameter type",
                    entry.path().display(),
                    function.name
                );
            }
        }
        for record in &program.records {
            if [
                "Collection",
                "Map",
                "Set",
                "Iterable",
                "Iterator",
                "Reader",
                "Writer",
                "Copy",
                "Drop",
            ]
            .contains(&record.name.as_str())
            {
                assert!(
                    record.fields.is_empty(),
                    "{} must be a storage-free contract",
                    record.name
                );
            }
            if record.name == "HashMap" || record.name == "HashSet" {
                let expected = if record.name == "HashMap" {
                    "Map"
                } else {
                    "Set"
                };
                assert!(
                    record
                        .compositions
                        .iter()
                        .any(|ty| matches!(ty, TypeExpr::Named(name, _) if name == expected)),
                    "{} must compose {expected}",
                    record.name
                );
            }
        }
    }
    assert!(modules >= 50, "review must cover both core and std");
}

#[test]
fn concrete_collections_work_through_shared_contracts() {
    let source = include_str!("fixtures/programs/collection_contracts.fos");
    let compilation = foster::compile(source).unwrap();
    assert_eq!(foster::vm::run(&compilation).unwrap().to_string(), "42");
}
