//! Assemble a reusable platform runtime and a small, program-specific entry shim.
use super::*;

fn main_source(
    result: NativeType,
    accepts_arguments: bool,
    runtime_strings: &[String],
    releases_result: bool,
) -> String {
    let print = match result {
        NativeType::Unit => String::new(),
        NativeType::Bool => "foster_rt_v4_write_bool(value); foster_rt_v4_write_newline();".into(),
        NativeType::Int => "foster_rt_v4_write_int(value); foster_rt_v4_write_newline();".into(),
        NativeType::Float => {
            "foster_rt_v4_write_float(value); foster_rt_v4_write_newline();".into()
        }
        NativeType::CodePoint => {
            "foster_rt_v4_write_code_point(value); foster_rt_v4_write_newline();".into()
        }
        NativeType::Byte => "foster_rt_v4_write_byte(value); foster_rt_v4_write_newline();".into(),
        NativeType::String => {
            "foster_rt_v4_write_string(value); foster_rt_v4_write_newline();".into()
        }
        NativeType::Opaque | NativeType::Object(_) => {
            "foster_rt_v4_write_object(value); foster_rt_v4_write_newline();".into()
        }
    };
    let constants = runtime_strings
        .iter()
        .map(|value| format!("{value:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    let result_type = match result {
        NativeType::Unit | NativeType::Bool | NativeType::Byte => "u8",
        NativeType::CodePoint => "u32",
        NativeType::Int => "i64",
        NativeType::String => "usize",
        NativeType::Float => "f64",
        NativeType::Opaque | NativeType::Object(_) => "usize",
    };
    let declaration = if accepts_arguments {
        format!(
            "unsafe extern \"C\" {{ fn foster_native_entry(arguments: usize) -> {result_type}; }}"
        )
    } else {
        format!("unsafe extern \"C\" {{ fn foster_native_entry() -> {result_type}; }}")
    };
    let invocation = if accepts_arguments {
        "unsafe extern \"C\" { fn foster_native_arguments(executable: usize, values: usize, length: i64) -> usize; }\n    let mut supplied = std::env::args_os();\n    let executable = owned_string(&supplied.next().map(unicode_argument).unwrap_or_default());\n    let values: Vec<usize> = supplied.map(unicode_argument).map(|text| owned_string(&text)).collect();\n    let arguments = unsafe { foster_native_arguments(executable, values.as_ptr() as usize, values.len() as i64) };\n    let value = unsafe { foster_native_entry(arguments) };"
    } else {
        "let value = unsafe { foster_native_entry() };"
    };
    let release_declaration = if releases_result {
        "unsafe extern \"C\" { fn foster_native_release_result(value: usize) -> u8; }"
    } else {
        ""
    };
    let release = if releases_result {
        "unsafe { foster_native_release_result(value); }"
    } else {
        ""
    };
    format!(
        r#"{declaration}
{release_declaration}

fn main() {{
    foster_runtime_initialize(&[{constants}]);
    {invocation}
    foster_runtime_check_execution();
    {print}
    {release}
    foster_runtime_check_execution();
}}
"#
    )
}

fn runtime_source() -> String {
    let source = include_str!("../../runtime/src/lib.rs")
        .replace("\r\n", "\n")
        .replace(
            "#[path = \"../../src/remote.rs\"]\nmod remote_lifecycle;",
            &format!(
                "mod remote_lifecycle {{\n{}\n}}",
                include_str!("../remote.rs")
            ),
        )
        .replace(
            "pub use foster_host as services;",
            &format!(
                "pub mod services {{\n{}\n}}",
                include_str!("../../host/src/lib.rs")
            ),
        )
        .replace(
            "include!(\"host.rs\");",
            include_str!("../../runtime/src/host.rs"),
        )
        .replace(
            "include!(\"equality.rs\");",
            include_str!("../../runtime/src/equality.rs"),
        );
    let source = source.replace(
        "include!(\"version.rs\");",
        include_str!("../../runtime/src/version.rs"),
    );
    source + &abi::runtime_assertions()
}

// Allocation-census tests instrument a complete runtime, independently of the production cache.
#[cfg(test)]
fn entry_source(
    result: NativeType,
    accepts_arguments: bool,
    runtime_strings: &[String],
    releases_result: bool,
) -> String {
    runtime_source() + &main_source(result, accepts_arguments, runtime_strings, releases_result)
}

pub(super) fn link_executable(
    artifact: ObjectArtifact,
    output: &Path,
    options: CompileOptions,
) -> Result<(), FosterError> {
    let source = "use foster_native_runtime::*;\n".to_owned()
        + &main_source(
            artifact.result,
            artifact.accepts_arguments,
            &artifact.runtime_strings,
            artifact.releases_result,
        );
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let library = runtime_cache::library(&rustc, &runtime_source(), options)?;
    link_source(artifact, output, options, &source, Some(&library))
}

fn link_source(
    artifact: ObjectArtifact,
    output: &Path,
    options: CompileOptions,
    source: &str,
    library: Option<&Path>,
) -> Result<(), FosterError> {
    let output = absolute_path(output)?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            native_error(format!(
                "cannot create output directory `{}`: {error}",
                parent.display()
            ))
        })?;
    }
    let temporary = TemporaryDirectory::create()?;
    let object = temporary.path.join(if cfg!(windows) {
        "program.obj"
    } else {
        "program.o"
    });
    let shim = temporary.path.join("entry.rs");
    fs::write(&object, artifact.bytes)
        .map_err(|error| native_error(format!("cannot write `{}`: {error}", object.display())))?;
    fs::write(&shim, source)
        .map_err(|error| native_error(format!("cannot write linker shim: {error}")))?;

    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let mut command = Command::new(&rustc);
    let dependency_library;
    let dependencies = if let Some(library) = library {
        library
    } else {
        dependency_library = runtime_cache::library(&rustc, &runtime_source(), options)?;
        &dependency_library
    };
    runtime_cache::link_dependencies(&mut command, dependencies)?;
    if let Some(library) = library {
        command
            .arg("--extern")
            .arg(format!("foster_native_runtime={}", library.display()));
    }
    let result = command
        .arg("--edition=2024")
        .arg(&shim)
        .arg("-C")
        .arg(if options.optimize {
            "opt-level=2"
        } else {
            "opt-level=0"
        })
        .arg("-C")
        .arg(format!("link-arg={}", object.display()))
        .arg("-o")
        .arg(&output)
        .output()
        .map_err(|error| {
            native_error(format!(
                "cannot run `{}` to link the executable: {error}",
                Path::new(&rustc).display()
            ))
        })?;
    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        return Err(native_error(format!(
            "native linker failed with {}{}{}",
            result.status,
            if stderr.trim().is_empty() { "" } else { ": " },
            stderr.trim()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track_allocations(source: String) -> String {
        let source = source
            .replace("fn main() {", r#"
static LIVE: std::sync::Mutex<std::collections::BTreeMap<usize, (i64, i64)>> = std::sync::Mutex::new(std::collections::BTreeMap::new());
static HOST_RESPONSES: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
static LIVE_WORKERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
struct WorkerCensus;
impl Drop for WorkerCensus {
    fn drop(&mut self) { LIVE_WORKERS.fetch_sub(1, std::sync::atomic::Ordering::SeqCst); }
}
fn check_reclamation() {
    // Test instrumentation waits for actual worker completion: owner cancellation
    // deliberately publishes futures before physical storage reclamation finishes.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while LIVE_WORKERS.load(std::sync::atomic::Ordering::SeqCst) != 0 {
        assert!(std::time::Instant::now() < deadline, "cancelled workers did not finish cleanup");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let live = LIVE.lock().unwrap();
    assert!(live.is_empty(), "native allocations leaked: {:?}", *live);
    assert_eq!(HOST_RESPONSES.load(std::sync::atomic::Ordering::SeqCst), 0, "host responses leaked");
}
fn main() {"#)
            .replace("let worker = may::go_with!(1024 * 1024, move || {", "LIVE_WORKERS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);\n    let worker = may::go_with!(1024 * 1024, move || {\n        let _census = WorkerCensus;")
            .replace("let pointer = unsafe { alloc_zeroed(layout) };", "let pointer = unsafe { alloc_zeroed(layout) };\n    assert!(LIVE.lock().unwrap().insert(pointer as usize, (size, align)).is_none());")
            .replace("unsafe { dealloc(pointer as *mut u8, layout) };", "assert_eq!(LIVE.lock().unwrap().remove(&pointer), Some((size, align)), \"allocation layout mismatch\");\n    unsafe { dealloc(pointer as *mut u8, layout) };")
            .replace("Box::into_raw(Box::new(response)) as usize", "{ HOST_RESPONSES.fetch_add(1, std::sync::atomic::Ordering::SeqCst); Box::into_raw(Box::new(response)) as usize }")
            .replace("unsafe { drop(Box::from_raw(response as *mut FosterHostResponse)) };", "HOST_RESPONSES.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);\n    unsafe { drop(Box::from_raw(response as *mut FosterHostResponse)) };")
            .replace("unsafe { foster_native_release_result(value); }", "unsafe { foster_native_release_result(value); }\n    check_reclamation();")
            .replace("std::process::exit(2);", "check_reclamation(); std::process::exit(2);");
        for injection in [
            "insert(pointer",
            "allocation layout mismatch",
            "fetch_add(1",
            "fetch_sub(1",
            "check_reclamation();",
        ] {
            assert!(
                source.contains(injection),
                "missing test instrumentation: {injection}"
            );
        }
        source
    }

    #[test]
    fn failures_release_native_frames_and_transferred_arguments() {
        let compilation = crate::compile(
            r#"
import core.string
import core.result
import core.list
import std.process
import std.path
import std.iter
type Box = { text: String }
impl Box { func copy(self) -> self { Box { text: self.text.copy() } } }
func crash(kind: String, text: String) -> String {
    let keep = [Box { text: text + "nested" }]
    branch kind {
        "bounds" -> keep[9].text
        "arithmetic" -> keep[text.length / 0].text
        "host" -> path::join(text, "child")
        _ -> {
            assert(false, text)
            keep[0].text
        }
    }
}
func reference[g: group Box](box: ref[g] Box, kind: String) -> String {
    crash(kind, box.text)
}
func nested[g: group List<Box>](items: ref[g] List<Box>, text: String) -> String [mut g] {
    items[0].text = text + "replacement"
    crash("assert", text)
}
func combine(box: Box, text: String) -> String { box.text + text }
func boxed(box: Box) -> Result<Box, String> { Result.Ok(box) }
func invoke(action: func() -> String) -> String { action() }
func discard(text: String, box: Box) -> String [consume box] { "discarded" }
func recursive(text: String, depth: Int) -> String {
    let held = Box { text: text + "recursive" }
    return crash("assert", held.text) if depth == 0
    recursive(held.text, depth - 1)
}
func fail(kind: String, borrowed: String, owned: Box) -> String [consume owned, suspend] {
    let local = [owned, Box { text: borrowed + "local" }]
    branch kind {
        "closure" -> {
            let captured = borrowed + "capture"
            let action = [move captured] () -> crash("assert", captured)
            invoke(action)
        }
        "refclosure" -> {
            let held = Box { text: borrowed + "reference capture" }
            let action = [ref held] () -> crash("assert", held.text)
            action()
            "unreachable"
        }
        "update" -> {
            let saved = local[0].copy()
            local[0] = Box { text: borrowed + "changed" }
            assert(saved.text != local[0].text)
            crash(kind, local[0].text)
        }
        "nested-reference" -> {
            let snapshot = local.iterator()
            let outcome_text = nested(ref local, borrowed)
            snapshot.next()
            outcome_text
        }
        "reference" -> reference(ref (Box { text: borrowed + "temporary" }), kind)
        "pending" -> {
            let peer = remote Worker { text: borrowed + "peer" }
            let pending = peer.echo(borrowed + "pending result")
            let value = crash(kind, borrowed)
            (await pending).unwrap_or(move value)
        }
        "partial" -> combine(Box { text: borrowed + "first argument" }, crash(kind, borrowed))
        "recursive" -> recursive(borrowed, 4)
        "match" -> {
            let outcome = boxed(local[0].copy())
            branch outcome {
                Result.Ok(box) -> crash(kind, box.text)
                Result.Error(message) -> message
            }
        }
        _ -> crash(kind, local[1].text)
    }
}
type Worker = { text: String }
impl Worker {
    func fail(self, kind: String, owned: Box) -> String [read self.text, consume owned, suspend] {
        fail(kind, self.text, move owned)
    }
    func echo(self, text: String) -> String [consume text] { text }
}
func main(args: Arguments) -> String {
    let kind = args.values[1].copy()
    let text = "live caller"
    assert(discard(text, Box { text: "unused parameter" }) == "discarded")
    return fail(kind, text, Box { text: "owned argument" }) if args.values[0].copy() == "main"
    let index = 0
    loop {
        break if index == 8
        let worker = remote Worker { text: text + "receiver" }
        let failed = worker.fail(kind, Box { text: "remote argument" })
        let queued = worker.echo("queued argument")
        assert((await failed).error?())
        assert((await queued).error?())
        assert((await worker.echo("rejected argument")).error?())
        index = index + 1
    }
    text
}
"#,
        )
        .unwrap();
        let prepared = prepare(&compilation).unwrap();
        let temporary = TemporaryDirectory::create().unwrap();
        for optimize in [false, true] {
            let options = CompileOptions { optimize };
            let artifact = prepared.compile_object(options).unwrap();
            assert!(artifact.releases_result);
            let source = track_allocations(entry_source(artifact.result, artifact.accepts_arguments, &artifact.runtime_strings, artifact.releases_result))
                .replace("fn foster_host_response(response: FosterHostResponse) -> usize {", "fn foster_host_response(mut response: FosterHostResponse) -> usize {\n    if std::env::var_os(\"FOSTER_TEST_HOST_FAILURE\").is_some() { response.ok = false; response.error_message = \"injected host failure\".into(); }");
            assert!(source.contains("injected host failure"));
            let executable = temporary.path.join(format!(
                "failure-{optimize}{}",
                std::env::consts::EXE_SUFFIX
            ));
            link_source(artifact, &executable, options, &source, None).unwrap();
            let vm_program =
                vm::compile_with_options(&compilation, vm::CompileOptions { optimize }).unwrap();
            vm::verify(&vm_program).unwrap();
            for mode in ["main", "remote"] {
                for kind in [
                    "assert",
                    "bounds",
                    "arithmetic",
                    "closure",
                    "refclosure",
                    "update",
                    "reference",
                    "nested-reference",
                    "pending",
                    "partial",
                    "recursive",
                    "match",
                    "host",
                ] {
                    let expected = match kind {
                        "bounds" => "index",
                        "arithmetic" => "division",
                        "host" => "injected host failure",
                        "match" => "assertion failed: owned argument",
                        _ => "assertion failed: live caller",
                    };
                    if kind != "host" {
                        let arguments =
                            crate::entry::CommandArguments::new("cleanup", [mode, kind]);
                        let outcome =
                            vm::Machine::new(&vm_program).run_main_with_arguments(&arguments);
                        if mode == "main" {
                            let error = outcome.unwrap_err();
                            assert!(
                                error.to_string().contains(expected),
                                "VM {kind}, optimize={optimize}: {error}"
                            );
                        } else {
                            assert_eq!(
                                outcome.unwrap().to_string(),
                                "live caller",
                                "VM {kind}, optimize={optimize}"
                            );
                        }
                    }
                    let mut command = Command::new(&executable);
                    command.args([mode, kind]);
                    if kind == "host" {
                        command.env("FOSTER_TEST_HOST_FAILURE", "1");
                    }
                    let output = command.output().unwrap();
                    assert_eq!(
                        output.status.code(),
                        Some(if mode == "main" { 2 } else { 0 }),
                        "{mode}/{kind}, optimize={optimize}: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                    if mode == "remote" {
                        assert_eq!(
                            String::from_utf8_lossy(&output.stdout).trim(),
                            "live caller"
                        );
                    } else {
                        assert!(output.stdout.is_empty());
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        assert!(stderr.contains(expected), "{mode}/{kind}: {stderr}");
                    }
                }
            }
        }
    }

    #[test]
    fn remote_owner_shutdown_stops_running_work_and_reclaims_queued_messages() {
        let compilation = crate::compile("func main() -> Int { 0 }").unwrap();
        let prepared = prepare(&compilation).unwrap();
        let temporary = TemporaryDirectory::create().unwrap();
        let source = runtime_source()
            + r#"
static STARTED: (Mutex<bool>, std::sync::Condvar) = (Mutex::new(false), std::sync::Condvar::new());
static RELEASED: (Mutex<bool>, std::sync::Condvar) = (Mutex::new(false), std::sync::Condvar::new());
static QUEUED_CLEANUP: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

unsafe extern "C" fn release_state(_: usize) -> u8 {
    *RELEASED.0.lock().unwrap() = true;
    RELEASED.1.notify_all();
    0
}
unsafe extern "C" fn request(_: u64, arguments: usize, execute: u8) -> u64 {
    if execute == 0 {
        QUEUED_CLEANUP.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        return 0;
    }
    let kind = unsafe { *(arguments as *const u64) };
    if kind == 0 { return 7; }
    assert_eq!(kind, 1, "queued work ran after shutdown");
    *STARTED.0.lock().unwrap() = true;
    STARTED.1.notify_all();
    while foster_rt_v4_cancellation_point() == 0 {}
    0
}
fn wait(signal: &(Mutex<bool>, std::sync::Condvar)) {
    let (ready, timed) = signal.1.wait_timeout_while(signal.0.lock().unwrap(), std::time::Duration::from_secs(5), |ready| !*ready).unwrap();
    assert!(*ready && !timed.timed_out(), "worker failed to reach a cancellation boundary");
}
fn main() {
    foster_runtime_initialize(&[]);
    let remote = foster_rt_v4_remote_spawn(0, release_state as *const () as usize, 0);
    let invoke = |kind: u64, blocking| foster_rt_v4_remote_call(remote, request as *const () as usize, &kind as *const u64 as usize, 1, blocking, 0);
    let completed = invoke(0, 1);
    let running = invoke(1, 0);
    wait(&STARTED);
    let queued = invoke(2, 0);
    let discarded = invoke(3, 0);
    foster_rt_v4_future_release(discarded);
    foster_rt_v4_remote_release(remote);
    for future in [running, queued] {
        foster_rt_v4_future_await(future);
        assert_eq!(foster_rt_v4_future_error(future), 1);
        foster_rt_v4_future_release(future);
    }
    assert_eq!(foster_rt_v4_future_await(completed), 7);
    assert_eq!(foster_rt_v4_future_error(completed), 0);
    foster_rt_v4_future_release(completed);
    wait(&RELEASED);
    assert_eq!(QUEUED_CLEANUP.load(std::sync::atomic::Ordering::SeqCst), 2);
}
"#;
        for optimize in [false, true] {
            let options = CompileOptions { optimize };
            let artifact = prepared.compile_object(options).unwrap();
            let executable = temporary.path.join(format!(
                "shutdown-{optimize}{}",
                std::env::consts::EXE_SUFFIX
            ));
            link_source(artifact, &executable, options, &source, None).unwrap();
            let output = Command::new(executable).output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn virtual_threads_share_one_worker_and_suspend_native_stacks() {
        let compilation = crate::compile(include_str!(
            "../../tests/fixtures/programs/native_virtual_threads.fos"
        ))
        .unwrap();
        let prepared = prepare(&compilation).unwrap();
        let temporary = TemporaryDirectory::create().unwrap();
        for optimize in [false, true] {
            let options = CompileOptions { optimize };
            let artifact = prepared.compile_object(options).unwrap();
            let constants = artifact
                .runtime_strings
                .iter()
                .map(|value| format!("{value:?}"))
                .collect::<Vec<_>>()
                .join(", ");
            let source = runtime_source()
                + &include_str!("virtual_threads_test.rs").replace(
                    "foster_runtime_initialize(&[]);",
                    &format!("foster_runtime_initialize(&[{constants}]);"),
                );
            let executable = temporary.path.join(format!(
                "virtual-threads-{optimize}{}",
                std::env::consts::EXE_SUFFIX
            ));
            link_source(artifact, &executable, options, &source, None).unwrap();
            let output = Command::new(executable).output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout).trim(),
                "virtual threads passed"
            );
        }
    }

    #[test]
    fn managed_text_and_arguments_release_all_native_allocations() {
        let compilation = crate::compile(
            r#"
import core.string
import core.option
import core.result
import core.list
import core.byte
import core.bytes
import std.process
import std.iter
type Box = { text: String }
impl Box { func deinit(self) -> () { assert(self.text.length >= 0, "nonnegative length in destructor") } }
type Token = { symbol: Symbol }
type Echo = { text: String }
func identity(text: String) -> String { String.from_utf8(text.bytes).unwrap_or("invalid") }
impl Echo {
    func read(self: Echo) -> String { identity(self.text) }
}
func main(args: Arguments) -> String {
    return "" if args.values.empty?
    assert(String.from_utf8(Bytes.from([Byte.unchecked(255)])).error?())
    let tokens = [Token { symbol: :ready }]
    assert(tokens[0].symbol == :ready, "symbol read")
    let text = args.values[0].copy()
    let box = Box { text: identity(text) }
    let cursor = [Box { text: identity(text) }, Box { text: "unvisited" }].iterator()
    branch cursor.next() {
        Option.Some(value) -> { assert(value.text == text, "list cursor text") }
        Option.None -> { assert(false) }
    }
    let letters = (text + "λ🙂").iterator()
    assert(letters.next() == Option.Some("λ"), "grapheme cursor text")
    let octets = (text + "λ").bytes.iterator()
    assert(octets.next() == Option.Some(Byte.unchecked(206)), "byte cursor text")
    let worker = remote Echo { text: identity(text) }
    let index = 0
    loop {
        break if index == 32
        let pending = worker.read()
        let text_copy = box.text + "🙂"
        let encoded = text_copy.bytes
        assert(String.from_utf8(move encoded).unwrap_or("invalid") == text_copy, "UTF-8 round trip")
        let captured = [move text_copy] () -> text_copy
        assert(captured() == text + "🙂", "captured text")
        assert((await pending).unwrap_or("") == text, "remote text")
        index = index + 1
    }
    branch box.text {
        "λ" -> identity(box.text)
        "" -> identity(box.text)
        _ -> "wrong argument"
    }
}
"#,
        )
        .unwrap();
        let prepared = prepare(&compilation).unwrap();
        let temporary = TemporaryDirectory::create().unwrap();
        for optimize in [false, true] {
            let options = CompileOptions { optimize };
            let artifact = prepared.compile_object(options).unwrap();
            assert!(artifact.releases_result);
            let source = entry_source(
                artifact.result,
                artifact.accepts_arguments,
                &artifact.runtime_strings,
                artifact.releases_result,
            );
            let source = track_allocations(source);
            let executable = temporary
                .path
                .join(format!("text-{optimize}{}", std::env::consts::EXE_SUFFIX));
            link_source(artifact, &executable, options, &source, None).unwrap();
            for argument in [None, Some(""), Some("λ")] {
                let output = Command::new(&executable).args(argument).output().unwrap();
                assert!(
                    output.status.success(),
                    "optimize={optimize}, argument={argument:?}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                assert_eq!(
                    String::from_utf8_lossy(&output.stdout).trim(),
                    argument.unwrap_or("")
                );
            }
        }
    }
}
