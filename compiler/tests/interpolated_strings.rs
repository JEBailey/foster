use foster_compiler::{lexer, parser};

#[test]
fn triple_strings_keep_source_ranges_and_following_lines() {
    let source = "\"\"\"\nλ %name%\n\"\"\"\nnext";
    let tokens = lexer::lex(source).unwrap();
    assert_eq!(tokens[0].range, 0..source.find("\nnext").unwrap());
    assert!(
        matches!(&tokens[0].kind, lexer::TokenKind::InterpolatedString(raw) if raw == "\nλ %name%\n")
    );
    assert_eq!(tokens[2].line, 4);
    assert_eq!(tokens[2].column, 1);
}

#[test]
fn malformed_substitutions_and_escapes_are_rejected() {
    for body in ["%missing", "%1%", "%a.b%", "%a + b%", "%let%", "%_%", "\\q"] {
        let source = format!("func main() {{ \"\"\"{body}\"\"\" }}");
        assert!(
            parser::parse(lexer::lex(&source).unwrap()).is_err(),
            "{body}"
        );
    }
    assert!(lexer::lex("\"\"\"unterminated").is_err());
    assert!(lexer::lex("\"ordinary\nstring\"").is_err());
}

#[test]
fn substitutions_require_visible_string_conversion_and_preserve_effects() {
    for source in [
        "type Item = { value: Int }\nfunc main() { let item = Item { value: 1 }\n\"\"\"%item%\"\"\" }",
        "func main() { \"\"\"%missing%\"\"\" }",
        "type Item = { value: Int }\nimpl Item { func as_string(self) -> String [mut self] { self.value = 1\n\"item\" } }\nfunc invalid(item: Item) -> String [read item] { \"\"\"%item%\"\"\" }",
    ] {
        assert!(foster_compiler::compile(source).is_err(), "{source}");
    }
}

#[test]
fn triple_string_fixture_typechecks() {
    foster_compiler::compile(include_str!(
        "../../tests/fixtures/programs/interpolated_strings.fos"
    ))
    .unwrap();
}

#[test]
fn malformed_expression_substitutions_are_rejected() {
    for body in ["%{}%", "%{1 2}%", "%{1 +}%", "%{1}", "%{1%"] {
        let source = format!("func main() {{ \"\"\"{body}\"\"\" }}");
        let result = lexer::lex(&source).and_then(parser::parse);
        assert!(result.is_err(), "{body}");
    }
}

#[test]
fn expression_diagnostics_use_source_coordinates() {
    let source = "func main() {\n    \"\"\"\nλ %{1 + }%\n\"\"\"\n}";
    let error = parser::parse(lexer::lex(source).unwrap()).unwrap_err();
    assert_eq!((error.line, error.column), (3, 9));
}
