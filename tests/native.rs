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
fn lowers_native_reference_captures() {
    let compilation = foster::compile(
        r#"
func main() -> Int {
    let value = 39
    let add = [ref value] (amount: Int) -> {
        value = value + amount
        value
    }
    assert(add(1) == 40)
    let direct = 40
    assert(increment(ref direct, 2) == 42)
    let values = [40]
    assert(increment(ref values[0], 2) == 42)
    let boxed = Outer { inner: Boxed { value: 40 } }
    increment(ref boxed.inner.value, 2)
}

type Boxed<T> = { value: T }
type Outer<T> = { inner: Boxed<T> }

func increment[g: group Int](value: ref[g] Int, amount: Int) -> Int [mut g] {
    value = value + amount
    value
}
"#,
    )
    .unwrap();
    let executable = std::env::temp_dir().join(format!(
        "foster-native-reference-test-{}{}",
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
type Boxed = { value: Int }
enum Choice = Number(Boxed) | Empty

func main() -> Int {
    let choice = Number(Boxed { value: 42 })
    branch choice {
        Number(boxed) -> boxed.value
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

#[test]
fn monomorphizes_generic_functions_and_record_layouts() {
    let compilation = foster::compile(
        r#"
type Boxed<T> = { value: T }
enum Maybe<T> = Some(T) | None

func identity<T>(value: T) -> T [consume value] { value }
func boxed<T>(value: T) -> Boxed<T> [consume value] { Boxed { value } }
func Boxed.unbox<T>(self: Boxed<T>) -> T [consume self] { self.value }
func value_or<T>(value: Maybe<T>, fallback: T) -> T [consume value, consume fallback] {
    branch value {
        Some(found) -> found
        None -> fallback
    }
}

func main() -> Int {
    assert(identity(1.5) == 1.5)
    boxed(identity(40)).unbox() + value_or(Some(2), 0)
}
"#,
    )
    .unwrap();
    let executable = std::env::temp_dir().join(format!(
        "foster-native-generic-test-{}{}",
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
fn specializes_closure_environments_and_capture_destructors() {
    let compilation = foster::compile(
        r#"
type Offset = { value: Int }

func make_offset<T>(marker: T, offset: Offset) [consume marker, consume offset] {
    [move marker, move offset] (value: Int) -> {
        marker
        offset.value + value
    }
}

func main() -> Int {
    let add = make_offset(0, Offset { value: 40 })
    add(2)
}
"#,
    )
    .unwrap();
    let executable = std::env::temp_dir().join(format!(
        "foster-native-closure-test-{}{}",
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
fn lowers_native_lists_and_core_list_algorithms() {
    let compilation = foster::compile(
        r#"
import core.list
import core.option
import core.float
import core.byte

func main() -> Int {
    let values = [10, 0]
    values[1] = 20
    values.push(12)
    let copied = values.append(100)
    assert(values.length == 3)
    assert(copied.length == 4)
    assert(from_code_point(65) == 'A')
    assert(parse_float("42.5") == 42.5)
    assert(42.5.as_string() == "42.5")
    assert(Byte.valid(255))
    assert(!Byte.valid(256))
    assert(Byte.unchecked(42) == Byte.unchecked(42))
    branch values.last() {
        Option.Some(value) -> value + 30
        Option.None -> 0
    }
}

"#,
    )
    .unwrap();
    let executable = std::env::temp_dir().join(format!(
        "foster-native-list-test-{}{}",
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
