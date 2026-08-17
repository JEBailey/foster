use super::resolution::qualified_path;
use super::*;

impl FunctionLowerer<'_> {
    pub(super) fn lower_function(&mut self, source: &ast::Function) -> Result<(), FosterError> {
        let mut parameters = Vec::new();
        let mut parameter_names = std::collections::HashSet::new();
        for parameter in &source.parameters {
            if !parameter_names.insert(parameter.name.as_str()) {
                return Err(self.error(format!(
                    "function `{}` has more than one parameter named `{}`",
                    source.name, parameter.name
                )));
            }
            let local = self.hir.locals.alloc(Local {
                span: source.span.clone(),
                function: self.function,
                name: parameter.name.clone(),
                kind: LocalKind::Parameter,
            });
            self.locals.insert(parameter.name.clone(), local);
            parameters.push(local);
        }

        let mut body = Vec::new();
        for statement in &source.body {
            body.push(self.lower_statement(statement)?);
        }
        self.hir.functions[self.function].parameters = parameters;
        self.hir.functions[self.function].body = body;
        self.hir.functions[self.function].statement_spans = source.statement_spans.clone();
        Ok(())
    }

    fn lower_statement(&mut self, statement: &ast::Stmt) -> Result<Stmt, FosterError> {
        match statement {
            ast::Stmt::Return { value, guard } => Ok(Stmt::Return {
                value: self.lower_expression(value)?,
                guard: guard
                    .as_ref()
                    .map(|guard| self.lower_expression(guard))
                    .transpose()?,
            }),
            ast::Stmt::Bind { name, value } => {
                let value = self.lower_expression(value)?;
                if let Some(local) = self.locals.get(name).copied() {
                    if self.hir.locals[local].function != self.function
                        && !self.captures.contains(&local)
                    {
                        self.captures.push(local);
                    }
                    return Ok(Stmt::Assign { local, value });
                }
                if self.hir.constant_named(self.module, name).is_some() {
                    return Err(self.error(format!("cannot assign to constant `{name}`")));
                }
                let local = self.hir.locals.alloc(Local {
                    span: self.hir.functions[self.function].span.clone(),
                    function: self.function,
                    name: name.clone(),
                    kind: LocalKind::Binding,
                });
                self.locals.insert(name.clone(), local);
                Ok(Stmt::Bind { local, value })
            }
            ast::Stmt::Function(source) => {
                if source.name.contains('.') {
                    return Err(
                        self.error("associated function declarations must be at module scope")
                    );
                }
                let local = self.hir.locals.alloc(Local {
                    span: source.span.clone(),
                    function: self.function,
                    name: source.name.clone(),
                    kind: LocalKind::Binding,
                });
                self.locals.insert(source.name.clone(), local);
                let value = self.lower_closure(ClosureSource {
                    name: &source.name,
                    parameters: &source.parameters,
                    return_type: source.return_type.clone(),
                    body: ast::ClosureBody::Block(source.body.clone()),
                    captures: &[],
                    named: true,
                    effects: &source.effects,
                    suspends: source.suspends,
                })?;
                Ok(Stmt::Bind { local, value })
            }
            ast::Stmt::Set { place, value } => {
                if !matches!(
                    place.unspanned(),
                    ast::Expr::Name(_) | ast::Expr::Member { .. } | ast::Expr::Index { .. }
                ) {
                    return Err(self.error("left side of assignment is not a place"));
                }
                Ok(Stmt::Set {
                    place: self.lower_expression(place)?,
                    value: self.lower_expression(value)?,
                })
            }
            ast::Stmt::Expr(expression) => Ok(Stmt::Expr(self.lower_expression(expression)?)),
        }
    }

    fn lower_expression(&mut self, expression: &ast::Expr) -> Result<ExprId, FosterError> {
        if let ast::Expr::Spanned { expression, span } = expression {
            let lowered = self.lower_expression(expression)?;
            self.hir.expression_spans.insert(lowered, span.clone());
            return Ok(lowered);
        }
        if let Some(path) = qualified_path(expression)
            && path.len() == 2
            && let Some(variant) = self.resolve_variant_constructor(path[0], path[1])?
        {
            return Ok(self.alloc_expression(Expr::Name(ResolvedName::Variant(variant))));
        }
        if let Some(path) = qualified_path(expression)
            && let Some(function) = self.resolve_associated_function(&path)?
        {
            return Ok(self.alloc_expression(Expr::Name(ResolvedName::Function(function))));
        }
        if let Some(path) = qualified_path(expression)
            && self.imports.contains_key(path[0])
        {
            let name = self.resolve_qualified(&path)?;
            return Ok(self.alloc_expression(Expr::Name(name)));
        }

        let expression = match expression {
            ast::Expr::Spanned { .. } => unreachable!("spans are removed before HIR lowering"),
            ast::Expr::Unit => Expr::Unit,
            ast::Expr::Bool(value) => Expr::Bool(*value),
            ast::Expr::Integer(value) => Expr::Integer(*value),
            ast::Expr::Float(value) => Expr::Float(*value),
            ast::Expr::String(value) => Expr::String(value.clone()),
            ast::Expr::CodePoint(value) => Expr::CodePoint(value.clone()),
            ast::Expr::Symbol(value) => Expr::Symbol(value.clone()),
            ast::Expr::List(items) => Expr::List(
                items
                    .iter()
                    .map(|item| self.lower_expression(item))
                    .collect::<Result<_, _>>()?,
            ),
            ast::Expr::Name(name) => Expr::Name(self.resolve_name(name)?),
            ast::Expr::Call { callee, arguments } => Expr::Call {
                callee: self.lower_expression(callee)?,
                arguments: arguments
                    .iter()
                    .map(|argument| self.lower_expression(argument))
                    .collect::<Result<_, _>>()?,
            },
            ast::Expr::Member { object, name } => Expr::Member {
                object: self.lower_expression(object)?,
                name: name.clone(),
            },
            ast::Expr::Index { object, index } => Expr::Index {
                object: self.lower_expression(object)?,
                index: self.lower_expression(index)?,
            },
            ast::Expr::Reference(place) => Expr::Reference(self.lower_expression(place)?),
            ast::Expr::MoveOut(place) => {
                if !matches!(
                    place.unspanned(),
                    ast::Expr::Name(_) | ast::Expr::Member { .. } | ast::Expr::Index { .. }
                ) {
                    return Err(self.error("`move` requires a place expression"));
                }
                Expr::MoveOut(self.lower_expression(place)?)
            }
            ast::Expr::Remote(value) => Expr::Remote(self.lower_expression(value)?),
            ast::Expr::Await(future) => Expr::Await(self.lower_expression(future)?),
            ast::Expr::Record {
                constructor,
                fields,
            } => {
                let path = qualified_path(constructor)
                    .ok_or_else(|| self.error("record constructor must be a type name"))?;
                let resolved = if path.len() == 1 {
                    self.resolve_name(path[0])?
                } else {
                    self.resolve_qualified(&path)?
                };
                let ResolvedName::Record(record) = resolved else {
                    return Err(self.error("record constructor must name a record type"));
                };
                Expr::Record {
                    record,
                    fields: fields
                        .iter()
                        .map(|field| Ok((field.name.clone(), self.lower_expression(&field.value)?)))
                        .collect::<Result<_, FosterError>>()?,
                }
            }
            ast::Expr::Unary { operator, operand } => Expr::Unary {
                operator: *operator,
                operand: self.lower_expression(operand)?,
            },
            ast::Expr::Binary {
                left,
                operator,
                right,
            } => Expr::Binary {
                left: self.lower_expression(left)?,
                operator: *operator,
                right: self.lower_expression(right)?,
            },
            ast::Expr::Branch { subject, arms } => {
                let subject = subject
                    .as_ref()
                    .map(|s| self.lower_expression(s))
                    .transpose()?;
                let mut lowered = Vec::new();
                for arm in arms {
                    let locals = self.locals.clone();
                    let test = match &arm.test {
                        ast::BranchTest::Condition(e) => {
                            BranchTest::Condition(self.lower_expression(e)?)
                        }
                        ast::BranchTest::Wildcard => BranchTest::Wildcard,
                        ast::BranchTest::Pattern(p) => BranchTest::Pattern(self.lower_pattern(p)?),
                    };
                    lowered.push(BranchArm {
                        test,
                        value: self.lower_expression(&arm.value)?,
                    });
                    self.locals = locals;
                }
                Expr::Branch {
                    subject,
                    arms: lowered,
                }
            }
            ast::Expr::Closure {
                captures,
                parameters,
                effects,
                suspends,
                body,
            } => {
                return self.lower_closure(ClosureSource {
                    name: "closure",
                    parameters,
                    return_type: None,
                    body: body.clone(),
                    captures,
                    named: false,
                    effects,
                    suspends: *suspends,
                });
            }
            ast::Expr::Placeholder => {
                return Err(self.error("placeholder `_` is only valid as a call argument"));
            }
        };
        Ok(self.alloc_expression(expression))
    }

    pub(super) fn alloc_expression(&mut self, expression: Expr) -> ExprId {
        let span = self.hir.functions[self.function].span.clone();
        let id = self.hir.expressions.alloc(expression);
        self.hir.expression_spans.insert(id, span);
        self.hir.expression_functions.insert(id, self.function);
        id
    }

    fn lower_pattern(&mut self, pattern: &ast::Pattern) -> Result<Pattern, FosterError> {
        if let ast::Pattern::Spanned { pattern, span } = pattern {
            let lowered = self.lower_pattern(pattern)?;
            if let Pattern::Binding(local) = lowered.unspanned() {
                self.hir.locals[*local].span = span.clone();
            }
            return Ok(Pattern::Spanned {
                pattern: Box::new(lowered),
                span: span.clone(),
            });
        }

        Ok(match pattern {
            ast::Pattern::Wildcard => Pattern::Wildcard,
            ast::Pattern::Binding(name) => {
                let local = self.hir.locals.alloc(Local {
                    span: self.hir.functions[self.function].span.clone(),
                    function: self.function,
                    name: name.clone(),
                    kind: LocalKind::Binding,
                });
                self.locals.insert(name.clone(), local);
                Pattern::Binding(local)
            }
            ast::Pattern::Bool(v) => Pattern::Bool(*v),
            ast::Pattern::Integer(v) => Pattern::Integer(*v),
            ast::Pattern::Float(v) => Pattern::Float(*v),
            ast::Pattern::String(v) => Pattern::String(v.clone()),
            ast::Pattern::CodePoint(v) => Pattern::CodePoint(v.clone()),
            ast::Pattern::Symbol(v) => Pattern::Symbol(v.clone()),
            ast::Pattern::Variant { path, fields } => {
                let variant = self.resolve_variant(path)?;
                Pattern::Variant {
                    variant,
                    fields: fields
                        .iter()
                        .map(|p| self.lower_pattern(p))
                        .collect::<Result<_, _>>()?,
                }
            }
            ast::Pattern::Spanned { .. } => unreachable!("spanned patterns are handled above"),
        })
    }
}
