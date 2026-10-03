use crate::error::FosterError;
#[cfg(any(not(foster_native_formatter), test))]
use crate::tooling;

/// Validates source with the compiler, then applies Foster-written formatting policy.
pub fn format(source: &str) -> Result<String, FosterError> {
    crate::compiler::cancellation::check()?;
    crate::parse(source)?;
    #[cfg(foster_native_formatter)]
    return native::format(source);
    #[cfg(not(foster_native_formatter))]
    {
        static TOOL: tooling::Tool =
            tooling::Tool::new(include_bytes!(concat!(env!("OUT_DIR"), "/format.fbc")));
        tooling::string(&TOOL.run(vec![source.to_owned()])?)
    }
}

#[cfg(foster_native_formatter)]
mod native {
    use super::FosterError;
    use foster_native_runtime as runtime;
    use std::sync::Once;
    include!(concat!(env!("OUT_DIR"), "/format_constants.rs"));
    unsafe extern "C" {
        fn foster_native_arguments(executable: usize, values: usize, length: i64) -> usize;
        fn foster_native_entry(arguments: usize) -> usize;
        fn foster_native_string_data(value: usize) -> usize;
        fn foster_native_string_length(value: usize) -> i64;
        fn foster_native_release_result(value: usize) -> u8;
    }
    pub(super) fn format(source: &str) -> Result<String, FosterError> {
        static INITIALIZE: Once = Once::new();
        INITIALIZE.call_once(|| runtime::foster_runtime_initialize(CONSTANTS));
        let result =
            runtime::embedded_execution(crate::compiler::cancellation::is_cancelled, || {
                // The generated importer transfers these owned strings into Arguments;
                // the consuming entry releases that record and its fields on every exit.
                let executable = runtime::owned_string("foster");
                let values = [runtime::owned_string(source)];
                unsafe {
                    let arguments =
                        foster_native_arguments(executable, values.as_ptr() as usize, 1);
                    let value = foster_native_entry(arguments);
                    if value == 0 {
                        return Err(FosterError::runtime("native formatter returned no text"));
                    }
                    let bytes = std::slice::from_raw_parts(
                        foster_native_string_data(value) as *const u8,
                        foster_native_string_length(value) as usize,
                    );
                    let text = String::from_utf8(bytes.to_vec()).map_err(|_| {
                        FosterError::runtime("native formatter returned invalid UTF-8")
                    });
                    foster_native_release_result(value);
                    text
                }
            });
        crate::compiler::cancellation::check()?;
        result.map_err(FosterError::runtime)?
    }
}

#[cfg(test)]
mod tests {
    use super::format;

    #[cfg(foster_native_formatter)]
    #[test]
    fn native_and_bytecode_formatters_agree() {
        static TOOL: super::tooling::Tool =
            super::tooling::Tool::new(include_bytes!(concat!(env!("OUT_DIR"), "/format.fbc")));
        let source = "// λ🙂 and literal braces { }\r\nfunc value() -> String {\r\nlet text = \"\"\"\r\n  %{branch { true -> \"{λ}\" _ -> \"}%\" }}%\r\n\"\"\"\r\ntext\r\n}\r\n";
        let interpreted = super::tooling::string(&TOOL.run(vec![source.into()]).unwrap()).unwrap();
        for _ in 0..5 {
            assert_eq!(format(source).unwrap(), interpreted);
        }
    }

    #[test]
    fn formatting_honors_cancellation_during_execution() {
        use std::{cell::Cell, rc::Rc};
        let polls = Rc::new(Cell::new(0));
        let probe = Rc::clone(&polls);
        let source = "func main() -> Int { 42 }\n".repeat(100);
        let error = crate::compiler::cancellation::scope(
            move || {
                probe.set(probe.get() + 1);
                probe.get() > 2
            },
            || format(&source),
        )
        .unwrap_err();
        assert!(crate::compiler::cancellation::is_cancellation(&error));
        assert_eq!(
            format("func main() -> Int { 42 }\n").unwrap(),
            "func main() -> Int {\n    42\n}\n"
        );
    }

    #[test]
    fn multiline_comma_lists_format_and_keep_their_behavior() {
        let source = "func sum(\na: Int\n, b: Int\n) -> Int { a + b }\nfunc main() -> Int {\nlet values = [\n20\n, 22\n]\nsum(\nvalues[0]\n, values[1]\n)\n}\n";
        let formatted = format(source).unwrap();
        crate::parse(&formatted).unwrap();
        assert_eq!(format(&formatted).unwrap(), formatted);
        assert_eq!(
            crate::run(&formatted).unwrap(),
            crate::vm::Value::Integer(42)
        );
    }

