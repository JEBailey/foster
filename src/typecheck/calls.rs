use super::*;

impl Checker<'_> {
    pub(super) fn infer_call(
        &mut self,
        function: FunctionId,
        callee: ExprId,
        arguments: &[ExprId],
    ) -> Result<Ty, FosterError> {
        if let hir::Expr::Member { object, name } = self.hir.expressions[callee].clone()
            && name == "push"
        {
            if arguments.len() != 1 {
                return Err(self.error(function, "`push` expects one argument"));
            }
            let object = self.infer_expression(function, object)?;
            let element = self.infer_expression(function, arguments[0])?;
            self.unify(object, Ty::List(Box::new(element.clone())), function)?;
            self.expressions
                .insert(callee, Ty::Function(vec![element], Box::new(Ty::Unit)));
            return Ok(Ty::Unit);
        }

        if let hir::Expr::Member { object, name } = self.hir.expressions[callee].clone()
            && name == "in?"
        {
            let receiver = self.infer_expression(function, object)?;
            for argument in arguments {
                let argument = self.infer_expression(function, *argument)?;
                self.unify(receiver.clone(), argument, function)?;
            }
            self.expressions.insert(
                callee,
                Ty::Function(vec![receiver.clone(); arguments.len()], Box::new(Ty::Bool)),
            );
            return Ok(Ty::Bool);
        }
        if let hir::Expr::Member { object, name } = self.hir.expressions[callee].clone()
            && name == "append"
        {
            if arguments.len() != 1 {
                return Err(self.error(function, "`append` expects one argument"));
            }
            let element = self.fresh();
            let object_ty = self.infer_expression(function, object)?;
            self.unify(object_ty, Ty::List(Box::new(element.clone())), function)?;
            let argument = self.infer_expression(function, arguments[0])?;
            self.unify(element.clone(), argument, function)?;
            let result = Ty::List(Box::new(element.clone()));
            self.expressions.insert(
                callee,
                Ty::Function(vec![element], Box::new(result.clone())),
            );
            return Ok(result);
        }

        if let hir::Expr::Name(ResolvedName::Builtin(Builtin::Print | Builtin::Println)) =
            self.hir.expressions[callee]
        {
            let mut parameter_types = Vec::new();
            for argument in arguments {
                parameter_types.push(self.infer_expression(function, *argument)?);
            }
            self.expressions
                .insert(callee, Ty::Function(parameter_types, Box::new(Ty::Unit)));
            return Ok(Ty::Unit);
        }

        if let hir::Expr::Name(ResolvedName::Builtin(builtin)) = self.hir.expressions[callee] {
            let (parameters, result) = self.builtin_signature(builtin)?;
            if arguments.len() != parameters.len() {
                return Err(self.error(function, "builtin argument count mismatch"));
            }
            for (argument, expected) in arguments.iter().zip(parameters.iter()) {
                let actual = self.infer_expression(function, *argument)?;
                self.unify(expected.clone(), actual, function)?;
            }
            self.expressions
                .insert(callee, Ty::Function(parameters, Box::new(result.clone())));
            return Ok(result);
        }

        let callee_type = self.infer_expression(function, callee)?;
        let argument_types = arguments
            .iter()
            .map(|argument| self.infer_expression(function, *argument))
            .collect::<Result<Vec<_>, _>>()?;
        let callee_type = instantiate_call_groups(callee_type, &argument_types);
        self.check_argument_modes(function, &callee_type, arguments, &argument_types)?;
        let remote_call =
            if let hir::Expr::Member { object, .. } = self.hir.expressions[callee].clone() {
                let object = self.infer_expression(function, object)?;
                matches!(self.resolved(object), Ty::Remote(_))
            } else {
                false
            };
        if remote_call
            && let Some(unsafe_type) = argument_types
                .iter()
                .find(|argument| !remote_transferable(&self.resolved((*argument).clone())))
        {
            return Err(self.error(
                function,
                format!(
                    "type `{}` cannot cross a remote-object boundary",
                    self.describe(unsafe_type)
                ),
            ));
        }
        let result = self.fresh();
        self.unify(
            callee_type,
            Ty::Function(argument_types, Box::new(result.clone())),
            function,
        )?;
        Ok(result)
    }

    pub(super) fn builtin_signature(&self, builtin: Builtin) -> Result<(Vec<Ty>, Ty), FosterError> {
        let io_result = |ok| self.host_result(ok, "core.io", "IoError");
        let tcp_result = |ok| self.host_result(ok, "core.net.tcp", "NetworkError");
        Ok(match builtin {
            Builtin::CodePoint => (vec![Ty::CodePoint], Ty::Int),
            Builtin::FromCodePoint => (vec![Ty::Int], Ty::CodePoint),
            Builtin::ParseFloat => (vec![Ty::String], Ty::Float),
            Builtin::IoReadText | Builtin::IoCanonicalize => {
                (vec![Ty::String], io_result(Ty::String)?)
            }
            Builtin::IoWriteText => (vec![Ty::String, Ty::String], io_result(Ty::Unit)?),
            Builtin::IoListDirectory => {
                (vec![Ty::String], io_result(Ty::List(Box::new(Ty::String)))?)
            }
            Builtin::IoExists | Builtin::IoIsFile | Builtin::IoIsDirectory => {
                (vec![Ty::String], Ty::Bool)
            }
            Builtin::IoJoin => (vec![Ty::String, Ty::String], Ty::String),
            Builtin::IoParent | Builtin::IoFileName | Builtin::IoExtension => {
                (vec![Ty::String], Ty::String)
            }
            Builtin::IoCurrentDirectory => (Vec::new(), io_result(Ty::String)?),
            Builtin::TcpListen => (vec![Ty::String, Ty::Int], tcp_result(Ty::Int)?),
            Builtin::TcpConnect => (vec![Ty::String, Ty::Int], tcp_result(Ty::Int)?),
            Builtin::TcpAccept => (vec![Ty::Int], tcp_result(Ty::Int)?),
            Builtin::TcpRead => (vec![Ty::Int, Ty::Int], tcp_result(Ty::String)?),
            Builtin::TcpWrite => (vec![Ty::Int, Ty::String], tcp_result(Ty::Unit)?),
            Builtin::TcpSetTimeout => (vec![Ty::Int, Ty::Int], tcp_result(Ty::Unit)?),
            Builtin::TcpCloseListener | Builtin::TcpCloseConnection => {
                (vec![Ty::Int], tcp_result(Ty::Unit)?)
            }
            Builtin::Print | Builtin::Println => unreachable!("print builtins are variadic"),
        })
    }

    fn host_result(&self, ok: Ty, error_module: &str, error_name: &str) -> Result<Ty, FosterError> {
        let result_module = self.hir.module_named("core.result").ok_or_else(|| {
            FosterError::runtime("host builtins require the embedded `core.result` module")
        })?;
        let result = self
            .hir
            .variant_type_named(result_module, "Result")
            .ok_or_else(|| FosterError::runtime("embedded `core.result.Result` is missing"))?;
        let error_module = self.hir.module_named(error_module).ok_or_else(|| {
            FosterError::runtime(format!("host builtin module `{error_module}` is missing"))
        })?;
        let error = self
            .hir
            .record_named(error_module, error_name)
            .ok_or_else(|| {
                FosterError::runtime(format!("host error type `{error_name}` is missing"))
            })?;
        Ok(Ty::Variant(result, vec![ok, Ty::Record(error, Vec::new())]))
    }

    fn check_argument_modes(
        &self,
        function: FunctionId,
        callee: &Ty,
        arguments: &[ExprId],
        argument_types: &[Ty],
    ) -> Result<(), FosterError> {
        let Ty::Callable {
            parameter_modes, ..
        } = self.resolved(callee.clone())
        else {
            return Ok(());
        };
        for (index, ((argument, argument_type), mode)) in arguments
            .iter()
            .zip(argument_types)
            .zip(parameter_modes)
            .enumerate()
        {
            let generated_partial_parameter = matches!(
                self.hir.expressions[*argument],
                hir::Expr::Name(ResolvedName::Local(local))
                    if self.hir.locals[local].name.starts_with("$partial")
            );
            if mode == crate::ast::ParameterMode::Consume
                && !generated_partial_parameter
                && !copy_type(&self.resolved(argument_type.clone()))
                && self.argument_is_owned_place(*argument)
                && !matches!(self.hir.expressions[*argument], hir::Expr::MoveOut(_))
            {
                return Err(self.error(
                    function,
                    format!(
                        "call consumes argument {}; pass this argument with `move`",
                        index + 1
                    ),
                ));
            }
        }
        Ok(())
    }

    fn argument_is_owned_place(&self, expression: ExprId) -> bool {
        match self.hir.expressions[expression] {
            hir::Expr::Name(ResolvedName::Local(_)) | hir::Expr::Index { .. } => true,
            hir::Expr::Member { object, .. } => self
                .expressions
                .get(&object)
                .is_some_and(|ty| matches!(self.resolved(ty.clone()), Ty::Record(_, _))),
            _ => false,
        }
    }

    pub(super) fn infer_member(
        &mut self,
        function: FunctionId,
        object: Ty,
        name: &str,
    ) -> Result<Ty, FosterError> {
        match (self.resolved(object), name) {
            (Ty::String, "empty?" | "whitespace?") => Ok(Ty::Bool),
            (Ty::String, "length") => Ok(Ty::Int),
            (Ty::String, "head") => Ok(Ty::CodePoint),
            (Ty::String, "rest") => Ok(Ty::String),
            (Ty::CodePoint, "whitespace?") => Ok(Ty::Bool),
            (Ty::CodePoint, "string") => Ok(Ty::String),
            (Ty::Sequence(_), "empty?") => Ok(Ty::Bool),
            (Ty::Sequence(_), "length") => Ok(Ty::Int),
            (Ty::Sequence(element), "head") => Ok(*element),
            (sequence @ Ty::Sequence(_), "rest") => Ok(sequence),
            (Ty::List(_), "empty?") => Ok(Ty::Bool),
            (Ty::List(_), "length") => Ok(Ty::Int),
            (Ty::List(element), "head") => Ok(*element),
            (list @ Ty::List(_), "rest") => Ok(list),
            (Ty::List(element), "append") => Ok(Ty::Function(
                vec![(*element).clone()],
                Box::new(Ty::List(element)),
            )),
            (Ty::Record(record, arguments), member) => {
                if self.hir.records[record]
                    .fields
                    .iter()
                    .any(|field| field.name == member)
                {
                    self.record_field_type(function, record, &arguments, member)
                } else {
                    self.record_method_type(function, record, arguments, member, false, false)
                }
            }
            (Ty::Intersection(members), member) => {
                let mut found: Option<Ty> = None;
                for component in members {
                    let Ty::Record(record, arguments) = component else {
                        continue;
                    };
                    if self.hir.records[record]
                        .fields
                        .iter()
                        .any(|field| field.name == member)
                    {
                        let candidate =
                            self.record_field_type(function, record, &arguments, member)?;
                        if let Some(previous) = &found {
                            self.unify(previous.clone(), candidate.clone(), function)?;
                        }
                        found = Some(candidate);
                    }
                }
                found.ok_or_else(|| {
                    self.error(
                        function,
                        format!("intersection type has no field `{member}`"),
                    )
                })
            }
            (Ty::Remote(value), method) => match *value {
                Ty::Record(record, arguments) => {
                    self.record_method_type(function, record, arguments, method, true, false)
                }
                Ty::Reference(_, value) => match *value {
                    Ty::Record(record, arguments) => {
                        self.record_method_type(function, record, arguments, method, true, true)
                    }
                    other => Err(self.error(
                        function,
                        format!(
                            "Remote[ref {}] has no method `{method}`",
                            self.describe(&other)
                        ),
                    )),
                },
                other => Err(self.error(
                    function,
                    format!("Remote[{}] has no method `{method}`", self.describe(&other)),
                )),
            },
            (receiver @ Ty::Variable(_), _) => {
                let result = self.fresh();
                self.member_constraints.push(MemberConstraint {
                    function,
                    receiver,
                    name: name.to_owned(),
                    result: result.clone(),
                });
                Ok(result)
            }
            (ty, _) => Err(self.error(
                function,
                format!("type `{}` has no member `{name}`", self.describe(&ty)),
            )),
        }
    }

    pub(super) fn record_method_type(
        &mut self,
        caller: FunctionId,
        record: RecordId,
        arguments: Vec<Ty>,
        name: &str,
        remote: bool,
        remote_read_only: bool,
    ) -> Result<Ty, FosterError> {
        let module = self.hir.records[record].module;
        let Some(method) = self.hir.function_named(module, name) else {
            return Err(self.error(
                caller,
                format!(
                    "type `{}` has no member `{name}`",
                    self.hir.records[record].name
                ),
            ));
        };
        let definition = &self.hir.functions[method];
        let is_method = definition
            .parameters
            .first()
            .is_some_and(|parameter| self.hir.locals[*parameter].name == "self");
        if !is_method {
            return Err(self.error(
                caller,
                format!("function `{name}` is not an instance method because its first parameter is not `self`"),
            ));
        }
        if definition.module != self.hir.functions[caller].module && !definition.public {
            return Err(self.error(caller, format!("method `{name}` is private")));
        }
        if remote_read_only
            && definition.effects.iter().any(|effect| {
                effect.target.root == "self" && effect.kind != crate::ast::EffectKind::Read
            })
        {
            return Err(self.error(
                caller,
                format!("read-only remote loan cannot call mutating method `{name}`"),
            ));
        }
        let signature = self.functions[&method].clone();
        let mut parameter_modes = signature.parameter_modes.clone();
        let mut generics = HashMap::new();
        let mut parameters = signature
            .parameters
            .into_iter()
            .map(|ty| self.instantiate(ty, &mut generics))
            .collect::<Vec<_>>();
        let receiver = parameters.remove(0);
        parameter_modes.remove(0);
        if remote {
            for (parameter, mode) in definition.parameters.iter().skip(1).zip(&parameter_modes) {
                let name = &self.hir.locals[*parameter].name;
                if *mode == crate::ast::ParameterMode::Borrow
                    && definition.effects.iter().any(|effect| {
                        effect.target.root == *name && effect.kind != crate::ast::EffectKind::Read
                    })
                {
                    return Err(self.error(
                        caller,
                        format!("remote borrowed parameter `{name}` may only have read effects"),
                    ));
                }
            }
        }
        self.unify(receiver, Ty::Record(record, arguments), caller)?;
        let result = self.instantiate(signature.result, &mut generics);
        if remote && !remote_transferable(&self.resolved(result.clone())) {
            return Err(self.error(
                caller,
                format!(
                    "remote method `{name}` returns `{}`, which cannot cross a remote-object boundary",
                    self.describe(&result)
                ),
            ));
        }
        let result = if remote {
            Ty::Future(Box::new(result))
        } else {
            result
        };
        Ok(Ty::Callable {
            parameters,
            parameter_modes,
            result: Box::new(result),
            erased: false,
            effects: if remote {
                Vec::new()
            } else {
                callable_effects(self.hir, method)
            },
            suspends: if remote { false } else { definition.suspends },
        })
    }
}

