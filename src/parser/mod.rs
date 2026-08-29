use crate::ast::*;
use crate::error::FosterError;
use crate::lexer::{Token, TokenKind};

mod cursor;
mod declarations;
mod expressions;

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
