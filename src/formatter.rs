use crate::{error::FosterError, tooling};

/// Validates source with the compiler, then applies Foster-written formatting policy.
pub fn format(source: &str) -> Result<String, FosterError> {
    crate::parse(source)?;
    static TOOL: tooling::Tool =
        tooling::Tool::new(include_bytes!(concat!(env!("OUT_DIR"), "/format.fbc")));
    tooling::string(&TOOL.run(vec![source.to_owned()])?)
}

#[cfg(test)]
mod tests {
    use super::format;

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
        let source = "import core.result\nfunc checked() -> Result<Int, String> {\nlet value = try Result.Ok(42)\nResult.Ok(value)\n}\n";
        assert_eq!(
            format(source).unwrap(),
            "import core.result\nfunc checked() -> Result<Int, String> {\n    let value = try Result.Ok(42)\n    Result.Ok(value)\n}\n"
        );
    }
}
