#[test]
fn json_guide_example_runs() {
    let guide = include_str!("../docs/json.md").replace("\r\n", "\n");
    for example in guide.split("```foster\n").skip(1) {
        let source = example.split_once("```").unwrap().0;
        let compilation = foster::compile(source).unwrap();
        assert_eq!(
            foster::vm::run(&compilation).unwrap().to_string(),
            "Result.Ok(42)"
        );
    }
}

#[test]
fn moving_a_local_does_not_consume_a_same_named_caller_parameter() {
    let compilation = foster::compile(
        r#"
import core.string
import core.string.*
import static core.string.*
func count(value: String) -> Int [consume value] { value.length }
func inspect(value: String) -> Int [read value] {
    let owned = "test"
    count(move owned) + value.length
}
func main() -> Int { inspect("abc") }
"#,
    )
    .unwrap();
    assert_eq!(foster::vm::run(&compilation).unwrap().to_string(), "7");
}

#[test]
fn json_public_api_runs_on_vm_bytecode_and_native() {
    let compilation = foster::compile(include_str!("fixtures/programs/json.fos")).unwrap();
    for optimize in [false, true] {
        let program =
            foster::vm::compile_with_options(&compilation, foster::vm::CompileOptions { optimize })
                .unwrap();
        let binary = foster::vm::encode_program(&program).unwrap();
        let decoded = foster::vm::decode_program(&binary).unwrap();
        assert_eq!(
            foster::vm::Machine::new(&decoded.clone().into_verified().unwrap())
                .run_main()
                .unwrap()
                .to_string(),
            "Result.Ok(42)"
        );
    }
    let directory = std::env::temp_dir().join(format!("foster-json-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let executable = directory.join(format!("json{}", std::env::consts::EXE_SUFFIX));
    foster::native::build_executable(&compilation, &executable, Default::default()).unwrap();
    let output = std::process::Command::new(executable).output().unwrap();
    assert!(
        output.status.success(),
        "status: {}; stdout: {}; stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "Result.Ok(42)"
    );
}
