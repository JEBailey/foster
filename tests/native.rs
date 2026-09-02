use foster::native::{CompileOptions, NativeType, build_executable, compile_object};

#[test]
fn compiles_reachable_primitive_functions_to_an_object() {
    let compilation = foster::compile(
        r#"
func unused_text() -> String { "the native reachability pass must ignore this" }

func choose(early: Bool, left: Float, right: Float) -> Float {
    return left if early
    right
}

func main() -> Float {
    let count = 0
    loop {
        count = count + 1
        continue if count < 2
        break
    }
    assert(2 < 3, "native assertion should pass")
    choose(true, 40.0 + 2.5, 0.0)
}
"#,
    )
    .unwrap();
    let artifact = compile_object(&compilation, CompileOptions::default()).unwrap();
    assert!(!artifact.bytes.is_empty());
    assert!(
        artifact
            .bytes
            .windows(b"foster_layout_0".len())
            .any(|bytes| bytes == b"foster_layout_0"),
        "native object should retain physical layout descriptors"
    );
    assert_eq!(artifact.result, NativeType::Float);
}

#[test]
fn compiles_lossless_integer_widening_to_an_int_result() {
    let compilation = foster::compile("func main() -> Int { 'A' }").unwrap();
    let artifact = compile_object(&compilation, CompileOptions::default()).unwrap();
    assert!(!artifact.bytes.is_empty());
    assert_eq!(artifact.result, NativeType::Int);
}

#[test]
fn rejects_unsupported_reachable_types_with_actionable_guidance() {
    let compilation = foster::compile(r#"func main() -> Symbol { :hello }"#).unwrap();
    let error = compile_object(&compilation, CompileOptions::default()).unwrap_err();
    assert!(
        error.message.contains("does not yet support type"),
        "{error}"
    );
    assert!(
        error
            .help
            .as_deref()
            .is_some_and(|help| help.contains("without `--native`")),
        "{error:?}"
    );
}

#[test]
fn compiles_the_command_arguments_entry_abi() {
    let compilation = foster::compile(include_str!("fixtures/programs/arguments.fos")).unwrap();
    let artifact = compile_object(&compilation, CompileOptions::default()).unwrap();
    assert!(!artifact.bytes.is_empty());
    assert_eq!(artifact.result, NativeType::String);
    assert!(artifact.accepts_arguments);
}

#[test]
fn lowers_records_and_copy_on_write_field_updates_to_cranelift() {
    let compilation = foster::compile(
        r#"
type Inner = { label: String, value: Int }
type Pair = { left: Inner, right: Inner }

func inner_value(inner: Inner) -> Int { inner.value }

func main() -> Int {
    let pair = Pair {
        left: Inner { label: "ok", value: 18 }
        right: Inner { label: "", value: 0 }
    }
    pair.right = Inner { label: "", value: 22 }
    inner_value(pair.left) + inner_value(pair.right) + pair.left.label.length
}
"#,
    )
    .unwrap();
    let artifact = compile_object(&compilation, CompileOptions::default()).unwrap();
    assert!(!artifact.bytes.is_empty());
    assert_eq!(artifact.result, NativeType::Int);

    let executable = std::env::temp_dir().join(format!(
        "foster-native-record-test-{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    build_executable(&compilation, &executable, CompileOptions::default()).unwrap();
    let output = std::process::Command::new(&executable).output().unwrap();
    let _ = std::fs::remove_file(&executable);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "42");
}

#[test]
fn lowers_variant_tags_payloads_and_pattern_bindings_to_cranelift() {
    let compilation = foster::compile(
        r#"
enum Choice = Number(Int) | Empty

func main() -> Int {
    let choice = Number(42)
    branch choice {
        Number(value) -> value
        Empty -> 0
    }
}
"#,
    )
    .unwrap();
    let artifact = compile_object(&compilation, CompileOptions::default()).unwrap();
    assert!(!artifact.bytes.is_empty());

    let executable = std::env::temp_dir().join(format!(
        "foster-native-variant-test-{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    build_executable(&compilation, &executable, CompileOptions::default()).unwrap();
    let output = std::process::Command::new(&executable).output().unwrap();
    let _ = std::fs::remove_file(&executable);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "42");
}
