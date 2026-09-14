use foster::vm::{CompileOptions, Value};

#[test]
fn agent_guide_examples_execute() {
    let guide = include_str!("../docs/writing-foster.md").replace("\r\n", "\n");
    let examples = guide.split("```foster\n").skip(1).collect::<Vec<_>>();
    assert!(
        !examples.is_empty(),
        "the agent guide must contain runnable examples"
    );
    for (index, example) in examples.iter().enumerate() {
        let source = example.split_once("```").expect("closed example fence").0;
        let compilation = foster::compile(source)
            .unwrap_or_else(|error| panic!("agent guide example {}: {error:?}", index + 1));
        for optimize in [false, true] {
            let result = foster::vm::run_with_options(&compilation, CompileOptions { optimize })
                .unwrap_or_else(|error| panic!("agent guide example {}: {error:?}", index + 1));
            assert_eq!(
                result,
                Value::Integer(42),
                "example {}, optimize={optimize}",
                index + 1
            );
        }
    }
}
