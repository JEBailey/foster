use super::*;

impl Parser {
    pub(super) fn expression(&mut self) -> Result<Expr, FosterError> {
        self.logical_or()
    }

    fn logical_or(&mut self) -> Result<Expr, FosterError> {
        self.logical_level(Self::logical_and, &TokenKind::DoublePipe, LogicalOp::Or)
    }

    fn logical_and(&mut self) -> Result<Expr, FosterError> {
        self.logical_level(Self::equality, &TokenKind::DoubleAmpersand, LogicalOp::And)
    }

    fn logical_level(
        &mut self,
        operand: fn(&mut Self) -> Result<Expr, FosterError>,
        token: &TokenKind,
        operator: LogicalOp,
    ) -> Result<Expr, FosterError> {
        let mut expr = operand(self)?;
        while self.take(token) {
            let start = expr.span().map_or(0, |span| span.start);
            let right = operand(self)?;
            expr = self.spanned(
                start,
                Expr::Logical {
                    left: Box::new(expr),
                    operator,
                    right: Box::new(right),
                },
            );
        }
        Ok(expr)
    }

    pub(super) fn equality(&mut self) -> Result<Expr, FosterError> {
        let mut expr = self.bit_or()?;
        while let Some(operator) = if self.take(&TokenKind::EqualEqual) {
            Some(BinaryOp::Equal)
        } else if self.take(&TokenKind::BangEqual) {
            Some(BinaryOp::NotEqual)
        } else {
            None
        } {
            let start = expr.span().map_or(0, |span| span.start);
            let right = self.bit_or()?;
            expr = self.spanned(
                start,
                Expr::Binary {
                    left: Box::new(expr),
                    operator,
                    right: Box::new(right),
                },
            );
        }
        Ok(expr)
    }

    fn bit_or(&mut self) -> Result<Expr, FosterError> {
        self.binary_level(Self::bit_xor, &TokenKind::Pipe, BinaryOp::BitOr)
    }

    fn bit_xor(&mut self) -> Result<Expr, FosterError> {
        self.binary_level(Self::bit_and, &TokenKind::Caret, BinaryOp::BitXor)
    }

    fn bit_and(&mut self) -> Result<Expr, FosterError> {
        self.binary_level(Self::comparison, &TokenKind::Ampersand, BinaryOp::BitAnd)
    }

    fn binary_level(
        &mut self,
        operand: fn(&mut Self) -> Result<Expr, FosterError>,
        token: &TokenKind,
        operator: BinaryOp,
    ) -> Result<Expr, FosterError> {
        let mut expr = operand(self)?;
        while self.take(token) {
            let start = expr.span().map_or(0, |span| span.start);
            let right = operand(self)?;
            expr = self.spanned(
                start,
                Expr::Binary {
                    left: Box::new(expr),
                    operator,
                    right: Box::new(right),
                },
            );
        }
        Ok(expr)
    }

    pub(super) fn comparison(&mut self) -> Result<Expr, FosterError> {
        let mut expr = self.shift()?;
        loop {
            let operator = if self.take(&TokenKind::Less) {
                Some(BinaryOp::Less)
            } else if self.take(&TokenKind::LessEqual) {
                Some(BinaryOp::LessEqual)
            } else if self.take(&TokenKind::Greater) {
                Some(BinaryOp::Greater)
            } else if self.take(&TokenKind::GreaterEqual) {
                Some(BinaryOp::GreaterEqual)
            } else {
                None
            };
            let Some(operator) = operator else { break };
            let start = expr.span().map_or(0, |span| span.start);
            let right = self.shift()?;
            expr = self.spanned(
                start,
                Expr::Binary {
                    left: Box::new(expr),
                    operator,
                    right: Box::new(right),
                },
            );
        }
        Ok(expr)
    }

    fn shift(&mut self) -> Result<Expr, FosterError> {
        let mut expr = self.term()?;
        loop {
            let operator = if self.at(&TokenKind::Less)
                && self
                    .tokens
                    .get(self.current + 1)
                    .is_some_and(|token| token.kind == TokenKind::Less)
            {
                self.take(&TokenKind::Less);
                self.take(&TokenKind::Less);
                Some(BinaryOp::ShiftLeft)
            } else if self.at(&TokenKind::Greater)
                && self
                    .tokens
                    .get(self.current + 1)
                    .is_some_and(|token| token.kind == TokenKind::Greater)
            {
                self.take(&TokenKind::Greater);
                self.take(&TokenKind::Greater);
                Some(BinaryOp::ShiftRight)
            } else {
                None
            };
            let Some(operator) = operator else { break };
            let start = expr.span().map_or(0, |span| span.start);
            let right = self.term()?;
            expr = self.spanned(
                start,
                Expr::Binary {
                    left: Box::new(expr),
                    operator,
                    right: Box::new(right),
                },
            );
        }
        Ok(expr)
    }

