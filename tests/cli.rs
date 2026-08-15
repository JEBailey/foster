use std::path::PathBuf;
use std::process::Command;

fn foster() -> Command {
    Command::new(env!("CARGO_BIN_EXE_foster"))
}

fn benchmark_source() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("benchmarks/fibonacci.foster")
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
            .contains("unknown run flag `--turbo`")
    );
}
