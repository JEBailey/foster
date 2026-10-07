use crate::ast::*;
use crate::error::FosterError;
use crate::lexer::{Token, TokenKind};

mod cursor;
mod declarations;
mod expressions;
mod interpolation;
mod iteration;

pub fn parse(tokens: Vec<Token>) -> Result<Program, FosterError> {
    Parser::new(tokens).program()
}

/// Parse as much of a source module as possible for interactive tooling.
///
/// A damaged top-level declaration is represented by a recovery node and excluded from the
/// returned program. Parsing resumes at the next declaration boundary, allowing later declarations
/// to be lowered and type checked independently.
pub fn parse_recovering(tokens: Vec<Token>) -> RecoveringParse {
    Parser::new(tokens).program_recovering()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryKind {
    Declaration,
    Expression,
    Statement,
    Type,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryNode {
    pub kind: RecoveryKind,
    pub range: std::ops::Range<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecoveringParse {
    pub program: Program,
    pub diagnostics: Vec<FosterError>,
    pub recovery_nodes: Vec<RecoveryNode>,
}

struct Parser {
    tokens: Vec<Token>,
    current: usize,
    suppress_record_literal: bool,
    iteration_span: Option<std::ops::Range<usize>>,
}

struct ParsedEffects {
    explicit: bool,
    effects: Vec<Effect>,
    spans: Vec<std::ops::Range<usize>>,
    suspend_span: Option<std::ops::Range<usize>>,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Self {
            tokens,
            current: 0,
            suppress_record_literal: false,
            iteration_span: None,
        }
    }

    fn spanned(&self, start: usize, expression: Expr) -> Expr {
        Expr::Spanned {
            expression: Box::new(expression),
            span: start..self.tokens[self.current.saturating_sub(1)].range.end,
        }
    }

    fn spanned_pattern(&self, start: usize, pattern: Pattern) -> Pattern {
        Pattern::Spanned {
            pattern: Box::new(pattern),
            span: start..self.tokens[self.current.saturating_sub(1)].range.end,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comma_separated_lists_allow_newlines_at_their_boundaries() {
        let source = r#"
enum Pair = Values(
    Int
    , Int
)
type Fields = {
    left: Int
    , right: Int
}
func sum(
    left: Int
    , right: Int
) -> Int { left + right }
func main() -> Int {
    let values = [
        20 // a comment before the comma
        ,

        22
    ]
    let fields = Fields {
        left: values[0]
        , right: values[1]
    }
    let {
        left
        , right
    } = fields
    let action = [
        copy left
        , copy right
    ] (
    ) -> {
        let first = left
        first + right
    }
    assert(
        action() == 42
        , "multiline captures must preserve their values"
    )
    branch Pair.Values(
        left
        , right
    ) {
        Pair.Values(
            a
            , b
        ) -> sum(
            a
            , b
        )
    }
}
"#;
        assert_eq!(crate::run(source).unwrap(), crate::vm::Value::Integer(42));
        assert!(
            crate::parse_recovering(source)
                .unwrap()
                .diagnostics
                .is_empty()
        );
    }

    #[test]
    fn multiline_type_parameter_argument_and_effect_lists_parse() {
        crate::parse(
            r#"
type Pair<
    T
    , U
> = { left: T, right: U }
impl Pair<
    T
    , U
> {
    func pick(
        self: Self
        , input: T
    ) -> T [
        read self
        , read input
    ] { input }
}
type Contract = {
    func apply<
        T
        , U
    > (
        self: Self
        , value: Pair<
            T
            , U
        >
    ) -> ()
}
func apply<
    T
    , U
>(
    callback: func(
        T
        , U
    ) -> ()
    , left: T
    , right: U
) -> () { callback(left, right) }
func main() -> Int {
    let callback = (
        left: Int
        , right: Int
    ) -> left + right
    callback(20, 22)
}
"#,
        )
        .unwrap();
    }

    #[test]
    fn multiline_lists_still_require_commas_and_blocks_keep_statement_boundaries() {
        for source in [
            "func main() { let values = [1\n2] }",
            "func f(a: Int, b: Int) {}\nfunc main() { f(1\n2) }",
            "func main(a: Int\nb: Int) {}",
            "type Pair<T\nU> = { a: T, b: U }",
        ] {
            assert!(crate::parse(source).is_err(), "{source}");
        }
        let source = "func main() -> Int {\nlet a = 20\nlet b = 22\na + b\n}";
        let program = crate::parse(source).unwrap();
        assert_eq!(program.functions[0].body.len(), 3);
        assert_eq!(crate::run(source).unwrap(), crate::vm::Value::Integer(42));
    }

    #[test]
    fn empty_multiline_lists_and_delimited_unit_remain_valid() {
        crate::parse("func main(\n) -> (\n) {\nlet values = [\n]\nprintln(\n)\n(\n)\n}").unwrap();
        assert!(crate::parse("enum Empty = Case(\n)\n").is_err());
    }

    #[test]
    fn copy_is_an_identifier_outside_capture_mode_position() {
        let tokens = crate::lexer::lex("copy").unwrap();
        assert_eq!(tokens[0].kind, TokenKind::Ident("copy".into()));
        let source = "func copy(value: Int) -> Int { value + 1 }\n\
            func keep(copy: Int) -> Int { copy }\n\
            func main() -> Int {\n\
                let calls = [copy(20)]\n\
                let copy = calls[0]\n\
                let values = [copy, copy + 1]\n\
                let captured = [copy copy] () -> copy\n\
                keep(values[0]) + captured()\n\
            }";
        assert_eq!(crate::run(source).unwrap(), crate::vm::Value::Integer(42));
        crate::parse("func main(copy: Int) { let values = [copy] }").unwrap();
    }

    #[test]
    fn copy_capture_mode_can_follow_other_capture_modes() {
        crate::parse("func main() { let action = [move item, copy count, ref other] () -> count }")
            .unwrap();
    }

    #[test]
    fn recovery_keeps_later_declarations_and_reports_independent_errors() {
        let source = "func broken_expression() -> Int { let value = }\n\
                      type Broken = { value: }\n\
                      func healthy() -> Int { 42 }\n";
        let parsed = crate::parse_recovering(source).unwrap();

        assert_eq!(parsed.diagnostics.len(), 2, "{:?}", parsed.diagnostics);
        assert_eq!(parsed.recovery_nodes.len(), 2);
        assert!(
            parsed
                .program
                .functions
                .iter()
                .any(|function| function.name == "healthy")
        );
        assert!(
            parsed
                .recovery_nodes
                .iter()
                .any(|node| node.kind == RecoveryKind::Expression)
        );
        assert!(
            parsed
                .recovery_nodes
                .iter()
                .any(|node| node.kind == RecoveryKind::Type)
        );
    }

    #[test]
    fn recovery_always_advances_on_unexpected_top_level_tokens() {
        let source = "}\n}\nfunc healthy() -> Int { 42 }\n";
        let parsed = crate::parse_recovering(source).unwrap();

        assert_eq!(parsed.diagnostics.len(), 1);
        assert_eq!(parsed.program.functions.len(), 1);
        assert_eq!(parsed.program.functions[0].name, "healthy");
    }

    #[test]
    fn strict_parser_still_rejects_a_damaged_module() {
        let source = "func broken() -> Int { let value = }\nfunc healthy() -> Int { 42 }\n";
        assert!(crate::parse(source).is_err());
    }
}