    pub(super) fn term(&mut self) -> Result<Expr, FosterError> {
        let mut expr = self.factor()?;
        loop {
            let operator = if self.take(&TokenKind::Plus) {
                Some(BinaryOp::Add)
            } else if self.take(&TokenKind::Minus) {
                Some(BinaryOp::Subtract)
            } else {
                None
            };
            let Some(operator) = operator else { break };
            let start = expr.span().map_or(0, |span| span.start);
            let right = self.factor()?;
            expr = self.spanned(
                start,
                Expr::Binary {
                    left: Box::new(expr),
                    operator,
                    right: Box::new(right),
                },
            );
        }
        Ok(expr)
    }

    pub(super) fn factor(&mut self) -> Result<Expr, FosterError> {
        let mut expr = self.unary()?;
        loop {
            let operator = if self.take(&TokenKind::Star) {
                Some(BinaryOp::Multiply)
            } else if self.take(&TokenKind::Slash) {
                Some(BinaryOp::Divide)
            } else {
                None
            };
            let Some(operator) = operator else { break };
            let start = expr.span().map_or(0, |span| span.start);
            let right = self.unary()?;
            expr = self.spanned(
                start,
                Expr::Binary {
                    left: Box::new(expr),
                    operator,
                    right: Box::new(right),
                },
            );
        }
        Ok(expr)
    }

    pub(super) fn unary(&mut self) -> Result<Expr, FosterError> {
        let start = self.peek().range.start;
        if self.take(&TokenKind::Move) {
            let operand = self.postfix()?;
            return Ok(self.spanned(start, Expr::MoveOut(Box::new(operand))));
        }
        if self.take(&TokenKind::Minus) {
            let operand = self.unary()?;
            return Ok(self.spanned(
                start,
                Expr::Unary {
                    operator: UnaryOp::Negate,
                    operand: Box::new(operand),
                },
            ));
        }
        if self.take(&TokenKind::Bang) || self.take(&TokenKind::Not) {
            let operand = self.unary()?;
            return Ok(self.spanned(
                start,
                Expr::Unary {
                    operator: UnaryOp::Not,
                    operand: Box::new(operand),
                },
            ));
        }
        if self.take(&TokenKind::Tilde) {
            let operand = self.unary()?;
            return Ok(self.spanned(
                start,
                Expr::Unary {
                    operator: UnaryOp::BitNot,
                    operand: Box::new(operand),
                },
            ));
        }
        self.postfix()
    }

    pub(super) fn postfix(&mut self) -> Result<Expr, FosterError> {
        let mut expr = self.primary()?;
        loop {
            let start = expr.span().map_or(0, |span| span.start);
            if self.take(&TokenKind::LParen) {
                let mut arguments = Vec::new();
                if !self.at(&TokenKind::RParen) {
                    loop {
                        arguments.push(self.expression()?);
                        if !self.take(&TokenKind::Comma) {
                            break;
                        }
                    }
                }
                self.expect(&TokenKind::RParen, "expected `)` after arguments")?;
                let call = Expr::Call {
                    callee: Box::new(expr),
                    arguments,
                };
                let call = self.desugar_partial_application(call);
                expr = self.spanned(start, call);
            } else if self.take(&TokenKind::Dot) {
                let name = self.expect_member_ident("expected member name after `.`")?;
                expr = self.spanned(
                    start,
                    Expr::Member {
                        object: Box::new(expr),
                        name,
                    },
                );
            } else if self.take(&TokenKind::DoubleColon) {
                let name = self.expect_ident("expected qualified name after `::`")?;
                expr = self.spanned(
                    start,
                    Expr::Qualified {
                        namespace: Box::new(expr),
                        name,
                    },
                );
            } else if self.take(&TokenKind::LBracket) {
                let index = self.expression()?;
                self.expect(&TokenKind::RBracket, "expected `]` after index")?;
                expr = self.spanned(
                    start,
                    Expr::Index {
                        object: Box::new(expr),
                        index: Box::new(index),
                    },
                );
            } else if !self.suppress_record_literal && self.take(&TokenKind::LBrace) {
                let mut fields = Vec::new();
                self.newlines();
                while !self.at(&TokenKind::RBrace) && !self.at(&TokenKind::Eof) {
                    let name = self.expect_ident("expected record field name")?;
                    let (value, shorthand) = if self.take(&TokenKind::Colon) {
                        (self.expression()?, false)
                    } else {
                        (
                            self.spanned(
                                self.tokens[self.current.saturating_sub(1)].range.start,
                                Expr::Name(name.clone()),
                            ),
                            true,
                        )
                    };
                    fields.push(RecordFieldValue { name, value });
                    if !self.take(&TokenKind::Comma)
                        && !self.at(&TokenKind::RBrace)
                        && !(shorthand && matches!(self.peek().kind, TokenKind::Ident(_)))
                    {
                        self.expect(
                            &TokenKind::Newline,
                            "expected newline between record fields",
                        )?;
                    }
                    self.newlines();
                }
                self.expect(&TokenKind::RBrace, "expected `}` after record fields")?;
                expr = self.spanned(
                    start,
                    Expr::Record {
                        constructor: Box::new(expr),
                        fields,
                    },
                );
            } else {
                break;
            }
        }
        Ok(expr)
    }

