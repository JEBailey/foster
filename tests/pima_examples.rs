use std::fs;
use std::path::{Path, PathBuf};

const PIMA_COUNTERPARTS: &[&str] = &[
    "birthday_paradox",
    "closure",
    "code_blocks",
    "curried_example",
    "fibonacci",
    "file_server",
    "file_server_lib",
    "foreach",
    "function_test",
    "http_server_lib",
    "import_test",
    "json_parser",
    "list",
    "maps",
    "newton",
    "object_test",
    "patterns",
    "repository_analyzer",
    "repository_analyzer_lib",
    "repository_analyzer_test",
    "showcase",
    "test",
    "timing",
    "while",
];

fn examples_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/pima")
}

#[test]
fn every_pima_example_has_a_foster_counterpart() {
    for name in PIMA_COUNTERPARTS {
        let file = examples_root().join(format!("{name}.foster"));
        let package = examples_root().join(name);
        assert!(
            file.is_file() || package.is_dir(),
            "missing Foster counterpart: {} or {}",
            file.display(),
            package.display()
        );
    }
}

#[test]
fn pima_corpus_runs_with_and_without_optimization() {
    let mut examples = fs::read_dir(examples_root())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "foster")
        })
        .collect::<Vec<_>>();
    examples.sort();

    for path in examples {
        let source = fs::read_to_string(&path).unwrap();
        for optimize in [false, true] {
            foster::run_with_options(&source, foster::vm::CompileOptions { optimize })
                .unwrap_or_else(|error| {
                    panic!(
                        "{} failed with optimize={optimize}: {error}",
                        path.display()
                    )
                });
        }
    }

    let json_parser = examples_root().join("json_parser");
    for optimize in [false, true] {
        foster::run_package_with_options(&json_parser, foster::vm::CompileOptions { optimize })
            .unwrap_or_else(|error| {
                panic!(
                    "{} failed with optimize={optimize}: {error}",
                    json_parser.display()
                )
            });
    }
}
