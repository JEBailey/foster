use foster::native::{CompileOptions, NativeType, compile_object};

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