    pub(super) fn primary(&mut self) -> Result<Expr, FosterError> {
        let token = self.advance().clone();
        let start = token.range.start;
        let expression = match token.kind {
            TokenKind::True => Expr::Bool(true),
            TokenKind::False => Expr::Bool(false),
            TokenKind::Integer(value) => Expr::Integer(value),
            TokenKind::Float(value) => Expr::Float(value),
            TokenKind::String(value) => Expr::String(value),
            TokenKind::CodePoint(value) => Expr::CodePoint(value),
            TokenKind::Symbol(value) => Expr::Symbol(value),
            TokenKind::Ident(name) if name == "_" => Expr::Placeholder,
            TokenKind::Ident(name) => Expr::Name(name),
            TokenKind::LParen => {
                if self.closure_follows_lparen() {
                    self.closure()?
                } else if self.take(&TokenKind::RParen) {
                    Expr::Unit
                } else {
                    let expr = self.expression()?;
                    self.expect(&TokenKind::RParen, "expected `)`")?;
                    return Ok(self.spanned(start, expr));
                }
            }
            TokenKind::LBracket => {
                if matches!(
                    self.peek().kind,
                    TokenKind::Copy | TokenKind::Move | TokenKind::Ref
                ) {
                    self.captured_closure()?
                } else {
                    let mut items = Vec::new();
                    if !self.at(&TokenKind::RBracket) {
                        loop {
                            items.push(self.expression()?);
                            if !self.take(&TokenKind::Comma) {
                                break;
                            }
                        }
                    }
                    self.expect(&TokenKind::RBracket, "expected `]`")?;
                    Expr::List(items)
                }
            }
            TokenKind::Branch => self.branch()?,
            TokenKind::Ref => Expr::Reference(Box::new(self.postfix()?)),
            TokenKind::Remote => Expr::Remote(Box::new(self.postfix()?)),
            TokenKind::Await => Expr::Await(Box::new(self.unary()?)),
            TokenKind::Try => Expr::Try(Box::new(self.unary()?)),
            _ => {
                return Err(FosterError::new(
                    "expected expression",
                    token.line,
                    token.column,
                ));
            }
        };
        Ok(self.spanned(start, expression))
    }

