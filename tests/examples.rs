use std::fs;
use std::path::{Path, PathBuf};

fn showcase_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/showcase")
}

#[test]
fn showcase_examples_run_with_and_without_optimization() {
    let mut examples = fs::read_dir(showcase_root())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "fos"))
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

    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    for package in ["json_parser", "modules"] {
        let path = manifest_dir.join("examples").join(package);
        for optimize in [false, true] {
            foster::run_package_with_options(&path, foster::vm::CompileOptions { optimize })
                .unwrap_or_else(|error| {
                    panic!(
                        "{} failed with optimize={optimize}: {error}",
                        path.display()
                    )
                });
        }
    }
}
