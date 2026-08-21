use super::*;

impl FunctionCompiler<'_> {
    pub(super) fn expression(&mut self, id: ExprId) -> Result<Register, FosterError> {
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
                self.emit(
                    Instruction::MakeList {
                        destination,
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
                        captures,
                    },
                    span,
                );
                Ok(destination)
            }
            hir::Expr::Name(ResolvedName::Variant(variant)) => {
                if !self.hir.variants[*variant].payload.is_empty() {
                    return Err(self.unsupported("unapplied variant constructor"));
                }
                let destination = self.allocate();
                self.emit(
                    Instruction::MakeVariant {
                        destination,
                        variant: *variant,
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
                    if name == "push" || name == "append" || name == "in?" {
                        let object = self.expression(*object)?;
                        let arguments = arguments
                            .iter()
                            .map(|argument| self.expression(*argument))
                            .collect::<Result<Vec<_>, _>>()?;
                        let destination = self.allocate();
                        match (name.as_str(), arguments.as_slice()) {
                            ("push", [value]) => {
                                self.emit(
                                    Instruction::Push {
                                        destination,
                                        object,
                                        value: *value,
                                    },
                                    span,
                                );
                            }
                            ("append", [value]) => {
                                self.emit(
                                    Instruction::Append {
                                        destination,
                                        object,
                                        value: *value,
                                    },
                                    span,
                                );
                            }
                            ("in?", candidates) => {
                                self.emit(
                                    Instruction::Contains {
                                        destination,
                                        value: object,
                                        candidates: candidates.to_vec(),
                                    },
                                    span,
                                );
                            }
                            _ => return Err(self.unsupported("member call arity")),
                        }
                        return Ok(destination);
                    }
                    if let Some(function) = self.primitive_method(*object, name) {
                        let receiver = self.expression(*object)?;
                        let arguments = arguments
                            .iter()
                            .map(|argument| self.expression(*argument))
                            .collect::<Result<Vec<_>, _>>()?;
                        let destination = self.allocate();
                        self.emit(
                            Instruction::CallMethod {
                                destination,
                                receiver,
                                function,
                                arguments,
                            },
                            span,
                        );
                        return Ok(destination);
                    }
                    if let Some((function, remote)) = self.record_method(*object, name) {
                        let receiver = self.expression(*object)?;
                        let arguments = arguments
                            .iter()
                            .map(|argument| self.expression(*argument))
                            .collect::<Result<Vec<_>, _>>()?;
                        let destination = self.allocate();
                        if remote {
                            let modes = self
                                .types
                                .function_type(function)
                                .ok_or_else(|| self.unsupported("remote method parameter modes"))?;
                            let arguments = modes
                                .parameter_modes
                                .iter()
                                .copied()
                                .skip(1)
                                .zip(arguments)
                                .collect();
                            self.emit(
                                Instruction::RemoteCall {
                                    destination,
                                    remote: receiver,
                                    function,
                                    arguments,
                                },
                                span,
                            );
                        } else {
                            self.emit(
                                Instruction::CallMethod {
                                    destination,
                                    receiver,
                                    function,
                                    arguments,
                                },
                                span,
                            );
                        }
                        return Ok(destination);
                    }
                    if self.contract_method(*object, name) {
                        let receiver = self.expression(*object)?;
                        let arguments = arguments
                            .iter()
                            .map(|argument| self.expression(*argument))
                            .collect::<Result<Vec<_>, _>>()?;
                        let destination = self.allocate();
                        self.emit(
                            Instruction::CallContractMethod {
                                destination,
                                receiver,
                                name: name.clone(),
                                arguments,
                            },
                            span,
                        );
                        return Ok(destination);
                    }
                }
                let arguments = arguments
                    .iter()
                    .map(|argument| self.expression(*argument))
                    .collect::<Result<Vec<_>, _>>()?;
                let destination = self.allocate();
                if let hir::Expr::Name(ResolvedName::Function(function)) =
                    self.hir.expressions[*callee]
                {
                    let captures = self.function_captures(function)?;
                    if captures.is_empty() {
                        self.emit(
                            Instruction::Call {
                                destination,
                                function,
                                arguments,
                            },
                            span,
                        );
                    } else {
                        self.emit(
                            Instruction::CallClosure {
                                destination,
                                function,
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
                let hir::Expr::Index { object, index } = self.hir.expressions[*place] else {
                    return Err(self.unsupported("non-indexed reference"));
                };
                let hir::Expr::Name(ResolvedName::Local(local)) = self.hir.expressions[object]
                else {
                    return Err(self.unsupported("non-local reference origin"));
                };
                let object = self.locals[&local];
                let index = self.expression(index)?;
                let destination = self.allocate();
                self.emit(
                    Instruction::MakeReference {
                        destination,
                        object,
                        index,
                    },
                    span,
                );
                Ok(destination)
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
            hir::Expr::Record { record, fields } => {
                let fields = fields
                    .iter()
                    .map(|(name, value)| Ok((name.clone(), self.expression(*value)?)))
                    .collect::<Result<Vec<_>, FosterError>>()?;
                let destination = self.allocate();
                self.emit(
                    Instruction::MakeRecord {
                        destination,
                        record: *record,
                        fields,
                    },
                    span,
                );
                Ok(destination)
            }
            hir::Expr::Member { object, name } => {
                if name == "iterator" && self.sequence_type(*object) {
                    let source = self.expression(*object)?;
                    let destination = self.allocate();
                    let module = self
                        .hir
                        .module_named("core.iteration")
                        .ok_or_else(|| self.unsupported("core.iteration module"))?;
                    let function = self
                        .hir
                        .function_named(module, "Iterator.from_sequence")
                        .ok_or_else(|| self.unsupported("Iterator.from_sequence"))?;
                    self.emit(
                        Instruction::Call {
                            destination,
                            function,
                            arguments: vec![source],
                        },
                        span,
                    );
                    return Ok(destination);
                }
                if let Some(function) = self.primitive_method(*object, name) {
                    let receiver = self.expression(*object)?;
                    let destination = self.allocate();
                    self.emit(
                        Instruction::CallMethod {
                            destination,
                            receiver,
                            function,
                            arguments: Vec::new(),
                        },
                        span,
                    );
                    return Ok(destination);
                }
                if self.contract_property(*object, name) {
                    let receiver = self.expression(*object)?;
                    let destination = self.allocate();
                    if let Some((function, false)) = self.record_method(*object, name) {
                        self.emit(
                            Instruction::CallMethod {
                                destination,
                                receiver,
                                function,
                                arguments: Vec::new(),
                            },
                            span,
                        );
                    } else {
                        self.emit(
                            Instruction::CallContractMethod {
                                destination,
                                receiver,
                                name: name.clone(),
                                arguments: Vec::new(),
                            },
                            span,
                        );
                    }
                    return Ok(destination);
                }
                let object = self.expression(*object)?;
                let destination = self.allocate();
                self.emit(
                    Instruction::LoadField {
                        destination,
                        object,
                        field: name.clone(),
                    },
                    span,
                );
                Ok(destination)
            }
            hir::Expr::Branch {
                subject: None,
                arms,
            } => self.conditional_branch(arms, span),
            hir::Expr::Branch {
                subject: Some(subject),
                arms,
            } => self.pattern_branch(*subject, arms, span),
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

    fn record_method(&self, object: ExprId, name: &str) -> Option<(hir::FunctionId, bool)> {
        let mut ty = self.types.expression_type(object)?;
        let mut remote = false;
        loop {
            match &self.types.types[ty] {
                crate::types::Type::Remote(inner) => {
                    remote = true;
                    ty = *inner;
                }
                crate::types::Type::Reference { value, .. } => ty = *value,
                crate::types::Type::Record { record, .. } => {
                    let function = self
                        .hir
                        .function_named(self.hir.records[*record].module, name)?;
                    let receiver_matches = self
                        .types
                        .function_type(function)
                        .and_then(|signature| signature.parameters.first())
                        .is_some_and(|ty| {
                            matches!(
                                self.types.types[*ty],
                                crate::types::Type::Record { record: receiver, .. }
                                    if receiver == *record
                            )
                        });
                    return receiver_matches.then_some((function, remote));
                }
                crate::types::Type::Variant { variant, .. } => {
                    let function = self
                        .hir
                        .function_named(self.hir.variant_types[*variant].module, name)?;
                    let receiver_matches = self
                        .types
                        .function_type(function)
                        .and_then(|signature| signature.parameters.first())
                        .is_some_and(|ty| {
                            matches!(
                                self.types.types[*ty],
                                crate::types::Type::Variant { variant: receiver, .. }
                                    if receiver == *variant
                            )
                        });
                    return receiver_matches.then_some((function, remote));
                }
                _ => return None,
            }
        }
    }

    fn primitive_method(&self, object: ExprId, name: &str) -> Option<hir::FunctionId> {
        let ty = self.types.expression_type(object)?;
        let module = match self.types.types[ty] {
            crate::types::Type::Bytes => "core.bytes",
            crate::types::Type::ByteBuffer => "core.byte_buffer",
            _ => return None,
        };
        let module = self.hir.module_named(module)?;
        let function = self.hir.function_named(module, name)?;
        self.hir.functions[function]
            .parameters
            .first()
            .is_some_and(|parameter| self.hir.locals[*parameter].name == "self")
            .then_some(function)
    }

    fn contract_property(&self, object: ExprId, name: &str) -> bool {
        self.contract_member(object, name, true)
    }

    fn sequence_type(&self, expression: ExprId) -> bool {
        self.types.expression_type(expression).is_some_and(|ty| {
            matches!(
                self.types.types[ty],
                crate::types::Type::List(_)
                    | crate::types::Type::Sequence(_)
                    | crate::types::Type::Bytes
            )
        })
    }

    fn contract_method(&self, object: ExprId, name: &str) -> bool {
        self.contract_member(object, name, false)
    }

    fn contract_member(&self, object: ExprId, name: &str, property_only: bool) -> bool {
        let Some(ty) = self.types.expression_type(object) else {
            return false;
        };
        self.type_has_contract_member(ty, name, property_only)
    }

    fn type_has_contract_member(
        &self,
        ty: crate::types::TypeId,
        name: &str,
        property_only: bool,
    ) -> bool {
        match &self.types.types[ty] {
            crate::types::Type::Sequence(_) => {
                matches!(name, "empty?" | "length" | "head" | "rest")
            }
            crate::types::Type::Record { record, .. } => {
                let members = if property_only {
                    &self.types.record_properties
                } else {
                    &self.types.record_methods
                };
                members
                    .get(record)
                    .is_some_and(|methods| methods.contains(name))
            }
            crate::types::Type::Intersection(members) => members
                .iter()
                .any(|member| self.type_has_contract_member(*member, name, property_only)),
            _ => false,
        }
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

    fn conditional_branch(
        &mut self,
        arms: &[hir::BranchArm],
        span: std::ops::Range<usize>,
    ) -> Result<Register, FosterError> {
        let destination = self.allocate();
        let mut exits = Vec::new();
        for arm in arms {
            let skip = match arm.test {
                hir::BranchTest::Condition(condition) => {
                    let condition = self.expression(condition)?;
                    Some(self.emit(
                        Instruction::JumpIfFalse {
                            condition,
                            target: 0,
                        },
                        span.clone(),
                    ))
                }
                hir::BranchTest::Wildcard => None,
                hir::BranchTest::Pattern(_) => return Err(self.unsupported("pattern branch")),
            };
            let value = self.expression(arm.value)?;
            self.emit(
                Instruction::Move {
                    destination,
                    source: value,
                },
                span.clone(),
            );
            exits.push(self.emit(Instruction::Jump { target: 0 }, span.clone()));
            if let Some(skip) = skip {
                self.patch_target(skip, self.instructions.len())?;
            }
        }
        let end = self.instructions.len();
        for exit in exits {
            self.patch_target(exit, end)?;
        }
        Ok(destination)
    }

    fn pattern_branch(
        &mut self,
        subject: ExprId,
        arms: &[hir::BranchArm],
        span: std::ops::Range<usize>,
    ) -> Result<Register, FosterError> {
        let subject = self.expression(subject)?;
        let destination = self.allocate();
        let mut exits = Vec::new();
        for arm in arms {
            let skip = match &arm.test {
                hir::BranchTest::Pattern(pattern) => {
                    let mut bindings = Vec::new();
                    self.allocate_pattern_bindings(pattern, &mut bindings);
                    let matched = self.allocate();
                    self.emit(
                        Instruction::MatchPattern {
                            destination: matched,
                            subject,
                            pattern: pattern.clone(),
                            bindings,
                        },
                        span.clone(),
                    );
                    Some(self.emit(
                        Instruction::JumpIfFalse {
                            condition: matched,
                            target: 0,
                        },
                        span.clone(),
                    ))
                }
                hir::BranchTest::Wildcard => None,
                hir::BranchTest::Condition(_) => {
                    return Err(self.unsupported("condition arm in subject branch"));
                }
            };
            let value = self.expression(arm.value)?;
            self.emit(
                Instruction::Move {
                    destination,
                    source: value,
                },
                span.clone(),
            );
            exits.push(self.emit(Instruction::Jump { target: 0 }, span.clone()));
            if let Some(skip) = skip {
                self.patch_target(skip, self.instructions.len())?;
            }
        }
        let end = self.instructions.len();
        for exit in exits {
            self.patch_target(exit, end)?;
        }
        Ok(destination)
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
