use super::*;
use crate::intrinsics::{Builtin, Intrinsic, IntrinsicReceiverMode};

impl FunctionCompiler<'_> {
    pub(super) fn expression(&mut self, id: ExprId) -> Result<Register, FosterError> {
        let source = self.expression_unwrapped(id)?;
        if !self.types.integer_promotions.contains(&id) {
            return Ok(source);
        }

        let span = self
            .hir
            .expression_spans
            .get(&id)
            .cloned()
            .unwrap_or_else(|| self.hir.functions[self.function].span.clone());
        let zero = self.load_constant(Constant::Integer(0), span.clone())?;
        let destination = self.allocate();
        self.emit(
            Instruction::Binary {
                destination,
                operator: crate::ast::BinaryOp::Add,
                left: zero,
                right: source,
            },
            span,
        );
        Ok(destination)
    }

    fn expression_unwrapped(&mut self, id: ExprId) -> Result<Register, FosterError> {
        let span = self
            .hir
            .expression_spans
            .get(&id)
            .cloned()
            .unwrap_or_else(|| self.hir.functions[self.function].span.clone());
        match &self.hir.expressions[id] {
            hir::Expr::Unit => self.load_constant(Constant::Unit, span),
            hir::Expr::Bool(value) => self.load_constant(Constant::Bool(*value), span),
            hir::Expr::Integer(value) => self.load_constant(Constant::Integer(*value), span),
            hir::Expr::Float(value) => self.load_constant(Constant::Float(*value), span),
            hir::Expr::String(value) => self.load_constant(Constant::String(value.clone()), span),
            hir::Expr::CodePoint(value) => self.load_constant(
                Constant::CodePoint(
                    value
                        .chars()
                        .next()
                        .expect("parsed CodePoint literals contain one scalar value"),
                ),
                span,
            ),
            hir::Expr::Symbol(value) => self.load_constant(Constant::Symbol(value.clone()), span),
            hir::Expr::List(items) => {
                let elements = items
                    .iter()
                    .map(|item| self.expression(*item))
                    .collect::<Result<Vec<_>, _>>()?;
                let destination = self.allocate();
                let element_type = self
                    .types
                    .expression_type(id)
                    .map(|ty| layout_verification_type(self.hir, self.types, ty, 0))
                    .and_then(|ty| match ty {
                        VerificationType::List(element) => Some(*element),
                        _ => None,
                    })
                    .unwrap_or(VerificationType::Unknown);
                self.emit(
                    Instruction::MakeList {
                        destination,
                        element_type,
                        elements,
                    },
                    span,
                );
                Ok(destination)
            }
            hir::Expr::Name(ResolvedName::Local(local)) => self
                .locals
                .get(local)
                .copied()
                .ok_or_else(|| self.unsupported("captured local")),
            hir::Expr::Name(ResolvedName::Constant(constant)) => {
                self.constant_value(&self.hir.constants[*constant].value, span)
            }
            hir::Expr::Name(ResolvedName::Function(function)) => {
                let destination = self.allocate();
                let captures = self.function_captures(*function)?;
                self.emit(
                    Instruction::MakeClosure {
                        destination,
                        function: *function,
                        specialization: self.specialization(*function, &[], id),
                        captures,
                    },
                    span,
                );
                Ok(destination)
            }
            hir::Expr::Name(ResolvedName::Variant(variant)) => {
                if self.hir.variants[*variant].payload.is_some() {
                    return Err(self.unsupported("unapplied enum case constructor"));
                }
                let destination = self.allocate();
                self.emit(
                    Instruction::MakeVariant {
                        destination,
                        variant: *variant,
                        type_arguments: self.nominal_type_arguments(id),
                        payload: Vec::new(),
                    },
                    span,
                );
                Ok(destination)
            }
            hir::Expr::Unary { operator, operand } => {
                let operand = self.expression(*operand)?;
                let destination = self.allocate();
                self.emit(
                    Instruction::Unary {
                        destination,
                        operator: *operator,
                        operand,
                    },
                    span,
                );
                Ok(destination)
            }
            hir::Expr::Binary {
                left,
                operator,
                right,
            } => {
                let left = self.expression(*left)?;
                let right = self.expression(*right)?;
                let destination = self.allocate();
                self.emit(
                    Instruction::Binary {
                        destination,
                        operator: *operator,
                        left,
                        right,
                    },
                    span,
                );
                Ok(destination)
            }
            hir::Expr::Call { callee, arguments } => {
                if let hir::Expr::Name(ResolvedName::Variant(variant)) =
                    self.hir.expressions[*callee]
                {
                    let payload = arguments
                        .iter()
                        .map(|argument| self.expression(*argument))
                        .collect::<Result<Vec<_>, _>>()?;
                    let destination = self.allocate();
                    self.emit(
                        Instruction::MakeVariant {
                            destination,
                            variant,
                            type_arguments: self.nominal_type_arguments(id),
                            payload,
                        },
                        span,
                    );
                    return Ok(destination);
                }
                if let hir::Expr::Name(ResolvedName::Builtin(builtin)) =
                    self.hir.expressions[*callee]
                {
                    let arguments = arguments
                        .iter()
                        .map(|argument| self.expression(*argument))
                        .collect::<Result<Vec<_>, _>>()?;
                    let destination = self.allocate();
                    self.emit(
                        Instruction::Builtin {
                            destination,
                            builtin,
                            arguments,
                        },
                        span,
                    );
                    return Ok(destination);
                }
                if let hir::Expr::Member { object, name } = &self.hir.expressions[*callee] {
                    if name == "iterator" && self.sequence_type(*object) && arguments.is_empty() {
                        let source = self.expression(*object)?;
                        // Each iterator call materializes an independent cursor over
                        // a read-only snapshot of the sequence.
                        let snapshot = self.allocate();
                        self.emit(
                            Instruction::Move {
                                destination: snapshot,
                                source,
                            },
                            span.clone(),
                        );
                        let destination = self.allocate();
                        let module = self
                            .hir
                            .module_named("std.iter")
                            .ok_or_else(|| self.unsupported("std.iter module"))?;
                        let function = self
                            .hir
                            .function_named(module, "Iterator.from_sequence")
                            .ok_or_else(|| self.unsupported("Iterator.from_sequence"))?;
                        self.emit(
                            Instruction::Call {
                                destination,
                                function,
                                specialization: self.specialization(function, &[*object], id),
                                arguments: vec![snapshot],
                            },
                            span,
                        );
                        return Ok(destination);
                    }
                    if let Some(crate::types::ResolvedCall::Method { function, remote }) =
                        self.types.resolved_call(*callee)
                    {
                        let function = *function;
                        let remote = *remote;
                        let receiver_mode =
                            self.intrinsic(function).and_then(Intrinsic::receiver_mode);
                        let receiver = if remote
                            || matches!(
                                receiver_mode,
                                Some(IntrinsicReceiverMode::Read | IntrinsicReceiverMode::Consume)
                            )
                            || self.read_only_method(function)
                        {
                            self.expression(*object)?
                        } else {
                            self.method_receiver(*object)?
                        };
                        let mut specialization_arguments = vec![*object];
                        specialization_arguments.extend(arguments.iter().copied());
                        let specialization =
                            self.specialization(function, &specialization_arguments, id);
                        let mut arguments = arguments
                            .iter()
                            .map(|argument| self.expression(*argument))
                            .collect::<Result<Vec<_>, _>>()?;
                        let destination = self.allocate();
                        if self.lower_list_intrinsic(
                            function,
                            receiver,
                            &arguments,
                            destination,
                            span.clone(),
                        )? {
                            return Ok(destination);
                        }
                        if let Some(builtin) = self.intrinsic_builtin(function) {
                            arguments.insert(0, receiver);
                            self.emit(
                                Instruction::Builtin {
                                    destination,
                                    builtin,
                                    arguments,
                                },
                                span,
                            );
                            return Ok(destination);
                        }
                        let instruction = if remote {
                            let modes = self
                                .types
                                .function_type(function)
                                .ok_or_else(|| self.unsupported("remote method parameter modes"))?;
                            Instruction::RemoteCall {
                                destination,
                                remote: receiver,
                                function,
                                arguments: modes
                                    .parameter_modes
                                    .iter()
                                    .copied()
                                    .skip(1)
                                    .zip(arguments)
                                    .collect(),
                            }
                        } else {
                            Instruction::CallMethod {
                                destination,
                                receiver,
                                function,
                                specialization,
                                arguments,
                            }
                        };
                        self.emit(instruction, span);
                        return Ok(destination);
                    }
                    if let Some(crate::types::ResolvedCall::ContractMethod { slot, name, .. }) =
                        self.types.resolved_call(*callee)
                    {
                        let receiver = self.method_receiver(*object)?;
                        let arguments = arguments
                            .iter()
                            .map(|argument| self.expression(*argument))
                            .collect::<Result<Vec<_>, _>>()?;
                        let destination = self.allocate();
                        self.emit(
                            Instruction::CallContractMethod {
                                destination,
                                receiver,
                                slot: *slot,
                                name: name.clone(),
                                arguments,
                            },
                            span,
                        );
                        return Ok(destination);
                    }
                }
                let argument_expressions = arguments.clone();
                let arguments = arguments
                    .iter()
                    .map(|argument| self.expression(*argument))
                    .collect::<Result<Vec<_>, _>>()?;
                let destination = self.allocate();
                let resolved_function = self
                    .types
                    .resolved_call(*callee)
                    .and_then(crate::types::ResolvedCall::function);
                if let Some(function) = resolved_function {
                    if let Some((&receiver, method_arguments)) = arguments.split_first()
                        && self.lower_list_intrinsic(
                            function,
                            receiver,
                            method_arguments,
                            destination,
                            span.clone(),
                        )?
                    {
                        return Ok(destination);
                    }
                    if let Some(builtin) = self.intrinsic_builtin(function) {
                        self.emit(
                            Instruction::Builtin {
                                destination,
                                builtin,
                                arguments,
                            },
                            span,
                        );
                        return Ok(destination);
                    }
                    let captures = self.function_captures(function)?;
                    if captures.is_empty() {
                        self.emit(
                            Instruction::Call {
                                destination,
                                function,
                                specialization: self.specialization(
                                    function,
                                    &argument_expressions,
                                    id,
                                ),
                                arguments,
                            },
                            span,
                        );
                    } else {
                        self.emit(
                            Instruction::CallClosure {
                                destination,
                                function,
                                specialization: self.specialization(
                                    function,
                                    &argument_expressions,
                                    id,
                                ),
                                captures,
                                arguments,
                            },
                            span,
                        );
                    }
                } else {
                    let callee = self.expression(*callee)?;
                    self.emit(
                        Instruction::CallValue {
                            destination,
                            callee,
                            arguments,
                        },
                        span,
                    );
                }
                Ok(destination)
            }
            hir::Expr::Closure { function, captures } => {
                let captures = captures
                    .iter()
                    .map(|capture| {
                        self.locals
                            .get(&capture.local)
                            .copied()
                            .map(|register| (capture.mode, register))
                            .ok_or_else(|| self.unsupported("closure capture"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let destination = self.allocate();
                self.emit(
                    Instruction::MakeClosure {
                        destination,
                        function: *function,
                        specialization: self.specialization(*function, &[], id),
                        captures,
                    },
                    span,
                );
                Ok(destination)
            }
            hir::Expr::Index { object, index } => {
                let object = self.expression(*object)?;
                let index = self.expression(*index)?;
                let destination = self.allocate();
                self.emit(
                    Instruction::Index {
                        destination,
                        object,
                        index,
                    },
                    span,
                );
                Ok(destination)
            }
            hir::Expr::Reference(place) => {
                let Some(place) = crate::hir::queries::expression_place(self.hir, *place) else {
                    let object = self.expression(*place)?;
                    let pointee_type = self
                        .types
                        .expression_type(id)
                        .map(|ty| layout_verification_type(self.hir, self.types, ty, 0))
                        .and_then(|ty| match ty {
                            VerificationType::Reference(pointee) => Some(*pointee),
                            _ => None,
                        })
                        .unwrap_or(VerificationType::Unknown);
                    let destination = self.allocate();
                    self.emit(
                        Instruction::MakeWholeReference {
                            destination,
                            pointee_type,
                            object,
                        },
                        span,
                    );
                    return Ok(destination);
                };
                let mut object = self.locals[&place.root];
                let mut object_type = self
                    .types
                    .local_type(place.root)
                    .map(|ty| layout_verification_type(self.hir, self.types, ty, 0))
                    .unwrap_or(VerificationType::Unknown);
                if place.projections.is_empty() {
                    let pointee_type = match object_type {
                        VerificationType::Reference(pointee) => *pointee,
                        value => value,
                    };
                    let destination = self.allocate();
                    self.emit(
                        Instruction::MakeWholeReference {
                            destination,
                            pointee_type,
                            object,
                        },
                        span,
                    );
                    return Ok(destination);
                }
                for projection in place.projections {
                    if matches!(&projection, hir::Projection::Dereference) {
                        object_type = match object_type {
                            VerificationType::Reference(pointee) => *pointee,
                            _ => VerificationType::Unknown,
                        };
                        continue;
                    }
                    let destination = self.allocate();
                    let instruction = match projection {
                        hir::Projection::Field(field) => {
                            object_type = projected_field_verification_type(
                                self.hir,
                                self.types,
                                &object_type,
                                &field,
                            )
                            .unwrap_or(VerificationType::Unknown);
                            Instruction::MakeFieldReference {
                                destination,
                                pointee_type: object_type.clone(),
                                object,
                                field,
                            }
                        }
                        hir::Projection::Index {
                            expression: index, ..
                        } => {
                            object_type = object_type
                                .indexed_element()
                                .unwrap_or(VerificationType::Unknown);
                            Instruction::MakeReference {
                                destination,
                                pointee_type: object_type.clone(),
                                object,
                                index: self.expression(index)?,
                            }
                        }
                        hir::Projection::Dereference => unreachable!(),
                    };
                    self.emit(instruction, span.clone());
                    object = destination;
                }
                Ok(object)
            }
            hir::Expr::MoveOut(place) => {
                if let hir::Expr::Name(ResolvedName::Local(local)) = self.hir.expressions[*place] {
                    let source = self.locals[&local];
                    let destination = self.allocate();
                    self.emit(
                        Instruction::MoveOut {
                            destination,
                            source,
                        },
                        span,
                    );
                    Ok(destination)
                } else {
                    self.expression(*place)
                }
            }
            hir::Expr::Remote(value) => {
                if let hir::Expr::Reference(place) = self.hir.expressions[*value]
                    && let hir::Expr::Name(ResolvedName::Local(local)) = self.hir.expressions[place]
                {
                    let destination = self.allocate();
                    self.emit(
                        Instruction::SpawnRemoteBorrow {
                            destination,
                            source: self.locals[&local],
                        },
                        span,
                    );
                    return Ok(destination);
                }
                let value = self.expression(*value)?;
                let destination = self.allocate();
                self.emit(Instruction::SpawnRemote { destination, value }, span);
                Ok(destination)
            }
            hir::Expr::Await(future) => {
                let future = self.expression(*future)?;
                let destination = self.allocate();
                self.emit(
                    Instruction::Await {
                        destination,
                        future,
                    },
                    span,
                );
                Ok(destination)
            }
            hir::Expr::Try { value, binding } => {
                let source = self.expression(*value)?;
                let result_module = self.hir.module_named("core.result").ok_or_else(|| {
                    FosterError::runtime("`try` requires the embedded `core.result` module")
                })?;
                let result = self
                    .hir
                    .variant_type_named(result_module, "Result")
                    .ok_or_else(|| FosterError::runtime("`try` requires `core.result::Result`"))?;
                let ok = self.hir.variant_types[result]
                    .alternatives
                    .iter()
                    .copied()
                    .find(|variant| self.hir.variants[*variant].name == "Ok")
                    .ok_or_else(|| FosterError::runtime("`core.result::Result.Ok` is missing"))?;
                let unwrapped = self.allocate();
                self.locals.insert(*binding, unwrapped);
                let matched = self.allocate();
                self.emit(
                    Instruction::MatchPattern {
                        destination: matched,
                        subject: source,
                        pattern: hir::Pattern::Variant {
                            variant: ok,
                            fields: vec![hir::Pattern::Binding(*binding)],
                        },
                        bindings: vec![unwrapped],
                    },
                    span.clone(),
                );
                let failed = self.emit(
                    Instruction::JumpIfFalse {
                        condition: matched,
                        target: 0,
                    },
                    span.clone(),
                );
                let succeeded = self.emit(Instruction::Jump { target: 0 }, span.clone());
                self.patch_target(failed, self.instructions.len())?;
                self.emit(Instruction::Return { source }, span.clone());
                self.patch_target(succeeded, self.instructions.len())?;
                Ok(unwrapped)
            }
            hir::Expr::Record { record, fields } => {
                let values = fields.iter().cloned().collect::<HashMap<_, _>>();
                let mut layout = self.types.record_fields[record]
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>();
                layout.sort();
                let fields = layout
                    .iter()
                    .map(|name| {
                        let value = values
                            .get(name)
                            .ok_or_else(|| self.unsupported("record field layout"))?;
                        Ok((name.clone(), self.expression(*value)?))
                    })
                    .collect::<Result<Vec<_>, FosterError>>()?;
                let destination = self.allocate();
                self.emit(
                    Instruction::MakeRecord {
                        destination,
                        record: *record,
                        type_arguments: self.nominal_type_arguments(id),
                        fields,
                    },
                    span,
                );
                Ok(destination)
            }
            hir::Expr::Member { object, name } => {
                let object = self.expression(*object)?;
                let destination = self.allocate();
                self.emit(
                    Instruction::LoadField {
                        destination,
                        object,
                        field: name.clone(),
                        by_reference: false,
                    },
                    span,
                );
                Ok(destination)
            }
            hir::Expr::Branch { subject, arms } => self.branch(*subject, arms, span),
            _ => Err(self.unsupported("expression")),
        }
    }

    pub(super) fn store_place(
        &mut self,
        place: ExprId,
        source: Register,
        span: std::ops::Range<usize>,
    ) -> Result<(), FosterError> {
        match &self.hir.expressions[place] {
            hir::Expr::Name(ResolvedName::Local(local)) => {
                self.emit(
                    Instruction::Move {
                        destination: self.locals[local],
                        source,
                    },
                    span,
                );
            }
            hir::Expr::Member { object, name } => {
                let object = self.expression(*object)?;
                self.emit(
                    Instruction::StoreField {
                        object,
                        field: name.clone(),
                        source,
                    },
                    span,
                );
            }
            hir::Expr::Index { object, index } => {
                let object = self.expression(*object)?;
                let index = self.expression(*index)?;
                self.emit(
                    Instruction::StoreIndex {
                        object,
                        index,
                        source,
                    },
                    span,
                );
            }
            _ => return Err(self.unsupported("assignment place")),
        }
        Ok(())
    }

    fn method_receiver(&mut self, id: ExprId) -> Result<Register, FosterError> {
        let hir::Expr::Member { object, name } = &self.hir.expressions[id] else {
            return self.expression(id);
        };
        let span = self
            .hir
            .expression_spans
            .get(&id)
            .cloned()
            .unwrap_or_else(|| self.hir.functions[self.function].span.clone());
        let object = self.expression(*object)?;
        let destination = self.allocate();
        self.emit(
            Instruction::LoadField {
                destination,
                object,
                field: name.clone(),
                by_reference: true,
            },
            span,
        );
        Ok(destination)
    }

    fn lower_list_intrinsic(
        &mut self,
        function: hir::FunctionId,
        receiver: Register,
        arguments: &[Register],
        destination: Register,
        span: std::ops::Range<usize>,
    ) -> Result<bool, FosterError> {
        let [value] = arguments else {
            return if self
                .intrinsic(function)
                .is_some_and(Intrinsic::is_list_operation)
            {
                Err(self.unsupported("list intrinsic arity"))
            } else {
                Ok(false)
            };
        };
        let Some(intrinsic) = self.intrinsic(function).and_then(Intrinsic::opcode) else {
            return Ok(false);
        };
        self.emit(
            super::opcode_intrinsic_instruction(intrinsic, destination, receiver, *value),
            span,
        );
        Ok(true)
    }

    fn intrinsic(&self, function: hir::FunctionId) -> Option<Intrinsic> {
        self.hir.functions[function]
            .intrinsic
            .as_deref()
            .and_then(Intrinsic::from_key)
    }

    fn intrinsic_builtin(&self, function: hir::FunctionId) -> Option<Builtin> {
        self.intrinsic(function).and_then(Intrinsic::builtin)
    }

    fn sequence_type(&self, expression: ExprId) -> bool {
        self.types
            .expression_type(expression)
            .is_some_and(|ty| match self.types.types[ty] {
                crate::types::Type::RawList(_)
                | crate::types::Type::Sequence(_)
                | crate::types::Type::RawBytes => true,
                crate::types::Type::Record { record, .. } => {
                    matches!(
                        (
                            self.hir.modules[self.hir.records[record].module]
                                .name
                                .as_str(),
                            self.hir.records[record].name.as_str(),
                        ),
                        ("core.bytes", "Bytes") | ("core.list", "List")
                    )
                }
                _ => false,
            })
    }

    fn read_only_method(&self, function: hir::FunctionId) -> bool {
        let function = &self.hir.functions[function];
        !function.suspends
            && function
                .effects
                .iter()
                .all(|effect| effect.kind == crate::ast::EffectKind::Read)
    }

    fn function_captures(
        &self,
        function: hir::FunctionId,
    ) -> Result<Vec<(hir::CaptureMode, Register)>, FosterError> {
        self.closure_captures
            .get(&function)
            .into_iter()
            .flatten()
            .map(|capture| {
                self.locals
                    .get(&capture.local)
                    .copied()
                    .map(|register| (capture.mode, register))
                    .ok_or_else(|| self.unsupported("nested function capture"))
            })
            .collect()
    }

    pub(super) fn load_constant(
        &mut self,
        constant: Constant,
        span: std::ops::Range<usize>,
    ) -> Result<Register, FosterError> {
        let index = u16::try_from(self.constants.len())
            .map_err(|_| FosterError::runtime("VM constant table exceeds 65535 entries"))?;
        self.constants.push(constant);
        let destination = self.allocate();
        self.emit(
            Instruction::LoadConstant {
                destination,
                constant: index,
            },
            span,
        );
        Ok(destination)
    }

    fn constant_value(
        &mut self,
        value: &hir::ConstantValue,
        span: std::ops::Range<usize>,
    ) -> Result<Register, FosterError> {
        Ok(match value {
            hir::ConstantValue::Unit => self.load_constant(Constant::Unit, span)?,
            hir::ConstantValue::Bool(value) => self.load_constant(Constant::Bool(*value), span)?,
            hir::ConstantValue::Integer(value) => {
                self.load_constant(Constant::Integer(*value), span)?
            }
            hir::ConstantValue::Float(value) => {
                self.load_constant(Constant::Float(*value), span)?
            }
            hir::ConstantValue::String(value) => {
                self.load_constant(Constant::String(value.clone()), span)?
            }
            hir::ConstantValue::CodePoint(value) => {
                self.load_constant(Constant::CodePoint(*value), span)?
            }
            hir::ConstantValue::Symbol(value) => {
                self.load_constant(Constant::Symbol(value.clone()), span)?
            }
            hir::ConstantValue::Constant(constant) => {
                self.constant_value(&self.hir.constants[*constant].value, span)?
            }
            hir::ConstantValue::List(values) => {
                let elements = values
                    .iter()
                    .map(|value| self.constant_value(value, span.clone()))
                    .collect::<Result<Vec<_>, _>>()?;
                let destination = self.allocate();
                self.emit(
                    Instruction::MakeList {
                        destination,
                        element_type: VerificationType::Unknown,
                        elements,
                    },
                    span,
                );
                destination
            }
        })
    }

    pub(super) fn allocate(&mut self) -> Register {
        let register = Register(self.next_register);
        self.next_register = self
            .next_register
            .checked_add(1)
            .expect("VM register limit exceeded");
        register
    }

    fn specialization(
        &self,
        function: FunctionId,
        arguments: &[ExprId],
        result: ExprId,
    ) -> crate::vm::Specialization {
        let Some(signature) = self.types.function_type(function) else {
            return Vec::new();
        };
        let mut substitutions = std::collections::BTreeMap::new();
        for (schema, argument) in signature.parameters.iter().zip(arguments) {
            if let Some(actual) = self.types.expression_type(*argument) {
                match_generic_types(self.types, *schema, actual, &mut substitutions);
            }
        }
        if let Some(actual) = self.types.expression_type(result) {
            match_generic_types(self.types, signature.result, actual, &mut substitutions);
        }
        let mut names = std::collections::BTreeSet::new();
        for parameter in &signature.parameters {
            collect_generic_names(self.types, *parameter, &mut names);
        }
        collect_generic_names(self.types, signature.result, &mut names);
        for capture in self.closure_captures.get(&function).into_iter().flatten() {
            if let Some(ty) = self.types.local_type(capture.local) {
                collect_generic_names(self.types, ty, &mut names);
            }
        }
        names
            .into_iter()
            .map(|name| {
                let ty = substitutions.get(&name).map_or_else(
                    || VerificationType::Generic(name.clone()),
                    |ty| layout_verification_type(self.hir, self.types, *ty, 0),
                );
                (name, ty)
            })
            .collect()
    }

    fn nominal_type_arguments(&self, expression: ExprId) -> Vec<VerificationType> {
        let Some(ty) = self.types.expression_type(expression) else {
            return Vec::new();
        };
        let arguments = match &self.types.types[ty] {
            crate::types::Type::Record { arguments, .. }
            | crate::types::Type::Variant { arguments, .. } => arguments,
            _ => return Vec::new(),
        };
        arguments
            .iter()
            .map(|ty| layout_verification_type(self.hir, self.types, *ty, 0))
            .collect()
    }

    pub(super) fn emit(&mut self, instruction: Instruction, span: std::ops::Range<usize>) -> usize {
        let index = self.instructions.len();
        self.instructions.push(instruction);
        self.spans.push(span);
        index
    }

    pub(super) fn patch_target(
        &mut self,
        instruction: usize,
        target: usize,
    ) -> Result<(), FosterError> {
        match &mut self.instructions[instruction] {
            Instruction::Jump { target: found }
            | Instruction::JumpIfFalse { target: found, .. } => {
                *found = target;
                Ok(())
            }
            _ => Err(FosterError::runtime(
                "VM compiler attempted to patch a non-jump",
            )),
        }
    }

    fn branch(
        &mut self,
        subject: Option<ExprId>,
        arms: &[hir::BranchArm],
        span: std::ops::Range<usize>,
    ) -> Result<Register, FosterError> {
        let subject = subject
            .map(|subject| self.expression(subject))
            .transpose()?;
        let destination = self.allocate();
        let cfg = crate::control_flow::BranchCfg::new(arms);
        let mut labels = vec![None; cfg.node_count()];
        let mut pending = vec![Vec::new(); cfg.node_count()];

        for (node_id, node) in cfg.nodes() {
            self.begin_branch_node(node_id, &mut labels, &mut pending)?;
            match node {
                crate::control_flow::BranchNode::Test {
                    arm,
                    matched,
                    unmatched,
                } => match (&arms[arm].test, unmatched, subject) {
                    (hir::BranchTest::Condition(condition), Some(unmatched), None) => {
                        let condition = self.expression(*condition)?;
                        self.emit_branch_jump_if_false(
                            condition,
                            unmatched,
                            &labels,
                            &mut pending,
                            span.clone(),
                        );
                        self.emit_branch_jump(matched, &labels, &mut pending, span.clone());
                    }
                    (hir::BranchTest::Pattern(pattern), Some(unmatched), Some(subject)) => {
                        let mut bindings = Vec::new();
                        self.allocate_pattern_bindings(pattern, &mut bindings);
                        let condition = self.allocate();
                        self.emit(
                            Instruction::MatchPattern {
                                destination: condition,
                                subject,
                                pattern: pattern.clone(),
                                bindings,
                            },
                            span.clone(),
                        );
                        self.emit_branch_jump_if_false(
                            condition,
                            unmatched,
                            &labels,
                            &mut pending,
                            span.clone(),
                        );
                        self.emit_branch_jump(matched, &labels, &mut pending, span.clone());
                    }
                    (hir::BranchTest::Wildcard, None, _) => {
                        self.emit_branch_jump(matched, &labels, &mut pending, span.clone());
                    }
                    (hir::BranchTest::Condition(_), _, Some(_)) => {
                        return Err(self.unsupported("condition arm in subject branch"));
                    }
                    (hir::BranchTest::Pattern(_), _, None) => {
                        return Err(self.unsupported("pattern branch without a subject"));
                    }
                    _ => unreachable!("semantic branch CFG matches HIR tests"),
                },
                crate::control_flow::BranchNode::Body { arm, completed } => {
                    let value = self.compile_branch_body(&arms[arm], &span)?;
                    if let Some(completed) = completed {
                        self.emit(
                            Instruction::Move {
                                destination,
                                source: value,
                            },
                            span.clone(),
                        );
                        self.emit_branch_jump(completed, &labels, &mut pending, span.clone());
                    }
                }
                crate::control_flow::BranchNode::Exit => {}
            }
        }
        self.finish_branch_cfg(&pending)?;
        Ok(destination)
    }

    fn begin_branch_node(
        &mut self,
        node: crate::control_flow::NodeId,
        labels: &mut [Option<usize>],
        pending: &mut [Vec<usize>],
    ) -> Result<(), FosterError> {
        let offset = self.instructions.len();
        if labels[node.0].replace(offset).is_some() {
            return Err(FosterError::runtime(
                "VM compiler emitted a semantic branch node twice",
            ));
        }
        for instruction in pending[node.0].drain(..) {
            self.patch_target(instruction, offset)?;
        }
        Ok(())
    }

    fn emit_branch_jump(
        &mut self,
        target: crate::control_flow::NodeId,
        labels: &[Option<usize>],
        pending: &mut [Vec<usize>],
        span: std::ops::Range<usize>,
    ) {
        let resolved = labels[target.0].unwrap_or_default();
        let instruction = self.emit(Instruction::Jump { target: resolved }, span);
        if labels[target.0].is_none() {
            pending[target.0].push(instruction);
        }
    }

    fn emit_branch_jump_if_false(
        &mut self,
        condition: Register,
        target: crate::control_flow::NodeId,
        labels: &[Option<usize>],
        pending: &mut [Vec<usize>],
        span: std::ops::Range<usize>,
    ) {
        let resolved = labels[target.0].unwrap_or_default();
        let instruction = self.emit(
            Instruction::JumpIfFalse {
                condition,
                target: resolved,
            },
            span,
        );
        if labels[target.0].is_none() {
            pending[target.0].push(instruction);
        }
    }

    fn finish_branch_cfg(&self, pending: &[Vec<usize>]) -> Result<(), FosterError> {
        if pending.iter().all(Vec::is_empty) {
            Ok(())
        } else {
            Err(FosterError::runtime(
                "semantic branch CFG contains an unresolved edge",
            ))
        }
    }

    fn allocate_pattern_bindings(&mut self, pattern: &hir::Pattern, bindings: &mut Vec<Register>) {
        match pattern.unspanned() {
            hir::Pattern::Binding(local) => {
                let register = self.allocate();
                self.locals.insert(*local, register);
                bindings.push(register);
            }
            hir::Pattern::Variant { fields, .. } => {
                for field in fields {
                    self.allocate_pattern_bindings(field, bindings);
                }
            }
            _ => {}
        }
    }

    pub(super) fn unsupported(&self, kind: &str) -> FosterError {
        let function = &self.hir.functions[self.function];
        FosterError::runtime(format!(
            "VM lowering does not yet support {kind} in `{}`",
            function.name
        ))
    }
}
