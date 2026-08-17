use super::*;

impl Parser {
    pub(super) fn program(&mut self) -> Result<Program, FosterError> {
        let mut imports = Vec::new();
        let mut constants = Vec::new();
        let mut records = Vec::new();
        let mut variants = Vec::new();
        let mut functions = Vec::new();
        self.newlines();
        let mut documentation = self.documentation();
        while self.at(&TokenKind::Import) {
            imports.push(self.import()?);
            if !self.at(&TokenKind::Eof) {
                self.expect(&TokenKind::Newline, "expected newline after import")?;
                self.newlines();
            }
            documentation = self.documentation();
        }
        while !self.at(&TokenKind::Eof) {
            if self.at(&TokenKind::Const)
                || (self.at(&TokenKind::Pub)
                    && self
                        .peek_n(1)
                        .is_some_and(|token| token.kind == TokenKind::Const))
            {
                constants.push(self.constant(documentation.take())?);
            } else if self.at(&TokenKind::Type)
                || (self.at(&TokenKind::Pub)
                    && self
                        .peek_n(1)
                        .is_some_and(|token| token.kind == TokenKind::Type))
            {
                let (record, variant) = self.type_decl(documentation.take())?;
                records.extend(record);
                variants.extend(variant);
            } else {
                functions.push(self.function(documentation.take())?);
            }
            self.newlines();
            documentation = self.documentation();
        }
        if documentation.is_some() {
            return Err(self.error("documentation comment must precede a declaration"));
        }
        Ok(Program {
            imports,
            constants,
            records,
            variants,
            functions,
        })
    }

    fn constant(&mut self, documentation: Option<String>) -> Result<ConstDecl, FosterError> {
        let start = self.peek().range.start;
        let public = self.take(&TokenKind::Pub);
        self.expect(&TokenKind::Const, "expected `const`")?;
        let name = self.expect_ident("expected constant name")?;
        self.expect(&TokenKind::Equal, "expected `=` after constant name")?;
        let value = self.expression()?;
        Ok(ConstDecl {
            span: start..self.tokens[self.current.saturating_sub(1)].range.end,
            documentation,
            name,
            public,
            value,
        })
    }

    pub(super) fn type_decl(
        &mut self,
        documentation: Option<String>,
    ) -> Result<(Option<RecordDecl>, Option<VariantDecl>), FosterError> {
        let start = self.peek().range.start;
        let public = self.take(&TokenKind::Pub);
        self.expect(&TokenKind::Type, "expected `type`")?;
        let name = self.expect_ident("expected record name")?;
        let mut parameters = Vec::new();
        if self.take(&TokenKind::Less) {
            loop {
                parameters.push(self.expect_ident("expected type parameter")?);
                if !self.take(&TokenKind::Comma) {
                    break;
                }
            }
            self.expect(&TokenKind::Greater, "expected `>` after type parameters")?;
        }
        self.newlines();
        if self.take(&TokenKind::Equal) {
            self.newlines();
            let mut alternatives = Vec::new();
            while self.take(&TokenKind::Pipe) {
                let alternative = self.expect_ident("expected variant name after `|`")?;
                let mut payload = Vec::new();
                if self.take(&TokenKind::LParen) {
                    if !self.at(&TokenKind::RParen) {
                        loop {
                            payload.push(self.type_expr()?);
                            if !self.take(&TokenKind::Comma) {
                                break;
                            }
                        }
                    }
                    self.expect(&TokenKind::RParen, "expected `)` after variant payload")?;
                }
                alternatives.push(VariantAlternative {
                    name: alternative,
                    payload,
                });
                self.newlines();
            }
            if alternatives.is_empty() {
                return Err(self.error("variant type requires at least one `|` alternative"));
            }
            return Ok((
                None,
                Some(VariantDecl {
                    span: start..self.tokens[self.current.saturating_sub(1)].range.end,
                    documentation,
                    name,
                    public,
                    parameters,
                    alternatives,
                }),
            ));
        }
        self.expect(&TokenKind::LBrace, "expected `{` in record declaration")?;
        self.newlines();
        let mut fields = Vec::new();
        while !self.at(&TokenKind::RBrace) && !self.at(&TokenKind::Eof) {
            let public = self.take(&TokenKind::Pub);
            let name = self.expect_ident("expected field name")?;
            self.expect(&TokenKind::Colon, "expected `:` after field name")?;
            let ty = self.type_expr()?;
            fields.push(RecordField { name, public, ty });
            if !self.take(&TokenKind::Comma) && !self.at(&TokenKind::RBrace) {
                self.expect(&TokenKind::Newline, "expected newline between fields")?;
            }
            self.newlines();
        }
        self.expect(&TokenKind::RBrace, "expected `}` after record fields")?;
        Ok((
            Some(RecordDecl {
                span: start..self.tokens[self.current.saturating_sub(1)].range.end,
                documentation,
                name,
                public,
                parameters,
                fields,
            }),
            None,
        ))
    }

