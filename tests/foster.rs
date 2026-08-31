use std::path::Path;
use std::process::Command;

fn run_suite(path: &str, optimize: bool, minimum_tests: usize) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_foster"));
    command
        .arg("test")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join(path));
    if !optimize {
        command.arg("--no-optimize");
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "foster test {path} failed with optimize={optimize}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("test result: ok"), "{stdout}");
    let discovered = stdout
        .lines()
        .find_map(|line| {
            line.strip_prefix("running ")?
                .strip_suffix(" test(s)")?
                .parse::<usize>()
                .ok()
        })
        .expect("Foster test output should report the discovered test count");
    assert!(
        discovered >= minimum_tests,
        "{path} discovered only {discovered} tests; expected at least {minimum_tests}"
    );
}

#[test]
fn portable_language_suite_passes_with_and_without_optimization() {
    for optimize in [false, true] {
        run_suite("tests/foster", optimize, 56);
    }
}

#[test]
fn standard_library_suite_passes_with_and_without_optimization() {
    for optimize in [false, true] {
        run_suite("library", optimize, 30);
    }
}

#[test]
fn public_library_implementations_have_native_or_host_integration_coverage() {
    let library = Path::new(env!("CARGO_MANIFEST_DIR")).join("library");
    let host_integrated = ["std/env.fos", "std/fs.fos", "std/net/tcp.fos"];
    let externally_tested = ["core/range.fos"];
    let mut uncovered = Vec::new();
    for entry in walkdir::WalkDir::new(&library) {
        let entry = entry.unwrap();
        if !entry.file_type().is_file()
            || entry
                .path()
                .extension()
                .is_none_or(|extension| extension != "fos")
        {
            continue;
        }
        let source = std::fs::read_to_string(entry.path()).unwrap();
        if !source.lines().any(|line| line.starts_with("pub func "))
            || source.lines().any(|line| line.starts_with("test \""))
        {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(&library)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if !host_integrated.contains(&relative.as_str())
            && !externally_tested.contains(&relative.as_str())
        {
            uncovered.push(relative);
        }
    }
    assert!(
        uncovered.is_empty(),
        "public library modules need Foster tests or explicit host integration coverage: {uncovered:?}"
    );
}

#[test]
fn test_sources_do_not_depend_on_reader_examples() {
    let tests = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let forward_example_path = ["examples", "/"].concat();
    let backward_example_path = ["examples", "\\"].concat();
    for entry in walkdir::WalkDir::new(tests) {
        let entry = entry.unwrap();
        if !entry.file_type().is_file()
            || entry
                .path()
                .extension()
                .is_none_or(|extension| extension != "rs")
        {
            continue;
        }
        let source = std::fs::read_to_string(entry.path()).unwrap();
        assert!(
            !source.contains(&forward_example_path) && !source.contains(&backward_example_path),
            "{} depends on an example instead of a dedicated fixture",
            entry.path().display()
        );
    }
}
