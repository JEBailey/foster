use super::*;

impl Parser {
    pub(super) fn module_documentation(&mut self) -> Option<String> {
        let mut lines = Vec::new();
        while let TokenKind::ModuleDocComment(value) = &self.peek().kind {
            lines.push(value.clone());
            self.advance();
            self.newlines();
        }
        (!lines.is_empty()).then(|| lines.join("\n"))
    }

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
        if *kind == TokenKind::Comma {
            let mut next = self.current;
            while self.tokens[next].kind == TokenKind::Newline {
                next += 1;
            }
            if self.tokens[next].kind != TokenKind::Comma {
                return false;
            }
            self.current = next + 1;
            self.newlines();
            return true;
        }
        if self.at(kind) {
            self.advance();
            if matches!(kind, TokenKind::LParen | TokenKind::LBracket) {
                self.newlines();
            }
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
        if matches!(
            kind,
            TokenKind::RParen | TokenKind::RBracket | TokenKind::Greater
        ) {
            self.newlines();
        }
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
            TokenKind::Move => Ok("move".into()),
            TokenKind::Ref => Ok("ref".into()),
            TokenKind::Read => Ok("read".into()),
            TokenKind::Mut => Ok("mut".into()),
            TokenKind::Reshape => Ok("reshape".into()),
            TokenKind::Consume => Ok("consume".into()),
            TokenKind::Suspend => Ok("suspend".into()),
            TokenKind::Pub => Ok("pub".into()),
            TokenKind::Type => Ok("type".into()),
            TokenKind::Enum => Ok("enum".into()),
            _ => Err(FosterError::new(message, token.line, token.column)),
        }
    }
    pub(super) fn expect_member_ident(&mut self, message: &str) -> Result<String, FosterError> {
        if self.take(&TokenKind::Not) {
            Ok("not".into())
        } else {
            self.expect_ident(message)
        }
    }
    pub(super) fn expect_type_binding(&mut self, message: &str) -> Result<String, FosterError> {
        let name = self.expect_ident(message)?;
        if name == "Self" {
            return Err(self.error("`Self` is reserved for the implementing type"));
        }
        Ok(name)
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