    pub(super) fn import(&mut self) -> Result<Import, FosterError> {
        let start = self.peek().range.start;
        self.expect(&TokenKind::Import, "expected `import`")?;
        let mut path = vec![self.expect_ident("expected module name after `import`")?];
        while self.take(&TokenKind::Dot) {
            path.push(self.expect_ident("expected module name after `.`")?);
        }
        let alias = if self.take(&TokenKind::As) {
            Some(self.expect_ident("expected alias after `as`")?)
        } else {
            None
        };
        Ok(Import {
            span: start..self.tokens[self.current.saturating_sub(1)].range.end,
            path,
            alias,
        })
    }

    pub(super) fn function(
        &mut self,
        documentation: Option<String>,
    ) -> Result<Function, FosterError> {
        let start = self.peek().range.start;
        let public = self.take(&TokenKind::Pub);
        self.expect(&TokenKind::Func, "expected `func`")?;
        let mut name = self.expect_ident("expected function name")?;
        if self.take(&TokenKind::Dot) {
            let member = self.expect_ident("expected associated function name after `.`")?;
            name.push('.');
            name.push_str(&member);
            if self.at(&TokenKind::Dot) {
                return Err(
                    self.error("associated function declarations accept one type qualifier")
                );
            }
        }
        let (type_parameters, groups) = self.function_parameters()?;
        self.expect(&TokenKind::LParen, "expected `(` after function name")?;
        let mut parameters = Vec::new();
        if !self.at(&TokenKind::RParen) {
            loop {
                let name = self.expect_ident("expected parameter name")?;
                let ty = if self.take(&TokenKind::Colon) {
                    Some(self.type_expr()?)
                } else {
                    None
                };
                parameters.push(Parameter { name, ty });
                if !self.take(&TokenKind::Comma) {
                    break;
                }
            }
        }
        self.expect(&TokenKind::RParen, "expected `)` after parameters")?;
        self.newlines();
        let return_type = if self.take(&TokenKind::Arrow) {
            Some(self.type_expr()?)
        } else {
            None
        };
        let ParsedEffects {
            explicit: effects_explicit,
            effects,
            spans: effect_spans,
            suspend_span,
        } = self.effects()?;
        let suspends = suspend_span.is_some();
        self.newlines();
        let (body, statement_spans) = self.block_spanned()?;
        let end = self.tokens[self.current.saturating_sub(1)].range.end;
        Ok(Function {
            span: start..end,
            documentation,
            name,
            public,
            type_parameters,
            groups,
            parameters,
            return_type,
            effects_explicit,
            effects,
            effect_spans,
            suspends,
            suspend_span,
            body,
            statement_spans,
        })
    }

