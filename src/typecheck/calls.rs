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
            match self.resolved(object.clone()) {
                Ty::ByteBuffer => self.unify(Ty::Byte, element.clone(), function)?,
                _ => self.unify(object, Ty::List(Box::new(element.clone())), function)?,
            }
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

        if let hir::Expr::Member { object, name } = self.hir.expressions[callee].clone() {
            let object_type = self.infer_expression(function, object)?;
            if name == "freeze"
                && matches!(self.resolved(object_type.clone()), Ty::ByteBuffer)
                && !matches!(self.hir.expressions[object], hir::Expr::MoveOut(_))
            {
                return Err(self.error(
                    function,
                    "method `freeze` consumes its receiver; call `(move buffer).freeze()`",
                ));
            }
            if let Some(method) = self.contract_method_type(function, object_type, &name)? {
                self.expressions.insert(callee, method);
            }
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
        let string = self.string_type();
        Ok(match builtin {
            Builtin::CodePoint => (vec![Ty::CodePoint], Ty::Int),
            Builtin::FromCodePoint => (vec![Ty::Int], Ty::CodePoint),
            Builtin::ParseFloat => (vec![string.clone()], Ty::Float),
            Builtin::ByteValid => (vec![Ty::Int], Ty::Bool),
            Builtin::ByteUnchecked => (vec![Ty::Int], Ty::Byte),
            Builtin::BytesEmpty => (Vec::new(), Ty::Bytes),
            Builtin::BytesFromList => (vec![Ty::List(Box::new(Ty::Byte))], Ty::Bytes),
            Builtin::BytesConcat => (vec![Ty::Bytes, Ty::Bytes], Ty::Bytes),
            Builtin::BytesSlice => (vec![Ty::Bytes, Ty::Int, Ty::Int], Ty::Bytes),
            Builtin::BytesToList => (vec![Ty::Bytes], Ty::List(Box::new(Ty::Byte))),
            Builtin::BytesHex => (vec![Ty::Bytes], string.clone()),
            Builtin::BytesFromHex => (
                vec![string.clone()],
                self.host_result(Ty::Bytes, "core.bytes", "HexError")?,
            ),
            Builtin::StringUtf8 => (vec![string.clone()], Ty::Bytes),
            Builtin::BytesUtf8Valid => (vec![Ty::Bytes], Ty::Bool),
            Builtin::BytesDecodeUtf8 => (vec![Ty::Bytes], string.clone()),
            Builtin::ByteBufferEmpty => (Vec::new(), Ty::ByteBuffer),
            Builtin::ByteBufferWithCapacity => (vec![Ty::Int], Ty::ByteBuffer),
            Builtin::ByteBufferPush => (vec![Ty::ByteBuffer, Ty::Byte], Ty::Unit),
            Builtin::ByteBufferExtend => (vec![Ty::ByteBuffer, Ty::Bytes], Ty::Unit),
            Builtin::ByteBufferClear => (vec![Ty::ByteBuffer], Ty::Unit),
            Builtin::ByteBufferTruncate | Builtin::ByteBufferReserve => {
                (vec![Ty::ByteBuffer, Ty::Int], Ty::Unit)
            }
            Builtin::ByteBufferFreeze | Builtin::ByteBufferSnapshot => {
                (vec![Ty::ByteBuffer], Ty::Bytes)
            }
            Builtin::IoReadText | Builtin::IoCanonicalize => {
                (vec![string.clone()], io_result(string.clone())?)
            }
            Builtin::IoWriteText => (vec![string.clone(), string.clone()], io_result(Ty::Unit)?),
            Builtin::IoReadBytes => (vec![string.clone()], io_result(Ty::Bytes)?),
            Builtin::IoWriteBytes => (vec![string.clone(), Ty::Bytes], io_result(Ty::Unit)?),
            Builtin::IoListDirectory => (
                vec![string.clone()],
                io_result(Ty::List(Box::new(string.clone())))?,
            ),
            Builtin::IoExists | Builtin::IoIsFile | Builtin::IoIsDirectory => {
                (vec![string.clone()], Ty::Bool)
            }
            Builtin::IoJoin => (vec![string.clone(), string.clone()], string.clone()),
            Builtin::IoParent | Builtin::IoFileName | Builtin::IoExtension => {
                (vec![string.clone()], string.clone())
            }
            Builtin::IoCurrentDirectory => (Vec::new(), io_result(string.clone())?),
            Builtin::TcpListen => (vec![string.clone(), Ty::Int], tcp_result(Ty::Int)?),
            Builtin::TcpConnect => (vec![string.clone(), Ty::Int], tcp_result(Ty::Int)?),
            Builtin::TcpAccept => (vec![Ty::Int], tcp_result(Ty::Int)?),
            Builtin::TcpRead => (vec![Ty::Int, Ty::Int], tcp_result(string.clone())?),
            Builtin::TcpWrite => (vec![Ty::Int, string], tcp_result(Ty::Unit)?),
            Builtin::TcpReadBytes => (vec![Ty::Int, Ty::Int], tcp_result(Ty::Bytes)?),
            Builtin::TcpWriteBytes => (vec![Ty::Int, Ty::Bytes], tcp_result(Ty::Unit)?),
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
            hir::Expr::Member { object, .. } => self.expressions.get(&object).is_some_and(|ty| {
                !self.is_string_type(ty) && matches!(self.resolved(ty.clone()), Ty::Record(_, _))
            }),
            _ => false,
        }
    }

    pub(super) fn infer_member(
        &mut self,
        function: FunctionId,
        object: Ty,
        name: &str,
    ) -> Result<Ty, FosterError> {
        let object = self.resolved(object);
        if self.is_string_type(&object) {
            if name == "value" {
                let Ty::Record(record, arguments) = object else {
                    unreachable!("the Foster String type is a record")
                };
                return self.record_field_type(function, record, &arguments, name);
            }
            return match name {
                "empty?" | "whitespace?" => Ok(Ty::Bool),
                "length" => Ok(Ty::Int),
                "head" => Ok(Ty::CodePoint),
                "rest" => Ok(self.string_type()),
                "utf8" => Ok(Ty::Bytes),
                "iterator" => self.collection_iterator(Ty::CodePoint, function),
                member => {
                    self.primitive_method_type(function, self.string_type(), "core.string", member)
                }
            };
        }
        match (object, name) {
            (Ty::CodePoint, "whitespace?") => Ok(Ty::Bool),
            (Ty::CodePoint, "string") => Ok(self.string_type()),
            (Ty::Byte, "int") => Ok(Ty::Int),
            (Ty::Bytes, "empty?") => Ok(Ty::Bool),
            (Ty::Bytes, "length") => Ok(Ty::Int),
            (Ty::Bytes, "head") => Ok(Ty::Byte),
            (Ty::Bytes, "rest") => Ok(Ty::Bytes),
            (Ty::Bytes, "iterator") => self.collection_iterator(Ty::Byte, function),
            (Ty::Bytes, member) => {
                self.primitive_method_type(function, Ty::Bytes, "core.bytes", member)
            }
            (Ty::ByteBuffer, "empty?") => Ok(Ty::Bool),
            (Ty::ByteBuffer, "length" | "capacity") => Ok(Ty::Int),
            (Ty::ByteBuffer, member) => {
                self.primitive_method_type(function, Ty::ByteBuffer, "core.byte_buffer", member)
            }
            (Ty::Sequence(_), "empty?") => Ok(Ty::Bool),
            (Ty::Sequence(_), "length") => Ok(Ty::Int),
            (Ty::Sequence(element), "head") => Ok(*element),
            (sequence @ Ty::Sequence(_), "rest") => Ok(sequence),
            (Ty::Sequence(element), "iterator") => self.collection_iterator(*element, function),
            (Ty::List(_), "empty?") => Ok(Ty::Bool),
            (Ty::List(_), "length") => Ok(Ty::Int),
            (Ty::List(element), "head") => Ok(*element),
            (list @ Ty::List(_), "rest") => Ok(list),
            (Ty::List(element), "iterator") => self.collection_iterator(*element, function),
            (Ty::List(element), "append") => Ok(Ty::Function(
                vec![(*element).clone()],
                Box::new(Ty::List(element)),
            )),
            (Ty::Record(record, arguments), member) => {
                if self
                    .effective_record_fields(record, &arguments)?
                    .iter()
                    .any(|field| field.name == member)
                {
                    self.record_field_type(function, record, &arguments, member)
                } else if let Some(method) =
                    self.effective_method_type(function, record, &arguments, member)?
                {
                    match method {
                        Ty::Callable {
                            ref parameters,
                            ref result,
                            ref effects,
                            suspends: false,
                            ..
                        } if parameters.is_empty()
                            && effects
                                .iter()
                                .all(|effect| effect.kind == crate::ast::EffectKind::Read) =>
                        {
                            Ok((**result).clone())
                        }
                        method => Ok(method),
                    }
                } else {
                    self.record_method_type(function, record, arguments, member, false, false)
                }
            }
            (Ty::Variant(variant, arguments), member) => {
                self.variant_method_type(function, variant, arguments, member)
            }
            (Ty::Intersection(members), member) => {
                let mut found: Option<Ty> = None;
                for component in members {
                    let candidate = match component {
                        Ty::Record(record, arguments) => {
                            let has_field = self
                                .effective_record_fields(record, &arguments)?
                                .iter()
                                .any(|field| field.name == member);
                            if has_field {
                                Some(self.record_field_type(function, record, &arguments, member)?)
                            } else {
                                None
                            }
                        }
                        sequence @ Ty::Sequence(_)
                            if matches!(member, "empty?" | "length" | "head" | "rest") =>
                        {
                            Some(self.infer_member(function, sequence, member)?)
                        }
                        _ => None,
                    };
                    if let Some(candidate) = candidate {
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
                            "Remote<ref {}> has no method `{method}`",
                            self.describe(&other)
                        ),
                    )),
                },
                other => Err(self.error(
                    function,
                    format!("Remote<{}> has no method `{method}`", self.describe(&other)),
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

    fn collection_iterator(&self, element: Ty, function: FunctionId) -> Result<Ty, FosterError> {
        let module = self.hir.module_named("core.iteration").ok_or_else(|| {
            self.error(function, "collection iteration requires `core.iteration`")
        })?;
        let record = self.hir.modules[module]
            .records
            .get("Iterator")
            .copied()
            .ok_or_else(|| self.error(function, "core.iteration does not define `Iterator`"))?;
        Ok(Ty::Record(record, vec![element]))
    }

    fn primitive_method_type(
        &mut self,
        caller: FunctionId,
        receiver: Ty,
        module: &str,
        name: &str,
    ) -> Result<Ty, FosterError> {
        let module = self
            .hir
            .module_named(module)
            .ok_or_else(|| self.error(caller, "primitive core module is unavailable"))?;
        let function = self
            .hir
            .function_named(module, name)
            .ok_or_else(|| self.error(caller, format!("type has no member `{name}`")))?;
        let definition = &self.hir.functions[function];
        let property = definition
            .effects
            .iter()
            .all(|effect| effect.kind == crate::ast::EffectKind::Read);
        if !definition.public && definition.module != self.hir.functions[caller].module {
            return Err(self.error(caller, format!("method `{name}` is private")));
        }
        let signature = self.functions[&function].clone();
        let mut generics = HashMap::new();
        let mut parameters = signature
            .parameters
            .into_iter()
            .map(|ty| self.instantiate(ty, &mut generics))
            .collect::<Vec<_>>();
        let mut parameter_modes = signature.parameter_modes;
        let expected_receiver = parameters
            .first()
            .cloned()
            .ok_or_else(|| self.error(caller, format!("function `{name}` is not a method")))?;
        self.unify(expected_receiver, receiver, caller)?;
        parameters.remove(0);
        parameter_modes.remove(0);
        let result = self.instantiate(signature.result, &mut generics);
        let method = Ty::Callable {
            parameters,
            parameter_modes,
            result: Box::new(result.clone()),
            erased: false,
            effects: callable_effects(self.hir, function),
            suspends: definition.suspends,
        };
        match &method {
            Ty::Callable {
                parameters,
                effects,
                suspends: false,
                ..
            } if property
                && parameters.is_empty()
                && effects
                    .iter()
                    .all(|effect| effect.kind == crate::ast::EffectKind::Read) =>
            {
                Ok(result)
            }
            _ => Ok(method),
        }
    }

    fn contract_method_type(
        &mut self,
        function: FunctionId,
        object: Ty,
        name: &str,
    ) -> Result<Option<Ty>, FosterError> {
        match self.resolved(object) {
            Ty::Record(record, arguments) => {
                self.effective_method_type(function, record, &arguments, name)
            }
            Ty::Intersection(members) => {
                for member in members {
                    if let Some(found) = self.contract_method_type(function, member, name)? {
                        return Ok(Some(found));
                    }
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn effective_method_type(
        &mut self,
        caller: FunctionId,
        record: RecordId,
        arguments: &[Ty],
        name: &str,
    ) -> Result<Option<Ty>, FosterError> {
        let definition = self.hir.records[record].clone();
        let Some(method) = self
            .effective_record_methods(record, arguments)?
            .into_iter()
            .find(|method| method.name == name)
        else {
            return Ok(None);
        };
        if definition.module != self.hir.functions[caller].module && !method.public {
            return Err(self.error(caller, format!("method `{name}` is private")));
        }
        Ok(Some(Ty::Callable {
            parameters: method.parameters,
            parameter_modes: method.parameter_modes,
            result: Box::new(method.result),
            erased: false,
            effects: method.effects,
            suspends: method.suspends,
        }))
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

    pub(super) fn variant_method_type(
        &mut self,
        caller: FunctionId,
        variant: VariantTypeId,
        arguments: Vec<Ty>,
        name: &str,
    ) -> Result<Ty, FosterError> {
        let definition = self.hir.variant_types[variant].clone();
        let Some(method) = self.hir.function_named(definition.module, name) else {
            return Err(self.error(
                caller,
                format!("type `{}` has no member `{name}`", definition.name),
            ));
        };
        let function = &self.hir.functions[method];
        if function
            .parameters
            .first()
            .is_none_or(|parameter| self.hir.locals[*parameter].name != "self")
        {
            return Err(self.error(caller, format!(
                "function `{name}` is not an instance method because its first parameter is not `self`"
            )));
        }
        if function.module != self.hir.functions[caller].module && !function.public {
            return Err(self.error(caller, format!("method `{name}` is private")));
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
        self.unify(receiver, Ty::Variant(variant, arguments), caller)?;
        let result = self.instantiate(signature.result, &mut generics);
        Ok(Ty::Callable {
            parameters,
            parameter_modes,
            result: Box::new(result),
            erased: false,
            effects: callable_effects(self.hir, method),
            suspends: function.suspends,
        })
    }
}

fn copy_type(ty: &Ty) -> bool {
    matches!(
        ty,
        Ty::Unit | Ty::Bool | Ty::Int | Ty::Float | Ty::CodePoint | Ty::Byte | Ty::Symbol
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