    #[test]
    fn formats_indentation_and_preserves_comments_and_literals() {
        let source = "// { retained\r\nfunc main() -> String {  \r\nlet value = \"}\"\r\nbranch {\r\ntrue -> value\r\n_ -> \"no\"\r\n}\r\n}\r\n";
        assert_eq!(
            format(source).unwrap(),
            "// { retained\nfunc main() -> String {\n    let value = \"}\"\n    branch {\n        true -> value\n        _ -> \"no\"\n    }\n}\n"
        );
    }

    #[test]
    fn formats_multiline_type_composition() {
        let source =
            "type Foo =\nBar\n& What\n& SomeContract\n& {\npub func describe(self) -> String\n}\n";
        assert_eq!(
            format(source).unwrap(),
            "type Foo =\n    Bar\n    & What\n    & SomeContract\n    & {\n        pub func describe(self) -> String\n    }\n"
        );
    }

    #[test]
    fn formats_multiline_enum_declarations() {
        let source = "enum Choice =\nLeft(Int, Bool)\n| Right(String)\n";
        assert_eq!(
            format(source).unwrap(),
            "enum Choice = Left(Int, Bool)\n    | Right(String)\n"
        );
    }

    #[test]
    fn preserves_the_canonical_inline_first_enum_case() {
        let source = "pub enum Option<T> = Some(T)\n| None\n";
        assert_eq!(
            format(source).unwrap(),
            "pub enum Option<T> = Some(T)\n    | None\n"
        );
    }

    #[test]
    fn rejects_invalid_source() {
        assert!(format("func main( {\n").is_err());
    }

    #[test]
    fn expands_inline_blocks_and_preserves_program_behavior() {
        let source = "func main() -> Int { branch { true -> { 42 } _ -> { 0 } } }\n";
        let expected = "func main() -> Int {\n    branch {\n        true -> {\n            42\n        }\n        _ -> {\n            0\n        }\n    }\n}\n";
        let formatted = format(source).unwrap();
        assert_eq!(formatted, expected);
        assert_eq!(format(&formatted).unwrap(), formatted);
        assert_eq!(crate::run(&formatted).unwrap(), crate::run(source).unwrap());
    }

    #[test]
    fn formats_test_declarations() {
        assert_eq!(
            format("test \"works\" {\nprintln()\n}\n").unwrap(),
            "test \"works\" {\n    println()\n}\n"
        );
    }

    #[test]
    fn formats_loops_and_guarded_transfers() {
        assert_eq!(
            format("func main() {\nloop {\ncontinue if false\nbreak\n}\n}\n").unwrap(),
            "func main() {\n    loop {\n        continue if false\n        break\n    }\n}\n"
        );
    }

    #[test]
    fn formats_if_statements_with_inline_and_braced_bodies() {
        let source =
            "func main() {\nlet value = 0\nif true value = 42\nif false { println(value) }\n}\n";
        let expected = "func main() {\n    let value = 0\n    if true value = 42\n    if false {\n        println(value)\n    }\n}\n";
        let formatted = format(source).unwrap();
        assert_eq!(formatted, expected);
        assert_eq!(format(&formatted).unwrap(), formatted);
    }

    #[test]
    fn formats_while_without_expanding_the_sugar() {
        let formatted =
            "func main() {\n    while true {\n        // stop here\n        break\n    }\n}\n";
        assert_eq!(
            format("func main() {\nwhile true {\n// stop here\nbreak\n}\n}\n").unwrap(),
            formatted
        );
        assert_eq!(format(formatted).unwrap(), formatted);
    }

    #[test]
    fn formats_for_without_expanding_the_sugar() {
        let expected = "func main() {\n    for item in [1, 2] {\n        // keep this comment\n        println(item)\n    }\n}\n";
        assert_eq!(
            format(
                "func main() {\nfor item in [1, 2] {\n// keep this comment\nprintln(item)\n}\n}\n"
            )
            .unwrap(),
            expected
        );
        assert_eq!(format(expected).unwrap(), expected);
    }

    #[test]
    fn formats_multiline_branch_arms() {
        assert_eq!(
            format("func main() -> Int {\nbranch {\ntrue -> {\nlet value = 42\nvalue\n}\n_ -> 0\n}\n}\n").unwrap(),
            "func main() -> Int {\n    branch {\n        true -> {\n            let value = 42\n            value\n        }\n        _ -> 0\n    }\n}\n"
        );
    }

    #[test]
    fn formats_try_expressions() {
        let source = "import core.result.*\nfunc checked() -> Result<Int, String> {\nlet value = try Result.Ok(42)\nResult.Ok(value)\n}\n";
        assert_eq!(
            format(source).unwrap(),
            "import core.result.*\nfunc checked() -> Result<Int, String> {\n    let value = try Result.Ok(42)\n    Result.Ok(value)\n}\n"
        );
    }
}
