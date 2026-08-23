use std::path::PathBuf;
use std::process::Command;
use std::{fs, time};

fn foster() -> Command {
    Command::new(env!("CARGO_BIN_EXE_foster"))
}

fn benchmark_source() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("benchmarks/fibonacci.fos")
}

#[test]
fn run_accepts_explicit_optimizer_settings() {
    let optimized = foster()
        .arg("run")
        .arg(benchmark_source())
        .arg("--optimize")
        .output()
        .unwrap();
    let unoptimized = foster()
        .arg("run")
        .arg(benchmark_source())
        .arg("--no-optimize")
        .output()
        .unwrap();

    assert!(optimized.status.success());
    assert!(unoptimized.status.success());
    assert_eq!(optimized.stdout, unoptimized.stdout);
    assert_eq!(String::from_utf8(optimized.stdout).unwrap().trim(), "6765");
}

#[test]
fn run_rejects_unknown_optimizer_settings() {
    let output = foster()
        .arg("run")
        .arg(benchmark_source())
        .arg("--turbo")
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("unexpected argument '--turbo'")
    );
}

#[test]
fn check_can_dump_deterministic_ownership_state() {
    let first = foster()
        .arg("check")
        .arg(benchmark_source())
        .arg("--dump-ownership")
        .output()
        .unwrap();
    let second = foster()
        .arg("check")
        .arg(benchmark_source())
        .arg("--dump-ownership")
        .output()
        .unwrap();
    assert!(first.status.success());
    assert_eq!(first.stdout, second.stdout);
    let output = String::from_utf8(first.stdout).unwrap();
    assert!(output.contains("foster-language=1 ownership-model=1"));
    assert!(output.contains("function main.main"));
}

#[test]
fn build_writes_runnable_compiled_bytecode() {
    let output_path = std::env::temp_dir().join(format!(
        "foster-bytecode-{}-{}.fbc",
        std::process::id(),
        time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let build = foster()
        .arg("build")
        .arg(benchmark_source())
        .arg("--output")
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    assert_eq!(&fs::read(&output_path).unwrap()[..8], b"FOSTERBC");

    let run = foster().arg("run").arg(&output_path).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8(run.stdout).unwrap().trim(), "6765");
    fs::remove_file(output_path).unwrap();
}

#[test]
fn pack_writes_deterministic_runnable_archive_with_resources() {
    let directory = std::env::temp_dir().join(format!(
        "foster-pack-{}-{}",
        std::process::id(),
        time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(directory.join("resources/config")).unwrap();
    fs::write(
        directory.join("main.fos"),
        r#"import core.result
import std.fs

func main() -> String {
    branch read_text("resources/config/message.txt") {
        Result.Ok(text) -> text
        Result.Error(_) -> "missing resource"
    }
}
"#,
    )
    .unwrap();
    fs::write(
        directory.join("resources/config/message.txt"),
        "hello from package",
    )
    .unwrap();
    let first = directory.with_extension("first.fpk");
    let second = directory.with_extension("second.fpk");

    for output in [&first, &second] {
        let pack = foster()
            .arg("pack")
            .arg(&directory)
            .arg("--output")
            .arg(output)
            .output()
            .unwrap();
        assert!(
            pack.status.success(),
            "{}",
            String::from_utf8_lossy(&pack.stderr)
        );
    }
    assert_eq!(fs::read(&first).unwrap(), fs::read(&second).unwrap());

    let run = foster().arg("run").arg(&first).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8(run.stdout).unwrap().trim(),
        "hello from package"
    );

    fs::remove_dir_all(directory).unwrap();
    fs::remove_file(first).unwrap();
    fs::remove_file(second).unwrap();
}

#[test]
fn docs_generates_a_static_site_from_resolved_declarations() {
    let unique = format!(
        "foster-docs-{}-{}",
        std::process::id(),
        time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let output_directory = std::env::temp_dir().join(unique);
    let output = foster()
        .arg("docs")
        .arg(benchmark_source())
        .arg("--output")
        .arg(&output_directory)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let index = fs::read_to_string(output_directory.join("index.html")).unwrap();
    let module = fs::read_to_string(output_directory.join("modules/main.html")).unwrap();
    assert!(index.contains("Foster documentation"));
    assert!(index.contains("data-module-filter"));
    assert!(index.contains("declarations</span>"));
    assert!(module.contains("func fibonacci"));
    assert!(module.contains("aria-label=\"On this page\""));
    assert!(module.contains("class=\"badge kind\">function"));
    assert!(module.contains("class=\"anchor\" href=\"#fibonacci\""));
    assert!(output_directory.join("style.css").is_file());

    fs::remove_dir_all(output_directory).unwrap();
}

#[test]
fn docs_rejects_server_only_options_without_serve() {
    let output = foster().args(["docs", "--no-open"]).output().unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("required arguments were not provided")
    );
}

#[test]
fn fmt_formats_files_and_supports_check_mode() {
    let directory = std::env::temp_dir().join(format!(
        "foster-fmt-{}-{}",
        std::process::id(),
        time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&directory).unwrap();
    let source_path = directory.join("main.fos");
    fs::write(
        &source_path,
        "// preserve {\r\nfunc main() -> Int {  \r\nvalue = 42\r\nvalue\r\n}\r\n",
    )
    .unwrap();

    let check = foster()
        .arg("fmt")
        .arg(&directory)
        .arg("--check")
        .output()
        .unwrap();
    assert!(!check.status.success());
    assert!(String::from_utf8_lossy(&check.stderr).contains("needs formatting"));

    let format = foster().arg("fmt").arg(&directory).output().unwrap();
    assert!(format.status.success());
    assert_eq!(
        fs::read_to_string(&source_path).unwrap(),
        "// preserve {\nfunc main() -> Int {\n    value = 42\n    value\n}\n"
    );

    let check = foster()
        .arg("fmt")
        .arg(&directory)
        .arg("--check")
        .output()
        .unwrap();
    assert!(check.status.success());
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn test_discovers_and_runs_foster_test_declarations() {
    let directory = std::env::temp_dir().join(format!(
        "foster-test-{}-{}",
        std::process::id(),
        time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&directory).unwrap();
    let source_path = directory.join("main.fos");
    fs::write(
        &source_path,
        "test \"second\" { println() }\ntest \"first\" {}\n",
    )
    .unwrap();

    let output = foster()
        .arg("test")
        .arg(&source_path)
        .arg("--no-optimize")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("running 2 test(s)"), "{stdout}");
    assert!(stdout.contains("test first ... ok"), "{stdout}");
    assert!(stdout.contains("test second ... ok"), "{stdout}");
    assert!(stdout.contains("test result: ok"), "{stdout}");

    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn test_reports_runtime_failures_and_continues() {
    let directory = std::env::temp_dir().join(format!(
        "foster-test-failure-{}-{}",
        std::process::id(),
        time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&directory).unwrap();
    let source_path = directory.join("main.fos");
    fs::write(
        &source_path,
        "test \"fails\" {\n    let value = [1][4]\n    println(value)\n}\ntest \"still runs\" {}\n",
    )
    .unwrap();

    let output = foster().arg("test").arg(&source_path).output().unwrap();
    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stdout.contains("test fails ... FAILED"), "{stdout}");
    assert!(stdout.contains("test still runs ... ok"), "{stdout}");
    assert!(stderr.contains("index is out of bounds"), "{stderr}");
    assert!(stderr.contains("test result: FAILED"), "{stderr}");

    fs::remove_dir_all(directory).unwrap();
}
