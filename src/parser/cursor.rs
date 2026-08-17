use super::*;

impl Parser {
    pub(super) fn documentation(&mut self) -> Option<String> {
        let mut lines = Vec::new();
        while let TokenKind::DocComment(value) = &self.peek().kind {
            lines.push(value.clone());
            self.advance();
            self.newlines();
        }
        (!lines.is_empty()).then(|| lines.join("\n"))
    }

    pub(super) fn newlines(&mut self) {
        while self.take(&TokenKind::Newline) {}
    }
    pub(super) fn at(&self, kind: &TokenKind) -> bool {
        self.peek().kind == *kind
    }
    pub(super) fn take(&mut self, kind: &TokenKind) -> bool {
        if self.at(kind) {
            self.advance();
            true
        } else {
            false
        }
    }

    pub(super) fn take_ident(&mut self, expected: &str) -> bool {
        if matches!(&self.peek().kind, TokenKind::Ident(name) if name == expected) {
            self.advance();
            true
        } else {
            false
        }
    }
    pub(super) fn expect(&mut self, kind: &TokenKind, message: &str) -> Result<(), FosterError> {
        if self.take(kind) {
            Ok(())
        } else {
            Err(self.error(message))
        }
    }
    pub(super) fn expect_ident(&mut self, message: &str) -> Result<String, FosterError> {
        let token = self.advance().clone();
        match token.kind {
            TokenKind::Ident(name) => Ok(name),
            TokenKind::Copy => Ok("copy".into()),
            TokenKind::Move => Ok("move".into()),
            TokenKind::Ref => Ok("ref".into()),
            TokenKind::Group => Ok("group".into()),
            TokenKind::Read => Ok("read".into()),
            TokenKind::Mut => Ok("mut".into()),
            TokenKind::Reshape => Ok("reshape".into()),
            TokenKind::Consume => Ok("consume".into()),
            TokenKind::Suspend => Ok("suspend".into()),
            TokenKind::Pub => Ok("pub".into()),
            TokenKind::Type => Ok("type".into()),
            _ => Err(FosterError::new(message, token.line, token.column)),
        }
    }
    pub(super) fn error(&self, message: &str) -> FosterError {
        FosterError::new(message, self.peek().line, self.peek().column)
    }
    pub(super) fn peek(&self) -> &Token {
        &self.tokens[self.current]
    }
    pub(super) fn peek_n(&self, n: usize) -> Option<&Token> {
        self.tokens.get(self.current + n)
    }
    pub(super) fn advance(&mut self) -> &Token {
        let index = self.current;
        if !self.at(&TokenKind::Eof) {
            self.current += 1;
        }
        &self.tokens[index]
    }
}
