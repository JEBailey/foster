use super::*;

impl FunctionLowerer<'_> {
    pub(super) fn lower_partial_application(
        &mut self,
        callee: &ast::Expr,
        arguments: &[ast::Expr],
    ) -> Result<ExprId, FosterError> {
        let parent = &self.hir.functions[self.function];
        let span = callee.span().unwrap_or_else(|| parent.span.clone());
        let function = self.hir.functions.alloc(Function {
            span: span.clone(),
            documentation: None,
            module: self.module,
            name: format!("{}$partial", parent.name),
            owner: None,
            receiver: None,
            test_description: None,
            public: false,
            intrinsic: None,
            type_parameters: parent.type_parameters.clone(),
            groups: parent.groups.clone(),
            parameters: Vec::new(),
            parameter_types: Vec::new(),
            parameter_type_spans: Vec::new(),
            return_type: None,
            effects_explicit: false,
            effects: Vec::new(),
            effect_spans: Vec::new(),
            suspends: false,
            suspend_span: None,
            body: crate::block::Block::new(),
        });

        let mut captures = Vec::new();
        let lowered_callee = self.lower_expression(callee)?;
        let call_callee = match self.hir.expressions[lowered_callee].clone() {
            // Functions, built-ins, and enum constructors are resolved without
            // executing user code, so there is no runtime value to capture.
            Expr::Name(
                ResolvedName::Function(_) | ResolvedName::Builtin(_) | ResolvedName::Variant(_),
            ) => lowered_callee,
            // Selecting a method evaluates its receiver at partial-application
            // creation; dispatch itself remains a statically resolved call.
            Expr::Member { object, name } => {
                let receiver = self.partial_capture(object, &mut captures);
                self.hir.expressions[lowered_callee] = Expr::Member {
                    object: receiver,
                    name,
                };
                lowered_callee
            }
            _ => self.partial_capture(lowered_callee, &mut captures),
        };

        let mut parameters = Vec::new();
        let mut call_arguments = Vec::with_capacity(arguments.len());
        for argument in arguments {
            if matches!(argument.unspanned(), ast::Expr::Placeholder) {
                let name = format!("$partial{}", parameters.len());
                let local = self.hir.locals.alloc(Local {
                    span: argument.span().unwrap_or_else(|| span.clone()),
                    function,
                    name,
                    kind: LocalKind::Parameter,
                });
                parameters.push(local);
                call_arguments.push(self.alloc_expression(Expr::Name(ResolvedName::Local(local))));
            } else {
                let source = self.lower_expression(argument)?;
                call_arguments.push(self.partial_capture(source, &mut captures));
            }
        }

        let call = self.alloc_expression(Expr::Call {
            callee: call_callee,
            arguments: call_arguments,
        });
        self.hir.functions[function].parameters = parameters;
        self.hir.functions[function].parameter_types =
            vec![None; self.hir.functions[function].parameters.len()];
        self.hir.functions[function].parameter_type_spans =
            vec![None; self.hir.functions[function].parameters.len()];
        self.hir.functions[function]
            .body
            .push(Stmt::Expr(call), span);

        Ok(self.alloc_expression(Expr::Closure { function, captures }))
    }

    fn partial_capture(&mut self, source: ExprId, captures: &mut Vec<Capture>) -> ExprId {
        let name = format!("$partial.capture{}", captures.len());
        let local = self.hir.locals.alloc(Local {
            span: self
                .hir
                .expression_spans
                .get(&source)
                .cloned()
                .unwrap_or_else(|| self.hir.functions[self.function].span.clone()),
            function: self.function,
            name,
            kind: LocalKind::CapturedValue,
        });
        captures.push(Capture {
            local,
            mode: CaptureMode::Pending,
            source: Some(source),
        });
        self.alloc_expression(Expr::Name(ResolvedName::Local(local)))
    }

    pub(super) fn lower_closure(
        &mut self,
        source: ClosureSource<'_>,
    ) -> Result<ExprId, FosterError> {
        let ClosureSource {
            name: source_name,
            parameters,
            return_type,
            body,
            captures: capture_specs,
            named,
            effects,
            suspends,
        } = source;
        let closure_name = format!("{}${source_name}", self.hir.functions[self.function].name);
        let function = self.hir.functions.alloc(Function {
            span: 0..0,
            documentation: None,
            module: self.module,
            name: closure_name,
            owner: None,
            receiver: None,
            test_description: None,
            public: false,
            intrinsic: None,
            type_parameters: self.hir.functions[self.function].type_parameters.clone(),
            groups: self.hir.functions[self.function].groups.clone(),
            parameters: Vec::new(),
            parameter_types: parameters
                .iter()
                .map(|parameter| parameter.ty.clone())
                .collect(),
            parameter_type_spans: parameters
                .iter()
                .map(|parameter| parameter.type_span.clone())
                .collect(),
            return_type,
            effects_explicit: !effects.is_empty() || suspends,
            effects: effects.to_vec(),
            effect_spans: Vec::new(),
            suspends,
            suspend_span: None,
            body: crate::block::Block::new(),
        });
        let source = ast::Function {
            span: 0..0,
            documentation: None,
            name: source_name.to_owned(),
            owner: None,
            receiver: false,
            public: false,
            intrinsic: None,
            type_parameters: self.hir.functions[function].type_parameters.clone(),
            groups: Vec::new(),
            parameters: parameters.to_vec(),
            return_type: self.hir.functions[function].return_type.clone(),
            effects_explicit: self.hir.functions[function].effects_explicit,
            effects: effects.to_vec(),
            effect_spans: Vec::new(),
            suspends,
            suspend_span: None,
            body: match body {
                ast::ClosureBody::Expression(expression) => {
                    let span = expression.span().unwrap_or(0..0);
                    crate::block::Block::single(ast::Stmt::Expr(*expression), span)
                }
                ast::ClosureBody::Block(body) => body,
            },
        };
        let mut lowerer = FunctionLowerer {
            hir: self.hir,
            module: self.module,
            function,
            imports: self.imports,
            locals: self.locals.clone(),
            captures: Vec::new(),
            self_name: named.then(|| source_name.to_owned()),
            loop_depth: 0,
        };
        lowerer.lower_function(&source)?;
        let mut captures = lowerer
            .captures
            .into_iter()
            .map(|local| Capture {
                local,
                mode: CaptureMode::Pending,
                source: None,
            })
            .collect::<Vec<_>>();
        let mut specified = std::collections::HashSet::new();
        for spec in capture_specs {
            if !specified.insert(spec.name.as_str()) {
                return Err(self.error(format!(
                    "capture `{}` is specified more than once",
                    spec.name
                )));
            }
            let local = self.locals.get(&spec.name).copied().ok_or_else(|| {
                self.error(format!(
                    "capture clause names unknown local `{}`",
                    spec.name
                ))
            })?;
            let capture = captures
                .iter_mut()
                .find(|capture| capture.local == local)
                .ok_or_else(|| {
                    self.error(format!(
                        "capture clause names `{}`, but the closure does not use it",
                        spec.name
                    ))
                })?;
            capture.mode = match spec.mode {
                ast::CaptureMode::Copy => CaptureMode::Copy,
                ast::CaptureMode::Move => CaptureMode::Move,
                ast::CaptureMode::Ref => CaptureMode::Ref,
            };
        }
        Ok(self.alloc_expression(Expr::Closure { function, captures }))
    }
}
