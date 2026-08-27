use super::*;
use crate::hir::Builtin;

impl FunctionCompiler<'_> {
    pub(super) fn expression(&mut self, id: ExprId) -> Result<Register, FosterError> {
        self.expression_unwrapped(id)
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
                if self.hir.variants[*variant].payload.is_some() {
                    return Err(self.unsupported("unapplied enum case constructor"));
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
                    let raw_byte_buffer = self.types.expression_type(*object).is_some_and(|ty| {
                        matches!(self.types.types[ty], crate::types::Type::RawByteBuffer)
                    });
                    if (name == "push" && !raw_byte_buffer) || name == "append" || name == "in?" {
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
                    if let Some(function) = self.types.extension_methods.get(callee).copied() {
                        let receiver = self.method_receiver(*object)?;
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
                    if let Some(function) = self.primitive_method(*object, name) {
                        let receiver = self.expression(*object)?;
                        let mut arguments = arguments
                            .iter()
                            .map(|argument| self.expression(*argument))
                            .collect::<Result<Vec<_>, _>>()?;
                        let destination = self.allocate();
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
                        let receiver = self.method_receiver(*object)?;
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
                let Some(place) = crate::hir::queries::expression_place(self.hir, *place) else {
                    return Err(self.unsupported("reference to a non-place expression"));
                };
                let mut object = self.locals[&place.root];
                for projection in place.projections {
                    let destination = self.allocate();
                    let instruction = match projection {
                        hir::Projection::Field(field) => Instruction::MakeFieldReference {
                            destination,
                            object,
                            field,
                        },
                        hir::Projection::Index {
                            expression: index, ..
                        } => Instruction::MakeReference {
                            destination,
                            object,
                            index: self.expression(index)?,
                        },
                        hir::Projection::Dereference => continue,
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
                        fields,
                    },
                    span,
                );
                Ok(destination)
            }
            hir::Expr::Member { object, name } => {
                if name == "iterator" && self.sequence_type(*object) {
                    let source = self.expression(*object)?;
                    // `.iterator` promises an independent cursor over a read-only
                    // sequence view. Materialize that snapshot explicitly so the
                    // consuming constructor can transfer it without consuming the
                    // original collection.
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
                            arguments: vec![snapshot],
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
                if let Some((function, false)) = self.record_method(*object, name) {
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
                    let receiver = self.method_receiver(*object)?;
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
        if (name == "iterator" && self.sequence_type(*object))
            || self.primitive_method(*object, name).is_some()
            || self.contract_property(*object, name)
            || self.record_method(*object, name).is_some()
        {
            return self.expression(id);
        }
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
                    let qualified_name = format!("{}.{name}", self.hir.records[*record].name);
                    let function = self
                        .hir
                        .function_named(self.hir.records[*record].module, &qualified_name)?;
                    if self.hir.functions[function].receiver.is_none() {
                        return None;
                    }
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
                    let qualified_name =
                        format!("{}.{name}", self.hir.variant_types[*variant].name);
                    let function = self
                        .hir
                        .function_named(self.hir.variant_types[*variant].module, &qualified_name)?;
                    if self.hir.functions[function].receiver.is_none() {
                        return None;
                    }
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
            crate::types::Type::RawBytes => "core.bytes",
            crate::types::Type::RawByteBuffer => "core.bytes.buffer",
            _ => return None,
        };
        let module = self.hir.module_named(module)?;
        let qualified_name = match self.types.types[ty] {
            crate::types::Type::RawByteBuffer => format!("RawByteBuffer.{name}"),
            _ => name.to_owned(),
        };
        let function = self.hir.function_named(module, &qualified_name)?;
        self.hir.functions[function]
            .receiver
            .is_some()
            .then_some(function)
    }

    fn intrinsic_builtin(&self, function: hir::FunctionId) -> Option<Builtin> {
        let function = &self.hir.functions[function];
        let key = function.intrinsic.as_deref()?;
        match key {
            "byte.valid" => Some(Builtin::ByteValid),
            "byte.unchecked" => Some(Builtin::ByteUnchecked),
            "bytes.empty" => Some(Builtin::BytesEmpty),
            "bytes.from_list" => Some(Builtin::BytesFromList),
            "bytes.from_hex" => Some(Builtin::BytesFromHex),
            "bytes.concat" => Some(Builtin::BytesConcat),
            "bytes.slice" => Some(Builtin::BytesSlice),
            "bytes.to_list" => Some(Builtin::BytesToList),
            "bytes.hex" => Some(Builtin::BytesHex),
            "bytes.encode_utf8" => Some(Builtin::StringUtf8),
            "bytes.utf8_valid" => Some(Builtin::BytesUtf8Valid),
            "bytes.decode_utf8" => Some(Builtin::BytesDecodeUtf8),
            "byte_buffer.empty" => Some(Builtin::ByteBufferEmpty),
            "byte_buffer.with_capacity" => Some(Builtin::ByteBufferWithCapacity),
            "byte_buffer.push" => Some(Builtin::ByteBufferPush),
            "byte_buffer.extend" => Some(Builtin::ByteBufferExtend),
            "byte_buffer.clear" => Some(Builtin::ByteBufferClear),
            "byte_buffer.truncate" => Some(Builtin::ByteBufferTruncate),
            "byte_buffer.reserve" => Some(Builtin::ByteBufferReserve),
            "byte_buffer.freeze" => Some(Builtin::ByteBufferFreeze),
            "byte_buffer.snapshot" => Some(Builtin::ByteBufferSnapshot),
            "io.read_text" => Some(Builtin::IoReadText),
            "io.write_text" => Some(Builtin::IoWriteText),
            "io.read_bytes" => Some(Builtin::IoReadBytes),
            "io.write_bytes" => Some(Builtin::IoWriteBytes),
            "io.list_directory" => Some(Builtin::IoListDirectory),
            "io.exists" => Some(Builtin::IoExists),
            "io.is_file" => Some(Builtin::IoIsFile),
            "io.is_directory" => Some(Builtin::IoIsDirectory),
            "io.create_directory" => Some(Builtin::IoCreateDirectory),
            "io.create_directory_all" => Some(Builtin::IoCreateDirectoryAll),
            "io.remove_file" => Some(Builtin::IoRemoveFile),
            "io.remove_directory" => Some(Builtin::IoRemoveDirectory),
            "io.rename" => Some(Builtin::IoRename),
            "io.copy_file" => Some(Builtin::IoCopyFile),
            "io.join" => Some(Builtin::IoJoin),
            "io.parent" => Some(Builtin::IoParent),
            "io.file_name" => Some(Builtin::IoFileName),
            "io.extension" => Some(Builtin::IoExtension),
            "io.canonicalize" => Some(Builtin::IoCanonicalize),
            "io.current_directory" => Some(Builtin::IoCurrentDirectory),
            "tcp.listen" => Some(Builtin::TcpListen),
            "tcp.connect" => Some(Builtin::TcpConnect),
            "tcp.accept" => Some(Builtin::TcpAccept),
            "tcp.read" => Some(Builtin::TcpRead),
            "tcp.write" => Some(Builtin::TcpWrite),
            "tcp.read_bytes" => Some(Builtin::TcpReadBytes),
            "tcp.write_bytes" => Some(Builtin::TcpWriteBytes),
            "tcp.set_timeout" => Some(Builtin::TcpSetTimeout),
            "tcp.close_listener" => Some(Builtin::TcpCloseListener),
            "tcp.close_connection" => Some(Builtin::TcpCloseConnection),
            "float.format" => Some(Builtin::FormatFloat),
            _ => None,
        }
    }

    fn contract_property(&self, object: ExprId, name: &str) -> bool {
        self.contract_member(object, name, true)
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