    pub(super) fn function_parameters(
        &mut self,
    ) -> Result<(Vec<String>, Vec<GroupParameter>), FosterError> {
        let mut type_parameters = Vec::new();
        let mut groups = Vec::new();
        if self.take(&TokenKind::Less) {
            loop {
                type_parameters.push(self.expect_ident("expected type parameter")?);
                if !self.take(&TokenKind::Comma) {
                    break;
                }
            }
            self.expect(&TokenKind::Greater, "expected `>` after type parameters")?;
        }
        if self.take(&TokenKind::LBracket) {
            loop {
                let name = self.expect_ident("expected group parameter name")?;
                self.expect(&TokenKind::Colon, "expected `:` after group parameter name")?;
                self.expect(&TokenKind::Group, "expected `group`")?;
                let element = self.type_expr()?;
                groups.push(GroupParameter { name, element });
                if !self.take(&TokenKind::Comma) {
                    break;
                }
            }
            self.expect(&TokenKind::RBracket, "expected `]` after group parameters")?;
        }
        Ok((type_parameters, groups))
    }

    pub(super) fn type_expr(&mut self) -> Result<TypeExpr, FosterError> {
        let first = self.primary_type_expr()?;
        if !self.take(&TokenKind::Ampersand) {
            return Ok(first);
        }
        let mut members = vec![first, self.primary_type_expr()?];
        while self.take(&TokenKind::Ampersand) {
            members.push(self.primary_type_expr()?);
        }
        Ok(TypeExpr::Intersection(members))
    }

    fn primary_type_expr(&mut self) -> Result<TypeExpr, FosterError> {
        let erased = self.take(&TokenKind::Any);
        if erased || self.take(&TokenKind::Func) {
            if erased {
                self.expect(&TokenKind::Func, "expected `func` after `any`")?;
            }
            self.expect(&TokenKind::LParen, "expected `(` in function type")?;
            let mut parameters = Vec::new();
            let mut parameter_modes = Vec::new();
            if !self.at(&TokenKind::RParen) {
                loop {
                    parameter_modes.push(if self.take(&TokenKind::Consume) {
                        crate::ast::ParameterMode::Consume
                    } else {
                        crate::ast::ParameterMode::Borrow
                    });
                    parameters.push(self.type_expr()?);
                    if !self.take(&TokenKind::Comma) {
                        break;
                    }
                }
            }
            self.expect(&TokenKind::RParen, "expected `)` in function type")?;
            self.expect(&TokenKind::Arrow, "expected `->` in function type")?;
            let result = Box::new(self.type_expr()?);
            let ParsedEffects {
                explicit: _,
                effects,
                suspend_span,
                ..
            } = self.effects()?;
            return Ok(TypeExpr::Function {
                erased,
                parameters,
                parameter_modes,
                result,
                effects,
                suspends: suspend_span.is_some(),
            });
        }
        if self.take(&TokenKind::Ref) {
            self.expect(&TokenKind::LBracket, "expected `[` after `ref`")?;
            let group = self.expect_ident("expected reference group")?;
            self.expect(&TokenKind::RBracket, "expected `]` after reference group")?;
            return Ok(TypeExpr::Reference {
                group,
                value: Box::new(self.type_expr()?),
            });
        }
        let mut name = self.expect_ident("expected type name")?;
        while self.take(&TokenKind::Dot) {
            name.push('.');
            name.push_str(&self.expect_ident("expected type name after `.`")?);
        }
        let mut arguments = Vec::new();
        if self.take(&TokenKind::Less) {
            loop {
                arguments.push(self.type_expr()?);
                if !self.take(&TokenKind::Comma) {
                    break;
                }
            }
            self.expect(&TokenKind::Greater, "expected `>` after type arguments")?;
        }
        Ok(TypeExpr::Named(name, arguments))
    }

