use std::path::PathBuf;
use std::process::Command;
use std::{fs, time};

fn foster() -> Command {
    Command::new(env!("CARGO_BIN_EXE_foster"))
}

fn benchmark_source() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("benchmarks/fibonacci.fos")
}

fn arguments_source() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/arguments.fos")
}

fn temporary_directory(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "foster-cli-{label}-{}-{}",
        std::process::id(),
        time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

#[test]
fn help_describes_commands_and_important_options() {
    let top_level = foster().arg("--help").output().unwrap();
    assert!(top_level.status.success());
    let top_level = String::from_utf8(top_level.stdout).unwrap();
    for description in [
        "Compile and run a Foster program, bytecode file, or package",
        "Compile Foster source to bytecode or a native executable",
        "Type-check and validate Foster source without running it",
        "Compile and run Foster test declarations",
        "Generate static API documentation for Foster source",
        "Serve an existing generated documentation directory",
        "Start the Foster language server over standard input/output",
    ] {
        assert!(top_level.contains(description), "{top_level}");
    }

    let run = foster().args(["run", "--help"]).output().unwrap();
    assert!(run.status.success());
    let run = String::from_utf8(run.stdout).unwrap();
    assert!(
        run.contains("Enable bytecode optimization (the default)"),
        "{run}"
    );
    assert!(run.contains("Disable bytecode optimization"), "{run}");

    let docs = foster().args(["docs", "--help"]).output().unwrap();
    assert!(docs.status.success());
    let docs = String::from_utf8(docs.stdout).unwrap();
    assert!(
        docs.contains("Write generated documentation to this directory"),
        "{docs}"
    );
    assert!(
        docs.contains("Serve the generated documentation after building it"),
        "{docs}"
    );
}

#[test]
fn init_creates_a_project_that_commands_discover_from_nested_directories() {
    let root = temporary_directory("init");
    let init = foster()
        .arg("init")
        .arg(&root)
        .arg("--name")
        .arg("sample-app")
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    assert_eq!(
        fs::read_to_string(root.join("foster.toml")).unwrap(),
        "[package]\nname = \"sample-app\"\nsource = \"src\"\n"
    );
    assert_eq!(
        fs::read_to_string(root.join("src/main.fos")).unwrap(),
        "func main() -> () {\n    println(\"Hello, Foster!\")\n}\n"
    );
    let nested = root.join("src/nested");
    fs::create_dir(&nested).unwrap();

    let run = foster().arg("run").current_dir(&nested).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8(run.stdout).unwrap().trim(),
        "Hello, Foster!"
    );

    let check = foster().arg("check").arg(&root).output().unwrap();
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert_eq!(
        String::from_utf8(check.stdout).unwrap().trim(),
        "ok: checked 2 modules (1 implicit)"
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn projects_compile_and_run_transitive_path_dependencies() {
    let root = temporary_directory("path-dependencies");
    let app = root.join("app");
    let middle = root.join("middle");
    let leaf = root.join("leaf");
    for project in [&app, &middle, &leaf] {
        fs::create_dir_all(project.join("src")).unwrap();
    }
    fs::write(
        app.join("foster.toml"),
        "[package]\nname = \"app\"\nsource = \"src\"\n[dependencies]\nmiddle = { path = \"../middle\" }\n",
    )
    .unwrap();
    fs::write(
        app.join("src/main.fos"),
        "import middle\nfunc main() -> Int { answer() }\n",
    )
    .unwrap();
    fs::write(
        middle.join("foster.toml"),
        "[package]\nname = \"middle-package\"\nsource = \"src\"\n[dependencies]\nleaf = { path = \"../leaf\" }\n",
    )
    .unwrap();
    fs::write(
        middle.join("src/main.fos"),
        "import helper\nimport leaf\npub func answer() -> Int { base() + increment() }\n",
    )
    .unwrap();
    fs::write(
        middle.join("src/helper.fos"),
        "pub func increment() -> Int { 2 }\n",
    )
    .unwrap();
    fs::write(
        leaf.join("foster.toml"),
        "[package]\nname = \"leaf-package\"\nsource = \"src\"\n",
    )
    .unwrap();
    fs::write(leaf.join("src/main.fos"), "pub func base() -> Int { 40 }\n").unwrap();

    let run = foster().arg("run").arg(&app).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8(run.stdout).unwrap().trim(), "42");

    let check = foster().arg("check").arg(&app).output().unwrap();
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert_eq!(
        String::from_utf8(check.stdout).unwrap().trim(),
        "ok: checked 1 module (0 implicit)"
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn project_dependencies_do_not_silently_replace_application_modules() {
    let root = temporary_directory("dependency-collision");
    let app = root.join("app");
    let dependency = root.join("dependency");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::create_dir_all(dependency.join("src")).unwrap();
    fs::write(
        app.join("foster.toml"),
        "[package]\nname = \"app\"\nsource = \"src\"\n[dependencies]\nshared = { path = \"../dependency\" }\n",
    )
    .unwrap();
    fs::write(app.join("src/main.fos"), "func main() -> Int { 0 }\n").unwrap();
    fs::write(
        app.join("src/shared.fos"),
        "pub func application_value() -> Int { 1 }\n",
    )
    .unwrap();
    fs::write(
        dependency.join("foster.toml"),
        "[package]\nname = \"dependency\"\nsource = \"src\"\n",
    )
    .unwrap();
    fs::write(
        dependency.join("src/main.fos"),
        "pub func dependency_value() -> Int { 2 }\n",
    )
    .unwrap();

    let check = foster().arg("check").arg(&app).output().unwrap();
    assert!(!check.status.success());
    let stderr = String::from_utf8(check.stderr).unwrap();
    assert!(
        stderr.contains("module `shared` has two source files"),
        "{stderr}"
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn project_errors_render_their_source_locations() {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/unknown_type");
    let output = foster().arg("check").arg(fixture).output().unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("main.fos:1:21"), "{stderr}");
    assert!(stderr.contains("invalid type annotation"), "{stderr}");

    let malformed = temporary_directory("project-parse-error");
    fs::create_dir_all(&malformed).unwrap();
    fs::write(malformed.join("main.fos"), "func main() {\n    @\n}\n").unwrap();
    let output = foster().arg("check").arg(&malformed).output().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("main.fos:2:5"), "{stderr}");
    fs::remove_dir_all(malformed).unwrap();
}

#[test]
fn project_manifest_errors_are_actionable() {
    let malformed = temporary_directory("malformed-manifest");
    fs::create_dir_all(malformed.join("src")).unwrap();
    fs::write(
        malformed.join("foster.toml"),
        "[package\nname = \"broken\"\n",
    )
    .unwrap();
    let output = foster().arg("check").arg(&malformed).output().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("invalid project manifest"), "{stderr}");
    assert!(stderr.contains("foster.toml"), "{stderr}");
    fs::remove_dir_all(malformed).unwrap();

    let missing_source = temporary_directory("missing-source");
    fs::create_dir_all(&missing_source).unwrap();
    fs::write(
        missing_source.join("foster.toml"),
        "[package]\nname = \"broken\"\nsource = \"missing\"\n",
    )
    .unwrap();
    let output = foster().arg("check").arg(&missing_source).output().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("project source root"), "{stderr}");
    assert!(stderr.contains("is not a directory"), "{stderr}");
    fs::remove_dir_all(missing_source).unwrap();
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
    assert!(output.contains("foster-language=7 ownership-model=1"));
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
fn build_native_writes_runnable_host_executable() {
    let mut output_path = std::env::temp_dir().join(format!(
        "foster-native-{}-{}",
        std::process::id(),
        time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    if cfg!(windows) {
        output_path.set_extension("exe");
    }
    let build = foster()
        .arg("build")
        .arg(benchmark_source())
        .arg("--native")
        .arg("--output")
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let run = Command::new(&output_path).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8(run.stdout).unwrap().trim(), "6765");
    fs::remove_file(output_path).unwrap();
}

#[test]
fn native_assertions_exit_with_their_message() {
    let directory = temporary_directory("native-assertion");
    fs::create_dir_all(&directory).unwrap();
    let source = directory.join("main.fos");
    fs::write(
        &source,
        "func main() -> () { assert(false, \"native assertion message\") }\n",
    )
    .unwrap();
    let mut executable = directory.join("assertion");
    if cfg!(windows) {
        executable.set_extension("exe");
    }

    let build = foster()
        .args(["build", "--native"])
        .arg(&source)
        .arg("--output")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let run = Command::new(&executable).output().unwrap();
    assert!(!run.status.success());
    assert!(
        String::from_utf8_lossy(&run.stderr).contains("assertion failed: native assertion message")
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn command_arguments_flow_through_source_bytecode_and_native_execution() {
    let source_run = foster()
        .arg("run")
        .arg(arguments_source())
        .arg("--")
        .arg("--about")
        .output()
        .unwrap();
    assert!(
        source_run.status.success(),
        "{}",
        String::from_utf8_lossy(&source_run.stderr)
    );
    assert_eq!(
        String::from_utf8(source_run.stdout).unwrap().trim(),
        "Foster command arguments"
    );

    let unique = format!(
        "foster-arguments-{}-{}",
        std::process::id(),
        time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let bytecode = std::env::temp_dir().join(format!("{unique}.fbc"));
    let mut native = std::env::temp_dir().join(unique);
    if cfg!(windows) {
        native.set_extension("exe");
    }

    let bytecode_build = foster()
        .args(["build"])
        .arg(arguments_source())
        .arg("-o")
        .arg(&bytecode)
        .output()
        .unwrap();
    assert!(bytecode_build.status.success());
    let bytecode_run = foster()
        .arg("run")
        .arg(&bytecode)
        .arg("--")
        .arg("bytecode-value")
        .output()
        .unwrap();
    assert!(bytecode_run.status.success());
    assert_eq!(
        String::from_utf8(bytecode_run.stdout).unwrap().trim(),
        "bytecode-value"
    );

    let native_build = foster()
        .arg("build")
        .arg(arguments_source())
        .arg("--native")
        .arg("-o")
        .arg(&native)
        .output()
        .unwrap();
    assert!(
        native_build.status.success(),
        "{}",
        String::from_utf8_lossy(&native_build.stderr)
    );
    let native_run = Command::new(&native).arg("native-value").output().unwrap();
    assert!(native_run.status.success());
    assert_eq!(
        String::from_utf8(native_run.stdout).unwrap().trim(),
        "native-value"
    );

    fs::remove_file(bytecode).unwrap();
    fs::remove_file(native).unwrap();
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
    assert!(index.contains("<span>1 module</span>"));
    assert!(index.contains("data-module-filter"));
    assert!(index.contains("declarations</span>"));
    assert!(!index.contains("data-module=\"core"));
    assert!(module.contains("func fibonacci"));
    assert!(module.contains("aria-label=\"On this page\""));
    assert!(module.contains("class=\"badge kind\">function"));
    assert!(module.contains("class=\"anchor\" href=\"#fibonacci\""));
    assert!(output_directory.join("style.css").is_file());
    assert!(!output_directory.join("modules/core.html").exists());

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
        "test \"fails\" {\n    assert(false, \"the value was not ready\")\n}\ntest \"still runs\" { assert(true) }\n",
    )
    .unwrap();

    let output = foster().arg("test").arg(&source_path).output().unwrap();
    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stdout.contains("test fails ... FAILED"), "{stdout}");
    assert!(stdout.contains("test still runs ... ok"), "{stdout}");
    assert!(
        stderr.contains("assertion failed: the value was not ready"),
        "{stderr}"
    );
    assert!(stderr.contains("test result: FAILED"), "{stderr}");

    fs::remove_dir_all(directory).unwrap();
}
