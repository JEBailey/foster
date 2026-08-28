use super::*;

impl Parser {
    pub(super) fn program(&mut self) -> Result<Program, FosterError> {
        let mut imports = Vec::new();
        let mut constants = Vec::new();
        let mut records = Vec::new();
        let mut variants = Vec::new();
        let mut functions = Vec::new();
        let mut tests = Vec::new();
        self.newlines();
        let module_documentation = self.module_documentation();
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
            if self.at(&TokenKind::Let) {
                return Err(self.error(
                    "local declarations are only allowed inside function, closure, or test bodies",
                ));
            } else if self.at(&TokenKind::Const)
                || (self.at(&TokenKind::Pub)
                    && self
                        .peek_n(1)
                        .is_some_and(|token| token.kind == TokenKind::Const))
            {
                constants.push(self.constant(documentation.take())?);
            } else if self.at(&TokenKind::Type)
                || self.at(&TokenKind::Enum)
                || self.at(&TokenKind::Intrinsic)
                || (self.at(&TokenKind::Pub)
                    && self.peek_n(1).is_some_and(|token| {
                        matches!(token.kind, TokenKind::Type | TokenKind::Enum)
                    }))
            {
                let (record, variant) = self.type_decl(documentation.take())?;
                records.extend(record);
                variants.extend(variant);
            } else if self.at(&TokenKind::Test) {
                if documentation.is_some() {
                    return Err(
                        self.error("test declarations do not accept documentation comments")
                    );
                }
                tests.push(self.test()?);
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
            documentation: module_documentation,
            imports,
            constants,
            records,
            variants,
            functions,
            tests,
        })
    }

    fn test(&mut self) -> Result<TestDecl, FosterError> {
        let start = self.peek().range.start;
        self.expect(&TokenKind::Test, "expected `test`")?;
        let TokenKind::String(description) = self.peek().kind.clone() else {
            return Err(self.error("expected test description string after `test`"));
        };
        self.current += 1;
        if description.trim().is_empty() {
            return Err(self.error("test description cannot be empty"));
        }
        let body = self.block()?;
        Ok(TestDecl {
            span: start..self.tokens[self.current.saturating_sub(1)].range.end,
            description,
            body,
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
        let intrinsic = self.take(&TokenKind::Intrinsic);
        let public = self.take(&TokenKind::Pub);
        let kind = if self.take(&TokenKind::Enum) {
            VariantKind::Enum
        } else {
            self.expect(&TokenKind::Type, "expected `type` or `enum`")?;
            VariantKind::Union
        };
        if intrinsic && kind == VariantKind::Enum {
            return Err(self.error("an intrinsic declaration must use `type`"));
        }
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
        if intrinsic {
            return Ok((
                Some(RecordDecl {
                    span: start..self.tokens[self.current.saturating_sub(1)].range.end,
                    documentation,
                    name,
                    public,
                    intrinsic: true,
                    parameters,
                    compositions: Vec::new(),
                    fields: Vec::new(),
                    methods: Vec::new(),
                }),
                None,
            ));
        }
        self.expect(&TokenKind::Equal, "expected `=` after type name")?;
        self.newlines();
        if self.at(&TokenKind::Pipe) {
            return Err(self.error(&format!(
                "an {} declaration starts with its first {}; remove the leading `|`",
                if kind == VariantKind::Enum {
                    "enum"
                } else {
                    "union"
                },
                if kind == VariantKind::Enum {
                    "case"
                } else {
                    "member type"
                }
            )));
        }
        if kind == VariantKind::Enum
            || (!self.at(&TokenKind::LBrace) && !self.at(&TokenKind::Ampersand))
        {
            let mut alternatives = Vec::new();
            loop {
                let member_start = self.peek().range.start;
                let alternative = if kind == VariantKind::Enum {
                    let name = self.expect_ident("expected enum case name")?;
                    let payload = if self.take(&TokenKind::LParen) {
                        if self.at(&TokenKind::RParen) {
                            return Err(self.error("a payloadless enum case omits parentheses"));
                        }
                        let payload = self.type_expr()?;
                        if self.at(&TokenKind::Comma) {
                            return Err(self.error(
                                "an enum case carries one payload type; use a record type to carry multiple fields",
                            ));
                        }
                        self.expect(
                            &TokenKind::RParen,
                            "expected `)` after enum case payload type",
                        )?;
                        Some(payload)
                    } else {
                        None
                    };
                    VariantAlternative::EnumCase {
                        span: member_start..self.tokens[self.current.saturating_sub(1)].range.end,
                        name,
                        payload,
                    }
                } else {
                    let ty = self.type_expr()?;
                    let member_end = self.tokens[self.current.saturating_sub(1)].range.end;
                    if self.at(&TokenKind::LParen) {
                        return Err(self.error(
                            "union members are complete types; declare an `enum` for labelled cases",
                        ));
                    }
                    VariantAlternative::UnionMember {
                        span: member_start..member_end,
                        ty,
                    }
                };
                alternatives.push(alternative);
                self.newlines();
                if !self.take(&TokenKind::Pipe) {
                    break;
                }
                self.newlines();
            }
            let mut compositions = Vec::new();
            let mut has_body = false;
            while self.take(&TokenKind::Ampersand) {
                self.newlines();
                if self.take(&TokenKind::LBrace) {
                    has_body = true;
                    break;
                }
                compositions.push(self.primary_type_expr()?);
                if self.at(&TokenKind::Newline) {
                    self.newlines();
                }
            }
            let mut methods = Vec::new();
            if has_body {
                self.newlines();
                while !self.at(&TokenKind::RBrace) && !self.at(&TokenKind::Eof) {
                    let documentation = self.documentation();
                    let method_follows = self.at(&TokenKind::Func)
                        || (self.at(&TokenKind::Pub)
                            && self
                                .peek_n(1)
                                .is_some_and(|token| token.kind == TokenKind::Func));
                    if !method_follows {
                        return Err(self.error(
                            "enum and union shared bodies may only declare required methods",
                        ));
                    }
                    methods.push(self.method_requirement(documentation)?);
                    if !self.at(&TokenKind::RBrace) {
                        self.expect(
                            &TokenKind::Newline,
                            "expected newline after required method signature",
                        )?;
                    }
                    self.newlines();
                }
                self.expect(&TokenKind::RBrace, "expected `}` after shared type body")?;
            }
            return Ok((
                None,
                Some(VariantDecl {
                    span: start..self.tokens[self.current.saturating_sub(1)].range.end,
                    documentation,
                    name,
                    public,
                    kind,
                    parameters,
                    alternatives,
                    compositions,
                    methods,
                }),
            ));
        }
        let mut compositions = Vec::new();
        let mut has_body = self.take(&TokenKind::LBrace);
        while !has_body && self.take(&TokenKind::Ampersand) {
            self.newlines();
            if self.take(&TokenKind::LBrace) {
                has_body = true;
                break;
            }
            compositions.push(self.primary_type_expr()?);
            if self.at(&TokenKind::Newline) {
                self.newlines();
            }
        }
        if !has_body {
            if compositions.is_empty() {
                return Err(self.error("expected `{`, `&`, or `|` after `=` in type declaration"));
            }
            return Ok((
                Some(RecordDecl {
                    span: start..self.tokens[self.current.saturating_sub(1)].range.end,
                    documentation,
                    name,
                    public,
                    intrinsic: false,
                    parameters,
                    compositions,
                    fields: Vec::new(),
                    methods: Vec::new(),
                }),
                None,
            ));
        }
        self.newlines();
        let mut fields = Vec::new();
        let mut methods = Vec::new();
        while !self.at(&TokenKind::RBrace) && !self.at(&TokenKind::Eof) {
            let documentation = self.documentation();
            let method_follows = self.at(&TokenKind::Func)
                || (self.at(&TokenKind::Pub)
                    && self
                        .peek_n(1)
                        .is_some_and(|token| token.kind == TokenKind::Func));
            if method_follows {
                methods.push(self.method_requirement(documentation)?);
                if !self.at(&TokenKind::RBrace) {
                    self.expect(
                        &TokenKind::Newline,
                        "expected newline after required method signature",
                    )?;
                }
                self.newlines();
                continue;
            }
            if documentation.is_some() {
                return Err(self.error("documentation inside a type must precede a method"));
            }
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
                intrinsic: false,
                parameters,
                compositions,
                fields,
                methods,
            }),
            None,
        ))
    }

    fn method_requirement(
        &mut self,
        documentation: Option<String>,
    ) -> Result<MethodRequirement, FosterError> {
        let start = self.peek().range.start;
        let public = self.take(&TokenKind::Pub);
        self.expect(&TokenKind::Func, "expected `func`")?;
        let name = self.expect_ident("expected required method name")?;
        let (type_parameters, groups) = self.function_parameters()?;
        self.expect(
            &TokenKind::LParen,
            "expected `(` after required method name",
        )?;
        let mut parameters = Vec::new();
        if !self.at(&TokenKind::RParen) {
            loop {
                parameters.push(self.parameter("expected parameter name")?);
                if !self.take(&TokenKind::Comma) {
                    break;
                }
            }
        }
        self.expect(
            &TokenKind::RParen,
            "expected `)` after required method parameters",
        )?;
        let receiver = self.receiver_parameter(&parameters)?;
        let return_type = if self.take(&TokenKind::Arrow) {
            Some(self.type_expr()?)
        } else {
            None
        };
        let effects = self.effects()?;
        Ok(MethodRequirement {
            span: start..self.tokens[self.current.saturating_sub(1)].range.end,
            documentation,
            name,
            receiver,
            public,
            type_parameters,
            groups,
            parameters,
            return_type,
            effects: effects.effects,
            suspends: effects.suspend_span.is_some(),
        })
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
        let first_name = self.expect_ident("expected function name")?;
        let mut owner = None;
        let mut name = first_name.clone();
        if self.take(&TokenKind::Dot) {
            let member = self.expect_ident("expected associated function name after `.`")?;
            owner = Some(first_name.clone());
            name = format!("{first_name}.{member}");
            if self.at(&TokenKind::Dot) {
                return Err(
                    self.error("associated function declarations accept one type qualifier")
                );
            }
        } else if self.at(&TokenKind::DoubleColon) {
            return Err(
                self.error("associated function declarations use `.`; replace `::` with `.`")
            );
        }
        let (type_parameters, groups) = self.function_parameters()?;
        self.expect(&TokenKind::LParen, "expected `(` after function name")?;
        let mut parameters = Vec::new();
        if !self.at(&TokenKind::RParen) {
            loop {
                parameters.push(self.parameter("expected parameter name")?);
                if !self.take(&TokenKind::Comma) {
                    break;
                }
            }
        }
        self.expect(&TokenKind::RParen, "expected `)` after parameters")?;
        let receiver = self.receiver_parameter(&parameters)?;
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
        let intrinsic = if self.take(&TokenKind::Equal) {
            self.expect(&TokenKind::Intrinsic, "expected `intrinsic` after `=`")?;
            self.expect(&TokenKind::LParen, "expected `(` after `intrinsic`")?;
            let TokenKind::String(key) = self.peek().kind.clone() else {
                return Err(self.error("expected intrinsic runtime key string"));
            };
            self.current += 1;
            self.expect(
                &TokenKind::RParen,
                "expected `)` after intrinsic runtime key",
            )?;
            Some(key)
        } else {
            None
        };
        let body = if intrinsic.is_some() {
            crate::block::Block::new()
        } else {
            self.block()?
        };
        let end = self.tokens[self.current.saturating_sub(1)].range.end;
        Ok(Function {
            span: start..end,
            documentation,
            name,
            owner,
            receiver,
            public,
            intrinsic,
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
        })
    }

    fn receiver_parameter(&self, parameters: &[Parameter]) -> Result<bool, FosterError> {
        let receiver = parameters
            .first()
            .is_some_and(|parameter| parameter.name == "self");
        if parameters
            .iter()
            .skip(usize::from(receiver))
            .any(|parameter| parameter.name == "self")
        {
            return Err(self.error("`self` is a receiver and must be the first parameter"));
        }
        Ok(receiver)
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

    pub(super) fn parameter(&mut self, expected_name: &str) -> Result<Parameter, FosterError> {
        let start = self.peek().range.start;
        let name = self.expect_ident(expected_name)?;
        let span = start..self.tokens[self.current.saturating_sub(1)].range.end;
        let (ty, type_span) = if self.take(&TokenKind::Colon) {
            let type_start = self.peek().range.start;
            let ty = self.type_expr()?;
            let type_end = self.tokens[self.current.saturating_sub(1)].range.end;
            (Some(ty), Some(type_start..type_end))
        } else {
            (None, None)
        };
        Ok(Parameter {
            span,
            name,
            ty,
            type_span,
        })
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
        if self.take(&TokenKind::LParen) {
            self.expect(
                &TokenKind::RParen,
                "expected `)` to complete the unit type `()`",
            )?;
            return Ok(TypeExpr::Unit);
        }
        if self.take(&TokenKind::Func) {
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
        while self.take(&TokenKind::DoubleColon) {
            name.push('.');
            name.push_str(&self.expect_ident("expected type name after `::`")?);
        }
        if self.at(&TokenKind::Dot) {
            return Err(self.error("qualified type names use `::`; replace `.` with `::`"));
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

    pub(super) fn block(&mut self) -> Result<crate::block::Block<Stmt>, FosterError> {
        self.expect(&TokenKind::LBrace, "expected `{`")?;
        self.newlines();
        let mut statements = crate::block::Block::new();
        while !self.at(&TokenKind::RBrace) && !self.at(&TokenKind::Eof) {
            let start = self.peek().range.start;
            let statement = self.statement()?;
            statements.push(
                statement,
                start..self.tokens[self.current.saturating_sub(1)].range.end,
            );
            if !self.at(&TokenKind::RBrace) {
                self.expect(&TokenKind::Newline, "expected a newline between statements")?;
                self.newlines();
            }
        }
        self.expect(&TokenKind::RBrace, "expected `}`")?;
        Ok(statements)
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
            let guard = self.control_guard()?;
            return Ok(Stmt::Return { value, guard });
        }
        if self.take(&TokenKind::Assert) {
            self.expect(&TokenKind::LParen, "expected `(` after `assert`")?;
            let condition = self.expression()?;
            let message = self
                .take(&TokenKind::Comma)
                .then(|| self.expression())
                .transpose()?;
            self.expect(&TokenKind::RParen, "expected `)` after assertion")?;
            self.reject_value_guard()?;
            return Ok(Stmt::Assert { condition, message });
        }
        if self.take(&TokenKind::Loop) {
            return Ok(Stmt::Loop {
                body: self.block()?,
            });
        }
        if self.take(&TokenKind::Break) {
            return Ok(Stmt::Break {
                guard: self.control_guard()?,
            });
        }
        if self.take(&TokenKind::Continue) {
            return Ok(Stmt::Continue {
                guard: self.control_guard()?,
            });
        }
        if self.take(&TokenKind::Let) {
            let name = self.expect_ident("expected local name after `let`")?;
            self.expect(&TokenKind::Equal, "expected `=` after local name")?;
            let value = self.expression()?;
            self.reject_value_guard()?;
            return Ok(Stmt::Bind { name, value });
        }
        if let TokenKind::Ident(name) = self.peek().kind.clone()
            && self.peek_n(1).is_some_and(|t| t.kind == TokenKind::Equal)
        {
            self.advance();
            self.advance();
            let value = self.expression()?;
            self.reject_value_guard()?;
            return Ok(Stmt::Assign { name, value });
        }
        let place = self.expression()?;
        if self.take(&TokenKind::Equal) {
            let value = self.expression()?;
            self.reject_value_guard()?;
            return Ok(Stmt::Set { place, value });
        }
        self.reject_value_guard()?;
        Ok(Stmt::Expr(place))
    }

    fn control_guard(&mut self) -> Result<Option<Expr>, FosterError> {
        self.take(&TokenKind::If)
            .then(|| self.expression())
            .transpose()
    }

    fn reject_value_guard(&self) -> Result<(), FosterError> {
        if self.at(&TokenKind::If) {
            return Err(self.error("postfix `if` may only guard a control statement"));
        }
        Ok(())
    }
}
