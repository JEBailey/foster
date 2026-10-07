//! Execute examples from authoritative library comments and the library guide.
use std::{fs, path::Path, process::Command};

use foster::vm::CompileOptions;
use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag, TagEnd};

fn run_examples(path: &Path, markdown: &str) -> usize {
    let mut count = 0;
    let mut source = None;
    for event in Parser::new(markdown) {
        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(language)))
                if language.as_ref() == "foster" =>
            {
                source = Some(String::new());
            }
            Event::Text(text) => {
                if let Some(source) = &mut source {
                    source.push_str(&text);
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some(source) = source.take() {
                    count += 1;
                    let label = format!("{} example {count}", path.display());
                    let compilation = foster::compile(&source)
                        .unwrap_or_else(|error| panic!("{label}: {error:?}"));
                    for optimize in [false, true] {
                        foster::vm::run_with_options(&compilation, CompileOptions { optimize })
                            .unwrap_or_else(|error| {
                                panic!("{label}, optimize={optimize}: {error:?}")
                            });
                    }
                }
            }
            _ => {}
        }
    }
    count
}

#[test]
fn library_source_and_guide_examples_execute() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("library");
    let mut count = run_examples(
        &root.join("README.md"),
        &fs::read_to_string(root.join("README.md")).unwrap(),
    );
    let mut paths = walkdir::WalkDir::new(&root)
        .into_iter()
        .map(|entry| entry.unwrap().into_path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "fos"))
        .collect::<Vec<_>>();
    paths.sort();
    for path in paths {
        let source = fs::read_to_string(&path).unwrap();
        let mut comments = String::new();
        for line in source.lines() {
            let line = line.trim_start();
            if let Some(comment) = line
                .strip_prefix("//!")
                .or_else(|| line.strip_prefix("///"))
            {
                comments.push_str(comment.strip_prefix(' ').unwrap_or(comment));
                comments.push('\n');
            } else {
                // Keep distinct documentation comments as separate Markdown blocks.
                comments.push('\n');
            }
        }
        count += run_examples(&path, &comments);
    }
    assert!(
        count >= 6,
        "library documentation examples must remain covered"
    );
}

#[test]
fn library_guide_commands_are_accepted_by_the_cli() {
    let guide = include_str!("../library/README.md");
    let mut checked = 0;
    for line in guide.lines() {
        let Some(arguments) = line.strip_prefix("foster ") else {
            continue;
        };
        let output = Command::new(env!("CARGO_BIN_EXE_foster"))
            .args(arguments.split_whitespace())
            .arg("--help")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{line}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        checked += 1;
    }
    assert!(checked > 0);
}
