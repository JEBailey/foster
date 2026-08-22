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
    assert!(module.contains("func fibonacci"));
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
