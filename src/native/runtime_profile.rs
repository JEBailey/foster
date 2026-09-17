//! Opt-in execution profiling; instrumentation is never linked into ordinary builds.
use super::*;

fn replace_once(source: String, needle: &str, replacement: &str) -> String {
    assert_eq!(
        source.matches(needle).count(),
        1,
        "profiling hook changed: {needle}"
    );
    source.replacen(needle, replacement, 1)
}

fn instrument(mut source: String, census: bool) -> String {
    if census {
        source = replace_once(
            source,
            "extern \"C\" fn foster_rt_v4_cancellation_point() -> u8 {",
            "extern \"C\" fn foster_rt_v4_cancellation_point() -> u8 { PROFILE_POLLS.fetch_add(1, PROFILE_ORDER);",
        );
        source = replace_once(
            source,
            "let pointer = unsafe { alloc_zeroed(layout) };",
            "let pointer = unsafe { alloc_zeroed(layout) }; PROFILE_ALLOCS.fetch_add(1, PROFILE_ORDER); PROFILE_BYTES.fetch_add(size as u64, PROFILE_ORDER);",
        );
        source = replace_once(
            source,
            "unsafe { dealloc(pointer as *mut u8, layout) };",
            "PROFILE_FREES.fetch_add(1, PROFILE_ORDER); unsafe { dealloc(pointer as *mut u8, layout) };",
        );
        source = replace_once(
            source,
            "unsafe { std::ptr::copy_nonoverlapping(source as *const u8, destination as *mut u8, length) };",
            "PROFILE_COPIES.fetch_add(1, PROFILE_ORDER); PROFILE_COPY_BYTES.fetch_add(length as u64, PROFILE_ORDER); unsafe { std::ptr::copy_nonoverlapping(source as *const u8, destination as *mut u8, length) };",
        );
        source.push_str(r#"
const PROFILE_ORDER: std::sync::atomic::Ordering = std::sync::atomic::Ordering::Relaxed;
static PROFILE_ALLOCS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PROFILE_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PROFILE_FREES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PROFILE_COPIES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PROFILE_COPY_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PROFILE_POLLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
fn profile_reset() {
    for counter in [&PROFILE_ALLOCS, &PROFILE_BYTES, &PROFILE_FREES, &PROFILE_COPIES, &PROFILE_COPY_BYTES, &PROFILE_POLLS] {
        counter.store(0, PROFILE_ORDER);
    }
}
fn profile_report() {
    eprintln!("PROFILE {{\"allocations\":{},\"allocated_bytes\":{},\"deallocations\":{},\"buffer_copy_calls\":{},\"buffer_copy_bytes\":{},\"cancellation_polls\":{}}}",
        PROFILE_ALLOCS.load(PROFILE_ORDER), PROFILE_BYTES.load(PROFILE_ORDER), PROFILE_FREES.load(PROFILE_ORDER),
        PROFILE_COPIES.load(PROFILE_ORDER), PROFILE_COPY_BYTES.load(PROFILE_ORDER), PROFILE_POLLS.load(PROFILE_ORDER));
}
"#);
    }
    let invocation = "let value = unsafe { foster_native_entry() };";
    replace_once(
        source,
        invocation,
        if census {
            "profile_reset(); let value = unsafe { foster_native_entry() }; profile_report();"
        } else {
            "let profile_start = std::time::Instant::now(); let value = unsafe { foster_native_entry() }; let profile_ns = profile_start.elapsed().as_nanos(); eprintln!(\"PROFILE {{\\\"elapsed_ns\\\":{profile_ns}}}\");"
        },
    )
}

fn clone_artifact(artifact: &ObjectArtifact) -> ObjectArtifact {
    ObjectArtifact {
        bytes: artifact.bytes.clone(),
        result: artifact.result,
        accepts_arguments: artifact.accepts_arguments,
        runtime_strings: artifact.runtime_strings.clone(),
        releases_result: artifact.releases_result,
    }
}

#[test]
#[ignore = "builds native workload executables and measures runtime; run alone in release mode"]
fn profile_native_runtime() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let output = root.join("target/native-runtime-profile");
    fs::create_dir_all(&output).unwrap();
    let workloads = [
        ("scalar", "4999950000"),
        ("record_fresh", "5000050000"),
        ("record_reuse", "5000050000"),
        ("field_read", "1000000"),
        ("method_read", "1000000"),
        ("list_fresh", "4999950000"),
        ("list_push", "100001"),
        ("list_iterator", "4999950000"),
        ("list_snapshot", "4999950000"),
    ];
    let options = CompileOptions { optimize: true };
    let mut built = Vec::new();
    for (name, expected) in workloads {
        eprintln!("building {name}");
        let source =
            fs::read_to_string(root.join(format!("benchmarks/native_runtime/{name}.fos"))).unwrap();
        let compilation = crate::compile(&source).unwrap_or_else(|e| panic!("{name}: {e}"));
        // A small VM run independently checks the workload's semantics.
        let small = source.replace("100000", "10");
        let checked = crate::compile(&small).unwrap();
        let small_result = crate::vm::run(&checked).unwrap();
        let small_expected = match name {
            "record_fresh" | "record_reuse" => "55",
            "field_read" | "method_read" => "100",
            "list_push" => "11",
            _ => "45",
        };
        assert_eq!(small_result.to_string(), small_expected, "VM {name}");
        let prepared = prepare_with_options(&compilation, options).unwrap();
        fs::write(output.join(format!("{name}.ir")), prepared.emit_ir()).unwrap();
        let artifact = prepared.compile_object(options).unwrap();
        let mut executables = Vec::new();
        for mode in 0..3 {
            let census = mode == 1;
            let mut shim = instrument(
                entry_source(
                    artifact.result,
                    artifact.accepts_arguments,
                    &artifact.runtime_strings,
                    artifact.releases_result,
                ),
                census,
            );
            if mode == 2 {
                // Diagnostic ablation for these successful, single-threaded
                // workloads only. Never a supported runtime/optimization mode.
                shim = replace_once(
                    shim,
                    "extern \"C\" fn foster_rt_v4_cancellation_point() -> u8 {",
                    "extern \"C\" fn foster_profile_original_cancellation_point() -> u8 {",
                );
                shim.push_str("\n#[unsafe(no_mangle)] extern \"C\" fn foster_rt_v4_cancellation_point() -> u8 { 0 }\n");
            }
            let executable =
                output.join(format!("{name}-mode{mode}{}", std::env::consts::EXE_SUFFIX));
            link_source(clone_artifact(&artifact), &executable, options, &shim, None).unwrap();
            executables.push(executable);
        }
        built.push((name, expected, executables));
    }
    let mut reports = Vec::new();
    for (name, expected, executables) in built {
        let mut timings = Vec::new();
        let mut bypass_timings = Vec::new();
        let mut census = serde_json::Value::Null;
        for (mode, executable) in executables.iter().enumerate() {
            let samples = if mode == 1 { 1 } else { 8 };
            for sample in 0..samples {
                let run = Command::new(executable).output().unwrap();
                let stderr = String::from_utf8_lossy(&run.stderr);
                assert!(run.status.success(), "{name}: {stderr}");
                assert_eq!(
                    String::from_utf8_lossy(&run.stdout).trim(),
                    expected,
                    "{name}"
                );
                let record = stderr
                    .lines()
                    .find_map(|line| line.strip_prefix("PROFILE "))
                    .expect("profile record");
                let record: serde_json::Value = serde_json::from_str(record).unwrap();
                if mode == 0 && sample > 0 {
                    timings.push(record["elapsed_ns"].as_u64().unwrap());
                }
                if mode == 1 {
                    census = record;
                } else if mode == 2 && sample > 0 {
                    bypass_timings.push(record["elapsed_ns"].as_u64().unwrap());
                }
            }
        }
        let mut sorted = timings.clone();
        sorted.sort_unstable();
        let mut bypass_sorted = bypass_timings.clone();
        bypass_sorted.sort_unstable();
        let report = serde_json::json!({"workload":name,"iterations":100000,"result":expected,
            "median_ns":sorted[sorted.len()/2],"samples_ns":timings,"census":census,
            "diagnostic_poll_body_bypass_median_ns":bypass_sorted[bypass_sorted.len()/2],
            "diagnostic_poll_body_bypass_samples_ns":bypass_timings});
        eprintln!("{report}");
        reports.push(report);
    }
    fs::write(
        output.join("results.json"),
        serde_json::to_string_pretty(&reports).unwrap(),
    )
    .unwrap();
}
