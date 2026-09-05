use super::resolution::{accessor_path, qualified_path};
use super::*;

impl FunctionLowerer<'_> {
    pub(super) fn lower_function(&mut self, source: &ast::Function) -> Result<(), FosterError> {
        let mut parameters = Vec::new();
        let mut parameter_names = std::collections::HashSet::new();
        for parameter in &source.parameters {
            if !parameter_names.insert(parameter.name.as_str()) {
                let error = self.error(format!(
                    "function `{}` has more than one parameter named `{}`",
                    source.name, parameter.name
                ));
                return Err(error.with_fallback_location(
                    self.hir.modules[self.module].name.clone(),
                    parameter.span.clone(),
                    "this parameter name is declared more than once",
                ));
            }
            let local = self.hir.locals.alloc(Local {
                span: parameter.span.clone(),
                function: self.function,
                name: parameter.name.clone(),
                kind: LocalKind::Parameter,
            });
            self.locals.insert(parameter.name.clone(), local);
            parameters.push(local);
        }

        let mut body = crate::block::Block::new();
        for (statement, statement_span) in source.body.iter_spanned() {
            let span = statement_span.clone();
            let lowered = self.lower_statement(statement).map_err(|error| {
                error.with_fallback_location(
                    self.hir.modules[self.module].name.clone(),
                    span,
                    "this statement could not be lowered",
                )
            })?;
            body.push(lowered, statement_span.clone());
        }
        self.hir.functions[self.function].parameters = parameters;
        self.hir.functions[self.function].receiver = source
            .receiver
            .then(|| self.hir.functions[self.function].parameters[0]);
        self.hir.functions[self.function].body = body;
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
            ast::Stmt::Assert { condition, message } => Ok(Stmt::Assert {
                condition: self.lower_expression(condition)?,
                message: message
                    .as_ref()
                    .map(|message| self.lower_expression(message))
                    .transpose()?,
            }),
            ast::Stmt::Loop { body } => {
                self.loop_depth += 1;
                let mut lowered = crate::block::Block::new();
                for (statement, statement_span) in body.iter_spanned() {
                    let result = self.lower_statement(statement).map_err(|error| {
                        error.with_fallback_location(
                            self.hir.modules[self.module].name.clone(),
                            statement_span.clone(),
                            "this loop statement could not be lowered",
                        )
                    })?;
                    lowered.push(result, statement_span.clone());
                }
                self.loop_depth -= 1;
                Ok(Stmt::Loop { body: lowered })
            }
            ast::Stmt::Break { guard } => {
                if self.loop_depth == 0 {
                    return Err(self.error("`break` may only appear inside `loop`"));
                }
                Ok(Stmt::Break {
                    guard: guard
                        .as_ref()
                        .map(|guard| self.lower_expression(guard))
                        .transpose()?,
                })
            }
            ast::Stmt::Continue { guard } => {
                if self.loop_depth == 0 {
                    return Err(self.error("`continue` may only appear inside `loop`"));
                }
                Ok(Stmt::Continue {
                    guard: guard
                        .as_ref()
                        .map(|guard| self.lower_expression(guard))
                        .transpose()?,
                })
            }
            ast::Stmt::Bind { name, value } => {
                if self.locals.contains_key(name) {
                    return Err(self.error(format!(
                        "local `{name}` is already declared; omit `let` to assign to it"
                    )));
                }
                if self.hir.constant_named(self.module, name).is_some() {
                    return Err(self.error(format!(
                        "local `{name}` conflicts with a module constant of the same name"
                    )));
                }
                let value = self.lower_expression(value)?;
                let local = self.hir.locals.alloc(Local {
                    span: self.hir.functions[self.function].span.clone(),
                    function: self.function,
                    name: name.clone(),
                    kind: LocalKind::Binding,
                });
                self.locals.insert(name.clone(), local);
                Ok(Stmt::Bind { local, value })
            }
            ast::Stmt::Assign { name, value } => {
                let Some(local) = self.locals.get(name).copied() else {
                    if self.hir.constant_named(self.module, name).is_some() {
                        return Err(self.error(format!("cannot assign to constant `{name}`")));
                    }
                    return Err(self.error(format!(
                        "cannot assign to undeclared local `{name}`; declare it with `let {name} = ...`"
                    )));
                };
                let value = self.lower_expression(value)?;
                if self.hir.locals[local].function != self.function
                    && !self.captures.contains(&local)
                {
                    self.captures.push(local);
                }
                Ok(Stmt::Assign { local, value })
            }
            ast::Stmt::Function(source) => {
                if source.owner.is_some() {
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
                // Keep HIR construction aligned with the language's assignment
                // sequence: the value is evaluated before the destination.
                let value = self.lower_expression(value)?;
                let place = self.lower_expression(place)?;
                Ok(Stmt::Set { place, value })
            }
            ast::Stmt::Expr(expression) => Ok(Stmt::Expr(self.lower_expression(expression)?)),
        }
    }

    fn lower_expression(&mut self, expression: &ast::Expr) -> Result<ExprId, FosterError> {
        if let ast::Expr::Spanned { expression, span } = expression {
            let lowered = self.lower_expression(expression).map_err(|mut error| {
                if error.source_module.is_none() {
                    error.source_module = Some(self.hir.modules[self.module].name.clone());
                }
                if error.labels.is_empty() {
                    let label = error.message.clone();
                    error = error.with_primary_label(span.clone(), label);
                }
                error
            })?;
            self.hir.expression_spans.insert(lowered, span.clone());
            return Ok(lowered);
        }
        if let ast::Expr::Member { object, name } = expression.unspanned()
            && let ast::Expr::Name(qualifier) = object.unspanned()
            && self.imports.contains_key(qualifier)
        {
            return Err(self.error(format!(
                "module qualification uses `::`; write `{qualifier}::{name}`"
            )));
        }
        if let Some(path) = accessor_path(expression)
            && path.len() == 2
        {
            let mut unions = std::iter::once(self.module)
                .chain(self.imports.values().copied())
                .filter_map(|module| self.hir.variant_type_named(module, path[0]))
                .filter(|union| {
                    self.hir.variant_types[*union].kind == ast::VariantKind::Union
                        && (self.hir.variant_types[*union].module == self.module
                            || self.hir.variant_types[*union].public)
                });
            if unions.next().is_some() {
                return Err(self.error(format!(
                    "type union `{}` has no constructors; declare an `enum` for labelled cases",
                    path[0]
                )));
            }
        }
        if let Some(path) = accessor_path(expression)
            && path.len() == 2
            && let Some(variant) = self.resolve_variant_constructor(path[0], path[1])?
        {
            return Ok(self.alloc_expression(Expr::Name(ResolvedName::Variant(variant))));
        }
        if let Some(path) = accessor_path(expression)
            && let Some(function) = self.resolve_associated_function(&path)?
        {
            return Ok(self.alloc_expression(Expr::Name(ResolvedName::Function(function))));
        }
        if let Some(path) = qualified_path(expression) {
            if path.len() == 2
                && !self.imports.contains_key(path[0])
                && (self.hir.record_named(self.module, path[0]).is_some()
                    || self.hir.variant_type_named(self.module, path[0]).is_some()
                    || self.imports.values().any(|module| {
                        self.hir.record_named(*module, path[0]).is_some()
                            || self.hir.variant_type_named(*module, path[0]).is_some()
                    }))
            {
                return Err(self.error(format!(
                    "type access uses `.`; write `{}.{}`",
                    path[0], path[1]
                )));
            }
            if path.len() == 3
                && let Some(module) = self.imports.get(path[0]).copied()
                && (self.hir.record_named(module, path[1]).is_some()
                    || self.hir.variant_type_named(module, path[1]).is_some())
            {
                return Err(self.error(format!(
                    "type access uses `.`; write `{}::{}.{}`",
                    path[0], path[1], path[2]
                )));
            }
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
            ast::Expr::Qualified { .. } => {
                return Err(
                    self.error("`::` requires an imported module name on its left-hand side")
                );
            }
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
            ast::Expr::Try(value) => {
                let value = self.lower_expression(value)?;
                let binding = self.hir.locals.alloc(Local {
                    span: self.hir.functions[self.function].span.clone(),
                    function: self.function,
                    name: "$try".to_owned(),
                    kind: LocalKind::Binding,
                });
                Expr::Try { value, binding }
            }
            ast::Expr::Record {
                constructor,
                fields,
            } => {
                let path = qualified_path(constructor)
                    .ok_or_else(|| self.error("record constructor must be a type name"))?;
                let resolved = if path.len() == 1 {
                    if let Some(record) = self.hir.record_named(self.module, path[0]) {
                        ResolvedName::Record(record)
                    } else {
                        let mut imported = self
                            .imports
                            .values()
                            .filter_map(|module| self.hir.record_named(*module, path[0]))
                            .filter(|record| self.hir.records[*record].public)
                            .collect::<Vec<_>>();
                        imported.sort();
                        imported.dedup();
                        match imported.as_slice() {
                            [record] => ResolvedName::Record(*record),
                            [_, _, ..] => {
                                return Err(self.error(format!(
                                    "imported record type `{}` is ambiguous; qualify it with its module",
                                    path[0]
                                )));
                            }
                            [] => self.resolve_name(path[0])?,
                        }
                    }
                } else {
                    self.resolve_qualified(&path)?
                };
                let ResolvedName::Record(record) = resolved else {
                    return Err(self.error("record constructor must name a record type"));
                };
                if self.hir.records[record].intrinsic {
                    return Err(self.error(format!(
                        "intrinsic type `{}` cannot be constructed as a record",
                        self.hir.records[record].name
                    )));
                }
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
            ast::Expr::Logical {
                left,
                operator,
                right,
            } => self.lower_logical(left, *operator, right)?,
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
                    let mut body = crate::block::Block::new();
                    for (statement, statement_span) in arm.body.iter_spanned() {
                        let lowered_statement =
                            self.lower_statement(statement).map_err(|error| {
                                error.with_fallback_location(
                                    self.hir.modules[self.module].name.clone(),
                                    statement_span.clone(),
                                    "this branch-arm statement could not be lowered",
                                )
                            })?;
                        body.push(lowered_statement, statement_span.clone());
                    }
                    lowered.push(BranchArm { test, body });
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

    fn lower_logical(
        &mut self,
        left: &ast::Expr,
        operator: ast::LogicalOp,
        right: &ast::Expr,
    ) -> Result<Expr, FosterError> {
        let left = self.lower_expression(left)?;
        let right_span = right
            .span()
            .unwrap_or_else(|| self.hir.functions[self.function].span.clone());
        let right = self.lower_expression(right)?;
        let literal_value = matches!(operator, ast::LogicalOp::Or);
        let literal = self.alloc_expression(Expr::Bool(literal_value));
        let literal_span = self
            .hir
            .expression_spans
            .get(&left)
            .cloned()
            .unwrap_or_else(|| self.hir.functions[self.function].span.clone());

        let (matched, fallback) = match operator {
            ast::LogicalOp::And => (right, literal),
            ast::LogicalOp::Or => (literal, right),
        };
        let matched_span = if matches!(operator, ast::LogicalOp::And) {
            right_span.clone()
        } else {
            literal_span.clone()
        };
        let fallback_span = if matches!(operator, ast::LogicalOp::And) {
            literal_span
        } else {
            right_span
        };

        Ok(Expr::Branch {
            subject: None,
            arms: vec![
                BranchArm {
                    test: BranchTest::Condition(left),
                    body: crate::block::Block::single(Stmt::Expr(matched), matched_span),
                },
                BranchArm {
                    test: BranchTest::Wildcard,
                    body: crate::block::Block::single(Stmt::Expr(fallback), fallback_span),
                },
            ],
        })
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
            ast::Pattern::Variant {
                path,
                enum_accessor,
                fields,
            } => {
                let variant = self.resolve_variant(path, *enum_accessor)?;
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
