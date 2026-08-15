use crate::ast::*;
use crate::error::FosterError;
use crate::lexer::{Token, TokenKind};

mod cursor;
mod declarations;
mod expressions;

pub fn parse(tokens: Vec<Token>) -> Result<Program, FosterError> {
    Parser {
        tokens,
        current: 0,
        suppress_record_literal: false,
    }
    .program()
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
