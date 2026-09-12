use foster::ast::{TypeExpr, VariantKind};

#[test]
fn nested_receiver_results_reject_an_unrelated_implementation() {
    let error = foster::compile(
        r#"
import core.option
type Wrapped = { pub func wrap(self) -> Option<self> [consume self] }
type Invalid = & Wrapped & { value: Int }
impl Invalid {
    func wrap(self) -> Option<Int> [consume self] { Option.Some(self.value) }
}
func main() -> Int { let invalid = Invalid { value: 42 }
    0 }
"#,
    )
    .unwrap_err();
    assert!(error.to_string().contains("wrap"), "{error}");
}

#[test]
fn library_declarations_use_current_type_forms_and_explicit_public_signatures() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("library");
    let mut modules = 0;
    let audit = include_str!("../docs/library-contract-audit.md");
    for entry in walkdir::WalkDir::new(&root) {
        let entry = entry.unwrap();
        if !entry.file_type().is_file() || entry.path().extension().is_none_or(|ext| ext != "fos") {
            continue;
        }
        let source = std::fs::read_to_string(entry.path()).unwrap();
        let program = foster::parse(&source).unwrap();
        let module = entry
            .path()
            .strip_prefix(&root)
            .unwrap()
            .with_extension("")
            .components()
            .map(|part| part.as_os_str().to_str().unwrap())
            .collect::<Vec<_>>()
            .join(".");
        for name in program
            .records
            .iter()
            .filter(|ty| ty.public)
            .map(|ty| &ty.name)
            .chain(
                program
                    .variants
                    .iter()
                    .filter(|ty| ty.public)
                    .map(|ty| &ty.name),
            )
        {
            assert!(
                audit.contains(&format!("[{module}.{name}]")),
                "public type {module}.{name} needs a contract audit entry"
            );
        }
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
                "Queue",
                "Deque",
                "Stack",
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
            let expected: &[&str] = match record.name.as_str() {
                "String" => &["Copy", "Sequence", "Collection"],
                "Bytes" => &["Copy", "Sequence", "Collection", "Equality", "Hashing"],
                "List" => &["Sequence", "Collection"],
                "Symbol" | "TomlEntry" => &["Copy"],
                "File" => &["ReadWrite", "TextWriter"],
                "Connection" => &["Drop", "Duplex", "TextWriter", "Closable"],
                "Listener" => &["Drop", "Accepting", "Closable"],
                "SystemRandom" => &["RandomSource", "secure.EntropySource"],
                _ => &[],
            };
            for expected in expected {
                assert!(
                    record
                        .compositions
                        .iter()
                        .any(|ty| matches!(ty, TypeExpr::Named(name, _) if name == expected)),
                    "{} must explicitly compose {expected}",
                    record.name
                );
            }
        }
        for variant in &program.variants {
            if variant.name == "TomlValue" {
                assert!(
                    variant
                        .compositions
                        .iter()
                        .any(|ty| matches!(ty, TypeExpr::Named(name, _) if name == "Copy"))
                );
            }
        }
    }
    assert!(modules >= 50, "review must cover both core and std");
}

#[test]
fn resource_and_entropy_types_support_their_explicit_contract_views() {
    foster::compile(
        r#"
import std.fs
import std.io
import std.resource
import std.net.tcp
import std.random
import std.random.secure
func text<E>(value: TextWriter<E>) -> () {}
func binary<E>(value: ReadWrite<E>) -> () {}
func entropy(value: EntropySource) -> () {}
func inspect_file(value: File) -> () {
    text(value)
    binary(value)
}
func inspect_connection(value: Connection) -> () { text(value) }
func inspect_system(value: SystemRandom) -> () { entropy(value) }
func main() -> Int { 0 }
"#,
    )
    .unwrap();
}

#[test]
fn concrete_collections_work_through_shared_contracts() {
    let source = include_str!("fixtures/programs/collection_contracts.fos");
    let compilation = foster::compile(source).unwrap();
    assert_eq!(foster::vm::run(&compilation).unwrap().to_string(), "42");
}
