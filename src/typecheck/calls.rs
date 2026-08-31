use super::*;

impl Checker<'_> {
    pub(super) fn infer_call(
        &mut self,
        function: FunctionId,
        call: ExprId,
        callee: ExprId,
        arguments: &[ExprId],
    ) -> Result<Ty, FosterError> {
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
                self.check_expression(function, *argument, expected.clone())?;
            }
            self.expressions
                .insert(callee, Ty::Function(parameters, Box::new(result.clone())));
            return Ok(result);
        }

        if let hir::Expr::Name(ResolvedName::Function(representative)) =
            self.hir.expressions[callee]
        {
            self.resolved_calls
                .insert(callee, ResolvedCall::Function(representative));
            let definition = &self.hir.functions[representative];
            let declared_overloads = self
                .hir
                .functions_named(definition.module, &definition.name);
            let overloads = declared_overloads
                .iter()
                .copied()
                .filter(|candidate| {
                    definition.module == self.hir.functions[function].module
                        || self.hir.functions[*candidate].public
                })
                .collect::<Vec<_>>();
            if declared_overloads.len() > 1 {
                return self.infer_overloaded_function_call(
                    function,
                    call,
                    callee,
                    arguments,
                    &definition.name,
                    &overloads,
                );
            }
        }

        if let hir::Expr::Member { object, name } = self.hir.expressions[callee].clone() {
            let object_type = self.infer_expression(function, object)?;
            if name == "freeze"
                && self.is_byte_buffer_type(&object_type)
                && !matches!(self.hir.expressions[object], hir::Expr::MoveOut(_))
            {
                return Err(self.error(
                    function,
                    "method `freeze` consumes its receiver; call `(move buffer).freeze()`",
                ));
            }
            let method_overloads = self.inherent_method_overloads(
                function,
                &self.resolved(object_type.clone()),
                &name,
            );
            if method_overloads.len() > 1 {
                return self.infer_overloaded_method_call(
                    function,
                    call,
                    callee,
                    arguments,
                    object_type,
                    &method_overloads,
                );
            }
            let selected_inherent_method = method_overloads.first().copied();
            let extension_overloads =
                self.extension_method_candidates(function, object_type.clone(), &name)?;
            if extension_overloads.len() > 1 {
                return self.infer_overloaded_method_call(
                    function,
                    call,
                    callee,
                    arguments,
                    object_type,
                    &extension_overloads,
                );
            }
            let contract_overloads =
                self.contract_method_overloads(function, object_type.clone(), &name)?;
            if contract_overloads.len() > 1 {
                return self.infer_overloaded_contract_method_call(
                    function,
                    call,
                    callee,
                    arguments,
                    &name,
                    &contract_overloads,
                );
            }
            if let Some(method) = self.contract_method_type(function, object_type.clone(), &name)? {
                if let Some(method_function) = selected_inherent_method {
                    let remote = matches!(self.resolved(object_type.clone()), Ty::Remote(_));
                    self.resolved_calls.insert(
                        callee,
                        ResolvedCall::Method {
                            function: method_function,
                            remote,
                        },
                    );
                } else {
                    let requirement = self.contract_method_requirement(object_type.clone(), &name);
                    let dispatch = match &method {
                        Ty::Callable {
                            parameters,
                            parameter_modes,
                            ..
                        } => self.method_key(&name, parameters, parameter_modes),
                        _ => unreachable!("contract methods are callable"),
                    };
                    let slot = self.dispatch_slot(dispatch);
                    self.resolved_calls.insert(
                        callee,
                        ResolvedCall::ContractMethod {
                            slot,
                            name: name.clone(),
                            requirement,
                        },
                    );
                }
                self.expressions.insert(callee, method);
            } else if let Some((method_function, method)) =
                self.extension_method_type(function, object_type, &name)?
            {
                self.resolved_calls.insert(
                    callee,
                    ResolvedCall::Method {
                        function: method_function,
                        remote: false,
                    },
                );
                self.expressions.insert(callee, method);
            }
            if let Some(method_function) = selected_inherent_method {
                let remote = matches!(
                    self.resolved(self.expressions[&object].clone()),
                    Ty::Remote(_)
                );
                self.resolved_calls
                    .entry(callee)
                    .or_insert(ResolvedCall::Method {
                        function: method_function,
                        remote,
                    });
            }
        }

        let callee_type = self.infer_expression(function, callee)?;
        if !self.resolved_calls.contains_key(&callee)
            && let hir::Expr::Member { object, name } = self.hir.expressions[callee].clone()
            && let Ty::Callable {
                parameters,
                parameter_modes,
                ..
            } = self.resolved(callee_type.clone())
        {
            let object_type = self.infer_expression(function, object)?;
            let value_member = self.has_stored_member(&object_type, &name)?
                || name == "head" && self.list_element(&object_type).is_some();
            if !value_member {
                let dispatch = self.method_key(&name, &parameters, &parameter_modes);
                let slot = self.dispatch_slot(dispatch);
                let requirement = self.contract_method_requirement(object_type, &name);
                self.resolved_calls.insert(
                    callee,
                    ResolvedCall::ContractMethod {
                        slot,
                        name,
                        requirement,
                    },
                );
            }
        }
        let expected_arguments = match self.resolved(callee_type.clone()) {
            Ty::Function(parameters, _) | Ty::Callable { parameters, .. }
                if parameters.len() == arguments.len() =>
            {
                Some(parameters)
            }
            _ => None,
        };
        let argument_types = arguments
            .iter()
            .enumerate()
            .map(|(index, argument)| match &expected_arguments {
                Some(parameters)
                    if matches!(
                        (
                            &self.hir.expressions[*argument],
                            self.resolved(parameters[index].clone())
                        ),
                        (hir::Expr::Branch { .. }, Ty::Variant(_, _))
                    ) || matches!(&self.hir.expressions[*argument], hir::Expr::List(_))
                        && self.list_element(&parameters[index]).is_some() =>
                {
                    self.check_expression(function, *argument, parameters[index].clone())
                        .map_err(|error| {
                            self.error_at_expression(
                                error,
                                function,
                                *argument,
                                "argument has an incompatible type",
                            )
                        })
                }
                None => self.infer_expression(function, *argument),
                Some(_) => self.infer_expression(function, *argument),
            })
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
        self.unify_call(
            function,
            call,
            callee_type,
            arguments,
            argument_types,
            result.clone(),
        )?;
        Ok(result)
    }

    pub(super) fn unify_call(
        &mut self,
        function: FunctionId,
        call: ExprId,
        callee: Ty,
        arguments: &[ExprId],
        argument_types: Vec<Ty>,
        result: Ty,
    ) -> Result<(), FosterError> {
        let (parameters, expected_result) = match self.resolved(callee.clone()) {
            Ty::Function(parameters, result)
            | Ty::Callable {
                parameters, result, ..
            } => (parameters, result),
            _ => {
                return self
                    .unify(
                        callee,
                        Ty::Function(argument_types, Box::new(result)),
                        function,
                    )
                    .map_err(|error| {
                        self.error_at_expression(
                            error,
                            function,
                            call,
                            "this value is not callable",
                        )
                    });
            }
        };
        if parameters.len() != argument_types.len() {
            let error = self.error(
                function,
                format!(
                    "function expects {} argument(s), received {}",
                    parameters.len(),
                    argument_types.len()
                ),
            );
            return Err(self.error_at_expression(
                error,
                function,
                call,
                "argument count does not match this call",
            ));
        }
        for ((argument, expected), actual) in arguments.iter().zip(parameters).zip(argument_types) {
            self.coerce_expression(expected, actual, function, *argument)
                .map_err(|error| {
                    self.error_at_expression(
                        error,
                        function,
                        *argument,
                        "argument has an incompatible type",
                    )
                })?;
        }
        self.unify(*expected_result, result, function)
            .map_err(|error| {
                self.error_at_expression(
                    error,
                    function,
                    call,
                    "call result has an incompatible type",
                )
            })
    }

    pub(super) fn builtin_signature(&self, builtin: Builtin) -> Result<(Vec<Ty>, Ty), FosterError> {
        let io_result = |ok| self.host_result(ok, "std.io", "IoError");
        let tcp_result = |ok| self.host_result(ok, "std.net.tcp", "NetworkError");
        let string = self.string_type();
        let bytes = self.bytes_type();
        Ok(match builtin {
            Builtin::FromCodePoint => (vec![Ty::Int], Ty::CodePoint),
            Builtin::ParseFloat => (vec![string.clone()], Ty::Float),
            Builtin::FormatFloat => (vec![Ty::Float], string.clone()),
            Builtin::ByteValid => (vec![Ty::Int], Ty::Bool),
            Builtin::ByteUnchecked => (vec![Ty::Int], Ty::Byte),
            Builtin::BytesEmpty => (Vec::new(), bytes.clone()),
            Builtin::BytesFromList => (vec![self.list_type(Ty::Byte)], bytes.clone()),
            Builtin::BytesConcat => (vec![bytes.clone(), bytes.clone()], bytes.clone()),
            Builtin::BytesSlice => (vec![bytes.clone(), Ty::Int, Ty::Int], bytes.clone()),
            Builtin::BytesToList => (vec![bytes.clone()], self.list_type(Ty::Byte)),
            Builtin::BytesHex => (vec![bytes.clone()], string.clone()),
            Builtin::BytesFromHex => (
                vec![string.clone()],
                self.host_result(bytes.clone(), "core.bytes", "HexError")?,
            ),
            Builtin::StringUtf8 => (vec![string.clone()], bytes.clone()),
            Builtin::BytesUtf8Valid => (vec![bytes.clone()], Ty::Bool),
            Builtin::BytesDecodeUtf8 => (vec![bytes.clone()], string.clone()),
            Builtin::ByteBufferEmpty => (Vec::new(), Ty::RawByteBuffer),
            Builtin::ByteBufferWithCapacity => (vec![Ty::Int], Ty::RawByteBuffer),
            Builtin::ByteBufferPush => (vec![Ty::RawByteBuffer, Ty::Byte], Ty::RawByteBuffer),
            Builtin::ByteBufferExtend => {
                (vec![Ty::RawByteBuffer, bytes.clone()], Ty::RawByteBuffer)
            }
            Builtin::ByteBufferClear => (vec![Ty::RawByteBuffer], Ty::RawByteBuffer),
            Builtin::ByteBufferTruncate | Builtin::ByteBufferReserve => {
                (vec![Ty::RawByteBuffer, Ty::Int], Ty::RawByteBuffer)
            }
            Builtin::ByteBufferFreeze | Builtin::ByteBufferSnapshot => {
                (vec![Ty::RawByteBuffer], bytes.clone())
            }
            Builtin::IoReadText | Builtin::IoCanonicalize => {
                (vec![string.clone()], io_result(string.clone())?)
            }
            Builtin::IoWriteText => (vec![string.clone(), string.clone()], io_result(Ty::Unit)?),
            Builtin::IoReadBytes => (vec![string.clone()], io_result(bytes.clone())?),
            Builtin::IoWriteBytes => (vec![string.clone(), bytes.clone()], io_result(Ty::Unit)?),
            Builtin::IoReadRange => (
                vec![string.clone(), Ty::Int, Ty::Int],
                io_result(bytes.clone())?,
            ),
            Builtin::IoAppendBytes => (vec![string.clone(), bytes.clone()], io_result(Ty::Int)?),
            Builtin::IoFileLength => (vec![string.clone()], io_result(Ty::Int)?),
            Builtin::IoListDirectory => (
                vec![string.clone()],
                io_result(self.list_type(string.clone()))?,
            ),
            Builtin::IoExists | Builtin::IoIsFile | Builtin::IoIsDirectory => {
                (vec![string.clone()], Ty::Bool)
            }
            Builtin::IoCreateDirectory
            | Builtin::IoCreateDirectoryAll
            | Builtin::IoRemoveFile
            | Builtin::IoRemoveDirectory => (vec![string.clone()], io_result(Ty::Unit)?),
            Builtin::IoRename => (vec![string.clone(), string.clone()], io_result(Ty::Unit)?),
            Builtin::IoCopyFile => (vec![string.clone(), string.clone()], io_result(Ty::Int)?),
            Builtin::IoJoin => (vec![string.clone(), string.clone()], string.clone()),
            Builtin::IoParent | Builtin::IoFileName | Builtin::IoExtension => {
                (vec![string.clone()], string.clone())
            }
            Builtin::IoCurrentDirectory => (Vec::new(), io_result(string.clone())?),
            Builtin::TimeWallNow => (Vec::new(), self.list_type(Ty::Int)),
            Builtin::TimeMonotonicNow => (Vec::new(), Ty::Int),
            Builtin::TcpListen => (vec![string.clone(), Ty::Int], tcp_result(Ty::Int)?),
            Builtin::TcpConnect => (vec![string.clone(), Ty::Int], tcp_result(Ty::Int)?),
            Builtin::TcpAccept => (vec![Ty::Int], tcp_result(Ty::Int)?),
            Builtin::TcpRead => (vec![Ty::Int, Ty::Int], tcp_result(string.clone())?),
            Builtin::TcpWrite => (vec![Ty::Int, string], tcp_result(Ty::Unit)?),
            Builtin::TcpReadBytes => (vec![Ty::Int, Ty::Int], tcp_result(bytes.clone())?),
            Builtin::TcpWriteBytes => (vec![Ty::Int, bytes], tcp_result(Ty::Unit)?),
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

    pub(super) fn check_argument_modes(
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
                && !self.is_copy_type(argument_type)
                && self.argument_is_owned_place(*argument)
                && !matches!(self.hir.expressions[*argument], hir::Expr::MoveOut(_))
            {
                let error = self.error(
                    function,
                    format!(
                        "call consumes argument {}; pass this argument with `move`",
                        index + 1
                    ),
                );
                return Err(self.error_at_expression(
                    error,
                    function,
                    *argument,
                    "this argument must be passed with `move`",
                ));
            }
        }
        Ok(())
    }

    fn argument_is_owned_place(&self, expression: ExprId) -> bool {
        match self.hir.expressions[expression] {
            hir::Expr::Name(ResolvedName::Local(_)) | hir::Expr::Index { .. } => true,
            hir::Expr::Member { object, .. } => self.expressions.get(&object).is_some_and(|ty| {
                !self.is_string_type(ty)
                    && !self.is_bytes_type(ty)
                    && !self.is_byte_buffer_type(ty)
                    && self.list_element(ty).is_none()
                    && matches!(self.resolved(ty.clone()), Ty::Record(_, _))
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
        if let Ty::Reference(_, value) = object {
            return self.infer_member(function, *value, name);
        }
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
                "utf8" => Ok(self.bytes_type()),
                "iterator" => self.collection_iterator_method(Ty::CodePoint, function),
                member => {
                    self.primitive_method_type(function, self.string_type(), "core.string", member)
                }
            };
        }
        if self.is_bytes_type(&object) {
            return match name {
                "empty?" => Ok(Ty::Bool),
                "length" => Ok(Ty::Int),
                "head" => Ok(Ty::Byte),
                "rest" => Ok(self.bytes_type()),
                "iterator" => self.collection_iterator_method(Ty::Byte, function),
                member => {
                    self.primitive_method_type(function, self.bytes_type(), "core.bytes", member)
                }
            };
        }
        if self.is_byte_buffer_type(&object) {
            if name == "value" {
                let Ty::Record(record, arguments) = object else {
                    unreachable!("the Foster ByteBuffer type is a record")
                };
                return self.record_field_type(function, record, &arguments, name);
            }
            return match name {
                "empty?" => Ok(Ty::Bool),
                "length" | "capacity" => Ok(Ty::Int),
                member => self.primitive_method_type(
                    function,
                    self.byte_buffer_type(),
                    "core.bytes.buffer",
                    member,
                ),
            };
        }
        if let Some(element) = self.list_element(&object) {
            return match name {
                "empty?" => Ok(Ty::Bool),
                "length" => Ok(Ty::Int),
                "head" => Ok(element.clone()),
                "rest" => Ok(self.list_type(element.clone())),
                "iterator" => self.collection_iterator_method(element.clone(), function),
                member => self.primitive_method_type(function, object, "core.list", member),
            };
        }
        match (object, name) {
            (Ty::CodePoint, "whitespace?") => Ok(Ty::Bool),
            (Ty::CodePoint, "string") => Ok(self.string_type()),
            (Ty::CodePoint, member) => {
                self.primitive_method_type(function, Ty::CodePoint, "core.code_point", member)
            }
            (Ty::Byte, "int") => Ok(Ty::Int),
            (Ty::Byte, member) => {
                self.primitive_method_type(function, Ty::Byte, "core.byte", member)
            }
            (Ty::Bool, member) => {
                self.primitive_method_type(function, Ty::Bool, "core.bool", member)
            }
            (Ty::Int, member) => self.primitive_method_type(function, Ty::Int, "core.int", member),
            (Ty::Float, member) => {
                self.primitive_method_type(function, Ty::Float, "core.float", member)
            }
            (Ty::RawBytes, "empty?") => Ok(Ty::Bool),
            (Ty::RawBytes, "length") => Ok(Ty::Int),
            (Ty::RawBytes, "head") => Ok(Ty::Byte),
            (Ty::RawBytes, "rest") => Ok(Ty::RawBytes),
            (Ty::RawBytes, "iterator") => self.collection_iterator_method(Ty::Byte, function),
            (Ty::RawBytes, member) => {
                self.primitive_method_type(function, Ty::RawBytes, "core.bytes", member)
            }
            (Ty::RawByteBuffer, "empty?") => Ok(Ty::Bool),
            (Ty::RawByteBuffer, "length" | "capacity") => Ok(Ty::Int),
            (Ty::RawByteBuffer, member) => {
                self.primitive_method_type(function, Ty::RawByteBuffer, "core.bytes.buffer", member)
            }
            (Ty::Sequence(_), "empty?") => Ok(self.sequence_accessor_method(Ty::Bool)),
            (Ty::Sequence(_), "length") => Ok(self.sequence_accessor_method(Ty::Int)),
            (Ty::Sequence(element), "head") => Ok(self.sequence_accessor_method(*element)),
            (sequence @ Ty::Sequence(_), "rest") => Ok(self.sequence_accessor_method(sequence)),
            (Ty::Sequence(element), "iterator") => {
                self.collection_iterator_method(*element, function)
            }
            (Ty::RawList(_), "empty?") => Ok(Ty::Bool),
            (Ty::RawList(_), "length") => Ok(Ty::Int),
            (Ty::RawList(element), "head") => Ok(*element),
            (list @ Ty::RawList(_), "rest") => Ok(list),
            (Ty::RawList(element), "iterator") => {
                self.collection_iterator_method(*element, function)
            }
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
                    Ok(method)
                } else {
                    self.record_method_type(function, record, arguments, member, false, false)
                }
            }
            (Ty::Variant(variant, arguments), member) => {
                let definition = &self.hir.variant_types[variant];
                let qualified_name = format!("{}.{member}", definition.name);
                let has_union_method = self
                    .hir
                    .function_named(definition.module, &qualified_name)
                    .is_some_and(|function| self.hir.functions[function].receiver.is_some());
                if definition.kind == crate::ast::VariantKind::Enum || has_union_method {
                    self.variant_method_type(function, variant, arguments, member)
                } else {
                    self.union_member_type(function, variant, arguments, member)
                }
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
        let module = self
            .hir
            .module_named("std.iter")
            .ok_or_else(|| self.error(function, "collection iteration requires `std.iter`"))?;
        let record = self.hir.modules[module]
            .records
            .get("Iterator")
            .copied()
            .ok_or_else(|| self.error(function, "std.iter does not define `Iterator`"))?;
        Ok(Ty::Record(record, vec![element]))
    }

    fn collection_iterator_method(
        &self,
        element: Ty,
        function: FunctionId,
    ) -> Result<Ty, FosterError> {
        Ok(Ty::Callable {
            parameters: Vec::new(),
            parameter_modes: Vec::new(),
            result: Box::new(self.collection_iterator(element, function)?),
            erased: false,
            effects: vec![crate::ast::Effect {
                kind: crate::ast::EffectKind::Read,
                target: crate::ast::GroupPath::root("self"),
            }],
            suspends: false,
        })
    }

    fn sequence_accessor_method(&self, result: Ty) -> Ty {
        Ty::Callable {
            parameters: Vec::new(),
            parameter_modes: Vec::new(),
            result: Box::new(result),
            erased: false,
            effects: vec![crate::ast::Effect {
                kind: crate::ast::EffectKind::Read,
                target: crate::ast::GroupPath::root("self"),
            }],
            suspends: false,
        }
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
            .ok_or_else(|| self.error(caller, format!("type has no member `{name}`")))?;
        let qualified_name = match receiver {
            Ty::Record(record, _) => format!("{}.{name}", self.hir.records[record].name),
            Ty::Bool => format!("Bool.{name}"),
            Ty::Int => format!("Int.{name}"),
            Ty::Float => format!("Float.{name}"),
            Ty::CodePoint => format!("CodePoint.{name}"),
            Ty::Byte => format!("Byte.{name}"),
            Ty::RawBytes => format!("RawBytes.{name}"),
            Ty::RawByteBuffer => format!("RawByteBuffer.{name}"),
            _ => name.to_owned(),
        };
        let function = self
            .hir
            .function_named(module, &qualified_name)
            .ok_or_else(|| self.error(caller, format!("type has no member `{name}`")))?;
        let definition = &self.hir.functions[function];
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
        Ok(Ty::Callable {
            parameters,
            parameter_modes,
            result: Box::new(result),
            erased: false,
            effects: callable_effects(self.hir, function),
            suspends: definition.suspends,
        })
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

    fn contract_method_requirement(&self, object: Ty, name: &str) -> Option<(RecordId, usize)> {
        match self.resolved(object) {
            Ty::Record(record, _) => self.hir.records[record]
                .methods
                .iter()
                .position(|method| method.name == name)
                .map(|method| (record, method)),
            Ty::Intersection(members) => members
                .into_iter()
                .find_map(|member| self.contract_method_requirement(member, name)),
            _ => None,
        }
    }

    fn extension_method_type(
        &mut self,
        caller: FunctionId,
        receiver: Ty,
        name: &str,
    ) -> Result<Option<(FunctionId, Ty)>, FosterError> {
        let candidates = self.extension_method_candidates(caller, receiver.clone(), name)?;
        let Some(method_function) = candidates.first().copied() else {
            return Ok(None);
        };

        let definition = &self.hir.functions[method_function];
        let signature = self.functions[&method_function].clone();
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
        Ok(Some((
            method_function,
            Ty::Callable {
                parameters,
                parameter_modes,
                result: Box::new(result),
                erased: false,
                effects: callable_effects(self.hir, method_function),
                suspends: definition.suspends,
            },
        )))
    }

    fn extension_method_candidates(
        &self,
        caller: FunctionId,
        receiver: Ty,
        name: &str,
    ) -> Result<Vec<FunctionId>, FosterError> {
        let caller_module = self.hir.functions[caller].module;
        let resolved_receiver = self.resolved(receiver);
        let resolved_receiver = match resolved_receiver {
            Ty::Reference(_, value) => *value,
            receiver => receiver,
        };
        let inherent_owner = match &resolved_receiver {
            Ty::Record(record, _) => Some((
                self.hir.records[*record].module,
                self.hir.records[*record].name.as_str(),
            )),
            Ty::Variant(variant, _) => Some((
                self.hir.variant_types[*variant].module,
                self.hir.variant_types[*variant].name.as_str(),
            )),
            _ => None,
        };
        if let Some((module, owner)) = inherent_owner
            && self
                .hir
                .functions_named(module, &format!("{owner}.{name}"))
                .iter()
                .any(|function| self.hir.functions[*function].receiver.is_some())
        {
            return Ok(Vec::new());
        }
        let inherent_module = inherent_owner.map(|(module, _)| module);
        let mut candidates = Vec::new();
        let mut candidate_modules = HashSet::new();
        for module in std::iter::once(caller_module)
            .chain(self.hir.modules[caller_module].imports.values().copied())
            .filter(|module| Some(*module) != inherent_module)
        {
            for function in self.hir.modules[module]
                .functions
                .values()
                .flatten()
                .copied()
            {
                let definition = &self.hir.functions[function];
                if definition.public
                    && definition.receiver.is_some()
                    && definition
                        .name
                        .rsplit_once('.')
                        .is_some_and(|(_, member)| member == name)
                    && self.functions.get(&function).is_some_and(|signature| {
                        signature.parameters.first().is_some_and(|expected| {
                            receiver_heads_match(
                                &self.resolved(expected.clone()),
                                &resolved_receiver,
                            )
                        })
                    })
                {
                    candidates.push(function);
                    candidate_modules.insert(module);
                }
            }
        }
        if candidate_modules.len() > 1 {
            return Err(self.error(
                caller,
                format!("extension method `{name}` is imported from more than one module"),
            ));
        }
        candidates.sort_unstable_by_key(|function| function.into_raw().into_u32());
        candidates.dedup();
        Ok(candidates)
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
        let qualified_name = format!("{}.{name}", self.hir.records[record].name);
        let Some(method) = self.hir.function_named(module, &qualified_name) else {
            return Err(self.error(
                caller,
                format!(
                    "type `{}` has no member `{name}`",
                    self.hir.records[record].name
                ),
            ));
        };
        let definition = &self.hir.functions[method];
        let is_method = definition.receiver.is_some();
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
        let qualified_name = format!("{}.{name}", definition.name);
        let Some(method) = self.hir.function_named(definition.module, &qualified_name) else {
            return Err(self.error(
                caller,
                format!("type `{}` has no member `{name}`", definition.name),
            ));
        };
        let function = &self.hir.functions[method];
        if function.receiver.is_none() {
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

    fn union_member_type(
        &mut self,
        function: FunctionId,
        union: VariantTypeId,
        arguments: Vec<Ty>,
        name: &str,
    ) -> Result<Ty, FosterError> {
        let definition = self.hir.variant_types[union].clone();
        let generics = definition
            .parameters
            .iter()
            .cloned()
            .zip(arguments)
            .collect::<HashMap<_, _>>();
        let mut common: Option<Ty> = None;
        for alternative in definition.alternatives {
            let member = self.annotation_type(
                definition.module,
                self.hir.variants[alternative]
                    .member
                    .as_ref()
                    .expect("a union alternative has a member type"),
                &generics,
            )?;
            let candidate = self.infer_member(function, member, name).map_err(|_| {
                self.error(
                    function,
                    format!(
                        "union contract `{}` does not provide member `{name}` on every alternative",
                        definition.name
                    ),
                )
            })?;
            if let Some(previous) = &common {
                self.unify(previous.clone(), candidate, function)
                    .map_err(|_| {
                        self.error(
                            function,
                            format!(
                                "union contract `{}` has incompatible definitions of member `{name}`",
                                definition.name
                            ),
                        )
                    })?;
            } else {
                common = Some(candidate);
            }
        }
        common.ok_or_else(|| {
            self.error(
                function,
                format!("union contract `{}` has no alternatives", definition.name),
            )
        })
    }
}

fn receiver_heads_match(expected: &Ty, actual: &Ty) -> bool {
    match (expected, actual) {
        (Ty::Record(left, _), Ty::Record(right, _)) => left == right,
        (Ty::Variant(left, _), Ty::Variant(right, _)) => left == right,
        (Ty::RawBytes, Ty::RawBytes)
        | (Ty::RawByteBuffer, Ty::RawByteBuffer)
        | (Ty::RawList(_), Ty::RawList(_))
        | (Ty::Sequence(_), Ty::Sequence(_))
        | (Ty::Bool, Ty::Bool)
        | (Ty::Int, Ty::Int)
        | (Ty::Float, Ty::Float)
        | (Ty::CodePoint, Ty::CodePoint)
        | (Ty::Byte, Ty::Byte) => true,
        (Ty::Reference(_, expected), Ty::Reference(_, actual)) => {
            receiver_heads_match(expected, actual)
        }
        _ => false,
    }
}

pub(super) fn instantiate_call_groups(callee: Ty, arguments: &[Ty]) -> Ty {
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
        Ty::RawList(value) => Ty::RawList(Box::new(substitute_groups(*value, substitutions))),
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
