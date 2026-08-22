use super::*;

impl FunctionLowerer<'_> {
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
            public: false,
            intrinsic: None,
            type_parameters: self.hir.functions[self.function].type_parameters.clone(),
            groups: self.hir.functions[self.function].groups.clone(),
            parameters: Vec::new(),
            parameter_types: parameters
                .iter()
                .map(|parameter| parameter.ty.clone())
                .collect(),
            return_type,
            effects_explicit: !effects.is_empty() || suspends,
            effects: effects.to_vec(),
            effect_spans: Vec::new(),
            suspends,
            suspend_span: None,
            body: Vec::new(),
            statement_spans: Vec::new(),
        });
        let source = ast::Function {
            span: 0..0,
            documentation: None,
            name: source_name.to_owned(),
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
                ast::ClosureBody::Expression(expression) => vec![ast::Stmt::Expr(*expression)],
                ast::ClosureBody::Block(body) => body,
            },
            statement_spans: Vec::new(),
        };
        let mut lowerer = FunctionLowerer {
            hir: self.hir,
            module: self.module,
            function,
            imports: self.imports,
            locals: self.locals.clone(),
            captures: Vec::new(),
            self_name: named.then(|| source_name.to_owned()),
        };
        lowerer.lower_function(&source)?;
        let mut captures = lowerer
            .captures
            .into_iter()
            .map(|local| Capture {
                local,
                mode: CaptureMode::Pending,
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
