//! Check the time guide's examples together with its documented imports.
#[test]
fn time_guide_examples_execute() {
    let guide = include_str!("../docs/time.md").replace("\r\n", "\n");
    let mut source = String::new();
    let mut calls = Vec::new();
    for (index, block) in guide.split("```foster\n").skip(1).enumerate() {
        let code = block.split_once("```").expect("closed Foster fence").0;
        if code.trim_start().starts_with("import ") || code.trim_start().starts_with("func ") {
            source.push_str(code);
            source.push('\n');
        } else {
            let name = format!("time_example_{index}");
            source.push_str(&format!("func {name}() -> () {{\n{code}\n()\n}}\n"));
            calls.push(format!("{name}()"));
        }
    }
    assert!(!calls.is_empty(), "the time guide must contain examples");
    source.push_str(&format!(
        "func main() -> () {{\n{}\n()\n}}",
        calls.join("\n")
    ));
    let compilation = foster::compile(&source).expect("time guide examples must compile");
    foster::vm::run(&compilation).expect("time guide assertions must pass");
}