    pub(super) fn effects(&mut self) -> Result<ParsedEffects, FosterError> {
        let explicit = self.effect_clause_follows();
        if !explicit {
            return Ok(ParsedEffects {
                explicit: false,
                effects: Vec::new(),
                spans: Vec::new(),
                suspend_span: None,
            });
        }
        self.expect(&TokenKind::LBracket, "expected `[` before effects")?;
        self.newlines();
        let mut effects = Vec::new();
        let mut spans = Vec::new();
        let mut suspend_span = None;
        while !self.at(&TokenKind::RBracket) {
            let start = self.peek().range.start;
            let kind = if self.take(&TokenKind::Read) {
                Some(EffectKind::Read)
            } else if self.take(&TokenKind::Mut) {
                Some(EffectKind::Mut)
            } else if self.take(&TokenKind::Reshape) {
                Some(EffectKind::Reshape)
            } else if self.take(&TokenKind::Consume) {
                Some(EffectKind::Consume)
            } else if self.at(&TokenKind::Suspend) {
                if suspend_span.is_some() {
                    return Err(self.error("effect clause contains `suspend` more than once"));
                }
                suspend_span = Some(self.advance().range.clone());
                None
            } else {
                return Err(self.error("expected effect or `suspend`"));
            };
            if let Some(kind) = kind {
                let root = self.expect_ident("expected effect target")?;
                let mut children = Vec::new();
                while self.take(&TokenKind::Dot) {
                    children.push(self.expect_ident("expected effect path component")?);
                }
                effects.push(Effect {
                    kind,
                    target: GroupPath { root, children },
                });
                spans.push(start..self.tokens[self.current.saturating_sub(1)].range.end);
            }
            if !self.take(&TokenKind::Comma) && !self.at(&TokenKind::RBracket) {
                self.expect(
                    &TokenKind::Newline,
                    "expected `,` or newline between effects",
                )?;
            }
            self.newlines();
        }
        self.expect(&TokenKind::RBracket, "expected `]` after effects")?;
        Ok(ParsedEffects {
            explicit,
            effects,
            spans,
            suspend_span,
        })
    }

    fn effect_clause_follows(&self) -> bool {
        self.at(&TokenKind::LBracket)
            && self.peek_n(1).is_some_and(|token| {
                matches!(
                    token.kind,
                    TokenKind::Read
                        | TokenKind::Mut
                        | TokenKind::Reshape
                        | TokenKind::Consume
                        | TokenKind::Suspend
                )
            })
    }

    pub(super) fn block(&mut self) -> Result<Vec<Stmt>, FosterError> {
        self.block_spanned().map(|(statements, _)| statements)
    }

    pub(super) fn block_spanned(
        &mut self,
    ) -> Result<(Vec<Stmt>, Vec<std::ops::Range<usize>>), FosterError> {
        self.expect(&TokenKind::LBrace, "expected `{`")?;
        self.newlines();
        let mut statements = Vec::new();
        let mut spans = Vec::new();
        while !self.at(&TokenKind::RBrace) && !self.at(&TokenKind::Eof) {
            let start = self.peek().range.start;
            statements.push(self.statement()?);
            spans.push(start..self.tokens[self.current.saturating_sub(1)].range.end);
            if !self.at(&TokenKind::RBrace) {
                self.expect(&TokenKind::Newline, "expected a newline between statements")?;
                self.newlines();
            }
        }
        self.expect(&TokenKind::RBrace, "expected `}`")?;
        Ok((statements, spans))
    }

    pub(super) fn statement(&mut self) -> Result<Stmt, FosterError> {
        let documentation = self.documentation();
        if self.at(&TokenKind::Func) || self.at(&TokenKind::Pub) {
            return Ok(Stmt::Function(Box::new(self.function(documentation)?)));
        }
        if documentation.is_some() {
            return Err(self.error("documentation comment must precede a declaration"));
        }
        if self.take(&TokenKind::Return) {
            let value = self.expression()?;
            let guard = if self.take(&TokenKind::If) {
                Some(self.expression()?)
            } else {
                None
            };
            return Ok(Stmt::Return { value, guard });
        }
        if let TokenKind::Ident(name) = self.peek().kind.clone()
            && self.peek_n(1).is_some_and(|t| t.kind == TokenKind::Equal)
        {
            self.advance();
            self.advance();
            return Ok(Stmt::Bind {
                name,
                value: self.expression()?,
            });
        }
        let place = self.expression()?;
        if self.take(&TokenKind::Equal) {
            return Ok(Stmt::Set {
                place,
                value: self.expression()?,
            });
        }
        Ok(Stmt::Expr(place))
    }
}