fn copy_type(ty: &Ty) -> bool {
    matches!(
        ty,
        Ty::Unit | Ty::Bool | Ty::Int | Ty::Float | Ty::CodePoint | Ty::Symbol
    )
}

fn instantiate_call_groups(callee: Ty, arguments: &[Ty]) -> Ty {
    let Ty::Callable {
        parameters,
        parameter_modes,
        result,
        erased,
        effects,
        suspends,
    } = callee
    else {
        return callee;
    };
    let substitutions = parameters
        .iter()
        .zip(arguments)
        .filter_map(|(parameter, argument)| match (parameter, argument) {
            (Ty::Reference(formal, _), Ty::Reference(actual, _)) => {
                Some((formal.clone(), actual.clone()))
            }
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    Ty::Callable {
        parameters: parameters
            .into_iter()
            .map(|parameter| substitute_groups(parameter, &substitutions))
            .collect(),
        parameter_modes,
        result: Box::new(substitute_groups(*result, &substitutions)),
        erased,
        effects,
        suspends,
    }
}

fn substitute_groups(ty: Ty, substitutions: &HashMap<String, String>) -> Ty {
    match ty {
        Ty::Reference(group, value) => Ty::Reference(
            substitutions.get(&group).cloned().unwrap_or(group),
            Box::new(substitute_groups(*value, substitutions)),
        ),
        Ty::List(value) => Ty::List(Box::new(substitute_groups(*value, substitutions))),
        Ty::Sequence(value) => Ty::Sequence(Box::new(substitute_groups(*value, substitutions))),
        Ty::Remote(value) => Ty::Remote(Box::new(substitute_groups(*value, substitutions))),
        Ty::Future(value) => Ty::Future(Box::new(substitute_groups(*value, substitutions))),
        Ty::Function(parameters, result) => Ty::Function(
            parameters
                .into_iter()
                .map(|parameter| substitute_groups(parameter, substitutions))
                .collect(),
            Box::new(substitute_groups(*result, substitutions)),
        ),
        Ty::Callable {
            parameters,
            parameter_modes,
            result,
            erased,
            effects,
            suspends,
        } => Ty::Callable {
            parameters: parameters
                .into_iter()
                .map(|parameter| substitute_groups(parameter, substitutions))
                .collect(),
            parameter_modes,
            result: Box::new(substitute_groups(*result, substitutions)),
            erased,
            effects,
            suspends,
        },
        Ty::Record(record, arguments) => Ty::Record(
            record,
            arguments
                .into_iter()
                .map(|argument| substitute_groups(argument, substitutions))
                .collect(),
        ),
        Ty::Intersection(members) => Ty::Intersection(
            members
                .into_iter()
                .map(|member| substitute_groups(member, substitutions))
                .collect(),
        ),
        Ty::Variant(variant, arguments) => Ty::Variant(
            variant,
            arguments
                .into_iter()
                .map(|argument| substitute_groups(argument, substitutions))
                .collect(),
        ),
        concrete => concrete,
    }
}