    pub(super) fn closure_follows_lparen(&self) -> bool {
        let mut depth = 1usize;
        let mut index = self.current;
        while let Some(token) = self.tokens.get(index) {
            match token.kind {
                TokenKind::LParen => depth += 1,
                TokenKind::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        return self
                            .tokens
                            .get(index + 1)
                            .is_some_and(|next| next.kind == TokenKind::Arrow);
                    }
                }
                TokenKind::Eof => return false,
                _ => {}
            }
            index += 1;
        }
        false
    }

    pub(super) fn closure(&mut self) -> Result<Expr, FosterError> {
        let mut parameters = Vec::new();
        if !self.at(&TokenKind::RParen) {
            loop {
                parameters.push(self.parameter("expected closure parameter name")?);
                if !self.take(&TokenKind::Comma) {
                    break;
                }
            }
        }
        self.expect(&TokenKind::RParen, "expected `)` after closure parameters")?;
        self.expect(&TokenKind::Arrow, "expected `->` after closure parameters")?;
        let ParsedEffects {
            explicit: _,
            effects,
            suspend_span,
            ..
        } = self.effects()?;
        self.newlines();
        let body = if self.at(&TokenKind::LBrace) {
            ClosureBody::Block(self.block()?)
        } else {
            ClosureBody::Expression(Box::new(self.expression()?))
        };
        Ok(Expr::Closure {
            captures: Vec::new(),
            parameters,
            effects,
            suspends: suspend_span.is_some(),
            body,
        })
    }

    pub(super) fn captured_closure(&mut self) -> Result<Expr, FosterError> {
        let mut captures = Vec::new();
        loop {
            let mode = match self.advance().kind {
                TokenKind::Copy => CaptureMode::Copy,
                TokenKind::Move => CaptureMode::Move,
                TokenKind::Ref => CaptureMode::Ref,
                _ => return Err(self.error("expected `copy`, `move`, or `ref`")),
            };
            let name = self.expect_ident("expected captured name")?;
            captures.push(CaptureSpec { mode, name });
            if !self.take(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(&TokenKind::RBracket, "expected `]` after capture clause")?;
        self.expect(&TokenKind::LParen, "expected `(` after capture clause")?;
        let Expr::Closure {
            parameters,
            effects,
            suspends,
            body,
            ..
        } = self.closure()?
        else {
            unreachable!()
        };
        Ok(Expr::Closure {
            captures,
            parameters,
            effects,
            suspends,
            body,
        })
    }

    pub(super) fn desugar_partial_application(&self, call: Expr) -> Expr {
        let Expr::Call { callee, arguments } = call else {
            unreachable!()
        };
        if !arguments
            .iter()
            .any(|argument| matches!(argument.unspanned(), Expr::Placeholder))
        {
            return Expr::Call { callee, arguments };
        }
        Expr::PartialApplication { callee, arguments }
    }

    pub(super) fn branch(&mut self) -> Result<Expr, FosterError> {
        self.newlines();
        let subject = if self.at(&TokenKind::LBrace) {
            None
        } else {
            self.suppress_record_literal = true;
            let parsed = self.expression();
            self.suppress_record_literal = false;
            Some(Box::new(parsed?))
        };
        self.expect(&TokenKind::LBrace, "expected `{` after `branch`")?;
        self.newlines();
        let mut arms = Vec::new();
        while !self.at(&TokenKind::RBrace) && !self.at(&TokenKind::Eof) {
            let test = if subject.is_some() {
                BranchTest::Pattern(self.pattern()?)
            } else if self.take_ident("_") {
                BranchTest::Wildcard
            } else {
                BranchTest::Condition(self.expression()?)
            };
            self.expect(&TokenKind::Arrow, "expected `->` in branch arm")?;
            self.newlines();
            let body = if self.at(&TokenKind::LBrace) {
                self.block()?
            } else {
                let value = self.expression()?;
                let span = value.span().unwrap_or(0..0);
                crate::block::Block::single(Stmt::Expr(value), span)
            };
            arms.push(BranchArm { test, body });
            self.newlines();
        }
        self.expect(&TokenKind::RBrace, "expected `}` after branch arms")?;
        Ok(Expr::Branch { subject, arms })
    }

    pub(super) fn pattern(&mut self) -> Result<Pattern, FosterError> {
        let token = self.advance().clone();
        let start = token.range.start;
        let pattern = match token.kind {
            TokenKind::Ident(name) if name == "_" => Pattern::Wildcard,
            TokenKind::True => Pattern::Bool(true),
            TokenKind::False => Pattern::Bool(false),
            TokenKind::Integer(value) => Pattern::Integer(value),
            TokenKind::Float(value) => Pattern::Float(value),
            TokenKind::String(value) => Pattern::String(value),
            TokenKind::CodePoint(value) => Pattern::CodePoint(value),
            TokenKind::Symbol(value) => Pattern::Symbol(value),
            TokenKind::Ident(first) => {
                let mut path = vec![first];
                while self.take(&TokenKind::DoubleColon) {
                    path.push(self.expect_ident("expected name after `::` in pattern")?);
                }
                let enum_accessor = self.take(&TokenKind::Dot);
                if enum_accessor {
                    path.push(self.expect_member_ident("expected enum case name after `.`")?);
                    if self.at(&TokenKind::Dot) || self.at(&TokenKind::DoubleColon) {
                        return Err(self.error("an enum pattern ends with one `.Case` accessor"));
                    }
                }
                if path.len() > 1 || self.at(&TokenKind::LParen) {
                    let mut fields = Vec::new();
                    if self.take(&TokenKind::LParen) {
                        if !self.at(&TokenKind::RParen) {
                            loop {
                                fields.push(self.pattern()?);
                                if !self.take(&TokenKind::Comma) {
                                    break;
                                }
                            }
                        }
                        self.expect(&TokenKind::RParen, "expected `)` after enum pattern")?;
                    }
                    Pattern::Variant {
                        path,
                        enum_accessor,
                        fields,
                    }
                } else {
                    Pattern::Binding(path.remove(0))
                }
            }
            _ => {
                return Err(FosterError::new(
                    "expected pattern",
                    token.line,
                    token.column,
                ));
            }
        };
        Ok(self.spanned_pattern(start, pattern))
    }
}
