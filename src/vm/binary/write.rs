use super::*;

pub(super) struct Writer {
    pub(super) bytes: Vec<u8>,
}
impl Writer {
    pub(super) fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }
    pub(super) fn u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }
    pub(super) fn u32(&mut self, value: usize) -> Result<(), BinaryError> {
        let value = u32::try_from(value)
            .map_err(|_| BinaryError::new("collection exceeds format limit"))?;
        self.bytes.extend_from_slice(&value.to_le_bytes());
        Ok(())
    }
    pub(super) fn u32_value(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }
    pub(super) fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }
    pub(super) fn id<T>(&mut self, value: Idx<T>) {
        self.u32_value(raw(value));
    }
    pub(super) fn reg(&mut self, value: Register) {
        self.u16(value.0);
    }
    pub(super) fn nominal_type(&mut self, value: NominalTypeId) {
        match value {
            NominalTypeId::Record(record) => {
                self.u8(0);
                self.id(record);
            }
            NominalTypeId::Variant(variant) => {
                self.u8(1);
                self.id(variant);
            }
        }
    }
    pub(super) fn string(&mut self, value: &str) -> Result<(), BinaryError> {
        self.u32(value.len())?;
        self.bytes.extend_from_slice(value.as_bytes());
        Ok(())
    }
    pub(super) fn option_id<T>(&mut self, value: Option<Idx<T>>) {
        match value {
            Some(id) => {
                self.u8(1);
                self.id(id);
            }
            None => self.u8(0),
        }
    }
    pub(super) fn regs(&mut self, values: &[Register]) -> Result<(), BinaryError> {
        self.u32(values.len())?;
        for value in values {
            self.reg(*value);
        }
        Ok(())
    }
    pub(super) fn specialization(&mut self, values: &Specialization) -> Result<(), BinaryError> {
        self.u32(values.len())?;
        for (name, ty) in values {
            self.string(name)?;
            self.verification_type(ty)?;
        }
        Ok(())
    }
    pub(super) fn constant(&mut self, value: &Constant) -> Result<(), BinaryError> {
        match value {
            Constant::Unit => self.u8(0),
            Constant::Bool(v) => {
                self.u8(1);
                self.u8(*v as u8);
            }
            Constant::Integer(v) => {
                self.u8(2);
                self.u64(*v as u64);
            }
            Constant::Float(v) => {
                self.u8(3);
                self.u64(v.to_bits());
            }
            Constant::String(v) => {
                self.u8(4);
                self.string(v)?;
            }
            Constant::CodePoint(v) => {
                self.u8(5);
                self.u32_value(*v as u32);
            }
            Constant::Symbol(v) => {
                self.u8(6);
                self.string(v)?;
            }
        }
        Ok(())
    }
    pub(super) fn function(&mut self, f: &BytecodeFunction) -> Result<(), BinaryError> {
        self.string(&f.name)?;
        self.u8(u8::from(f.intrinsic_stub));
        self.u16(f.parameters);
        self.u32(f.parameter_types.len())?;
        for ty in &f.parameter_types {
            self.verification_type(ty)?;
        }
        self.u32(f.parameter_modes.len())?;
        for mode in &f.parameter_modes {
            self.parameter_mode(*mode);
        }
        self.u32(f.mutable_parameters.len())?;
        for value in &f.mutable_parameters {
            self.u8(*value as u8);
        }
        self.u8(u8::from(f.returns_reference));
        self.u16(f.captures);
        self.u32(f.capture_types.len())?;
        for ty in &f.capture_types {
            self.verification_type(ty)?;
        }
        self.verification_type(&f.result_type)?;
        self.u16(f.registers);
        self.u32(f.instructions.len())?;
        for instruction in &f.instructions {
            self.instruction(instruction)?;
        }
        self.u32(f.instruction_spans.len())?;
        for span in &f.instruction_spans {
            self.range(span)?;
        }
        Ok(())
    }
    pub(super) fn verification_type(&mut self, ty: &VerificationType) -> Result<(), BinaryError> {
        match ty {
            VerificationType::Unknown => self.u8(0),
            VerificationType::Generic(name) => {
                self.u8(17);
                self.string(name)?;
            }
            VerificationType::Unit => self.u8(1),
            VerificationType::Bool => self.u8(2),
            VerificationType::Integer => self.u8(3),
            VerificationType::Float => self.u8(4),
            VerificationType::CodePoint => self.u8(5),
            VerificationType::Byte => self.u8(6),
            VerificationType::Bytes => self.u8(7),
            VerificationType::ByteBuffer => self.u8(8),
            VerificationType::List(element) => {
                self.u8(9);
                self.verification_type(element)?;
            }
            VerificationType::Reference(value) => {
                self.u8(10);
                self.verification_type(value)?;
            }
            VerificationType::Remote(value) => {
                self.u8(11);
                self.verification_type(value)?;
            }
            VerificationType::Future(value) => {
                self.u8(12);
                self.verification_type(value)?;
            }
            VerificationType::Function {
                parameters,
                parameter_modes,
                result,
            } => {
                self.u8(13);
                self.u32(parameters.len())?;
                for parameter in parameters {
                    self.verification_type(parameter)?;
                }
                self.u32(parameter_modes.len())?;
                for mode in parameter_modes {
                    self.parameter_mode(*mode);
                }
                self.verification_type(result)?;
            }
            VerificationType::Record { record, arguments } => {
                self.u8(14);
                self.id(*record);
                self.u32(arguments.len())?;
                for argument in arguments {
                    self.verification_type(argument)?;
                }
            }
            VerificationType::Variant { variant, arguments } => {
                self.u8(15);
                self.id(*variant);
                self.u32(arguments.len())?;
                for argument in arguments {
                    self.verification_type(argument)?;
                }
            }
            VerificationType::Union(members) => {
                self.u8(16);
                self.u32(members.len())?;
                for member in members {
                    self.verification_type(member)?;
                }
            }
        }
        Ok(())
    }
    pub(super) fn range(&mut self, value: &Range<usize>) -> Result<(), BinaryError> {
        self.u32(value.start)?;
        self.u32(value.end)
    }
    pub(super) fn instruction(&mut self, i: &Instruction) -> Result<(), BinaryError> {
        match i {
            Instruction::Drop { register } => {
                self.u8(0);
                self.reg(*register);
            }
            Instruction::LoadConstant {
                destination,
                constant,
            } => {
                self.u8(1);
                self.reg(*destination);
                self.u16(*constant);
            }
            Instruction::Move {
                destination,
                source,
            } => {
                self.u8(2);
                self.reg(*destination);
                self.reg(*source);
            }
            Instruction::Unary {
                destination,
                operator,
                operand,
            } => {
                self.u8(3);
                self.reg(*destination);
                self.unary(*operator);
                self.reg(*operand);
            }
            Instruction::Binary {
                destination,
                operator,
                left,
                right,
            } => {
                self.u8(4);
                self.reg(*destination);
                self.binary(*operator);
                self.reg(*left);
                self.reg(*right);
            }
            Instruction::MakeList {
                destination,
                elements,
            } => {
                self.u8(5);
                self.reg(*destination);
                self.regs(elements)?;
            }
            Instruction::Index {
                destination,
                object,
                index,
            } => {
                self.u8(6);
                self.reg(*destination);
                self.reg(*object);
                self.reg(*index);
            }
            Instruction::MakeRecord {
                destination,
                record,
                type_arguments,
                fields,
            } => {
                self.u8(7);
                self.reg(*destination);
                self.id(*record);
                self.u32(type_arguments.len())?;
                for ty in type_arguments {
                    self.verification_type(ty)?;
                }
                self.u32(fields.len())?;
                for (name, value) in fields {
                    self.string(name)?;
                    self.reg(*value);
                }
            }
            Instruction::MakeVariant {
                destination,
                variant,
                type_arguments,
                payload,
            } => {
                self.u8(8);
                self.reg(*destination);
                self.id(*variant);
                self.u32(type_arguments.len())?;
                for ty in type_arguments {
                    self.verification_type(ty)?;
                }
                self.regs(payload)?;
            }
            Instruction::LoadField {
                destination,
                object,
                field,
                by_reference,
            } => {
                self.u8(9);
                self.reg(*destination);
                self.reg(*object);
                self.string(field)?;
                self.u8(u8::from(*by_reference));
            }
            Instruction::StoreField {
                object,
                field,
                source,
            } => {
                self.u8(10);
                self.reg(*object);
                self.string(field)?;
                self.reg(*source);
            }
            Instruction::StoreIndex {
                object,
                index,
                source,
            } => {
                self.u8(11);
                self.reg(*object);
                self.reg(*index);
                self.reg(*source);
            }
            Instruction::MakeReference {
                destination,
                object,
                index,
            } => {
                self.u8(12);
                self.reg(*destination);
                self.reg(*object);
                self.reg(*index);
            }
            Instruction::MakeFieldReference {
                destination,
                object,
                field,
            } => {
                self.u8(32);
                self.reg(*destination);
                self.reg(*object);
                self.string(field)?;
            }
            Instruction::MakeWholeReference {
                destination,
                object,
            } => {
                self.u8(34);
                self.reg(*destination);
                self.reg(*object);
            }
            Instruction::MoveOut {
                destination,
                source,
            } => {
                self.u8(13);
                self.reg(*destination);
                self.reg(*source);
            }
            Instruction::Push {
                destination,
                object,
                value,
            } => {
                self.u8(14);
                self.reg(*destination);
                self.reg(*object);
                self.reg(*value);
            }
            Instruction::Append {
                destination,
                object,
                value,
            } => {
                self.u8(15);
                self.reg(*destination);
                self.reg(*object);
                self.reg(*value);
            }
            Instruction::Contains {
                destination,
                value,
                candidates,
            } => {
                self.u8(16);
                self.reg(*destination);
                self.reg(*value);
                self.regs(candidates)?;
            }
            Instruction::Builtin {
                destination,
                builtin,
                arguments,
            } => {
                self.u8(17);
                self.reg(*destination);
                self.builtin(*builtin);
                self.regs(arguments)?;
            }
            Instruction::SpawnRemote { destination, value } => {
                self.u8(18);
                self.reg(*destination);
                self.reg(*value);
            }
            Instruction::SpawnRemoteBorrow {
                destination,
                source,
            } => {
                self.u8(19);
                self.reg(*destination);
                self.reg(*source);
            }
            Instruction::RemoteCall {
                destination,
                remote,
                function,
                arguments,
            } => {
                self.u8(20);
                self.reg(*destination);
                self.reg(*remote);
                self.id(*function);
                self.u32(arguments.len())?;
                for (mode, reg) in arguments {
                    self.parameter_mode(*mode);
                    self.reg(*reg);
                }
            }
            Instruction::Await {
                destination,
                future,
            } => {
                self.u8(21);
                self.reg(*destination);
                self.reg(*future);
            }
            Instruction::MatchPattern {
                destination,
                subject,
                pattern,
                bindings,
            } => {
                self.u8(22);
                self.reg(*destination);
                self.reg(*subject);
                self.pattern(pattern)?;
                self.regs(bindings)?;
            }
            Instruction::Jump { target } => {
                self.u8(23);
                self.u32(*target)?;
            }
            Instruction::JumpIfFalse { condition, target } => {
                self.u8(24);
                self.reg(*condition);
                self.u32(*target)?;
            }
            Instruction::Call {
                destination,
                function,
                specialization,
                arguments,
            } => {
                self.u8(25);
                self.reg(*destination);
                self.id(*function);
                self.specialization(specialization)?;
                self.regs(arguments)?;
            }
            Instruction::CallMethod {
                destination,
                receiver,
                function,
                specialization,
                arguments,
            } => {
                self.u8(26);
                self.reg(*destination);
                self.reg(*receiver);
                self.id(*function);
                self.specialization(specialization)?;
                self.regs(arguments)?;
            }
            Instruction::CallContractMethod {
                destination,
                receiver,
                slot,
                name,
                arguments,
            } => {
                self.u8(27);
                self.reg(*destination);
                self.reg(*receiver);
                self.u32_value(slot.0);
                self.string(name)?;
                self.regs(arguments)?;
            }
            Instruction::MakeClosure {
                destination,
                function,
                captures,
            } => {
                self.u8(28);
                self.reg(*destination);
                self.id(*function);
                self.captures(captures)?;
            }
            Instruction::CallValue {
                destination,
                callee,
                arguments,
            } => {
                self.u8(29);
                self.reg(*destination);
                self.reg(*callee);
                self.regs(arguments)?;
            }
            Instruction::CallClosure {
                destination,
                function,
                captures,
                arguments,
            } => {
                self.u8(30);
                self.reg(*destination);
                self.id(*function);
                self.captures(captures)?;
                self.regs(arguments)?;
            }
            Instruction::Return { source } => {
                self.u8(31);
                self.reg(*source);
            }
            Instruction::Assert { condition, message } => {
                self.u8(33);
                self.reg(*condition);
                match message {
                    Some(message) => {
                        self.u8(1);
                        self.reg(*message);
                    }
                    None => self.u8(0),
                }
            }
        }
        Ok(())
    }
    pub(super) fn unary(&mut self, value: UnaryOp) {
        self.u8(match value {
            UnaryOp::Negate => 0,
            UnaryOp::Not => 1,
            UnaryOp::BitNot => 2,
        });
    }
    pub(super) fn binary(&mut self, value: BinaryOp) {
        self.u8(match value {
            BinaryOp::Add => 0,
            BinaryOp::Subtract => 1,
            BinaryOp::Multiply => 2,
            BinaryOp::Divide => 3,
            BinaryOp::BitAnd => 4,
            BinaryOp::BitOr => 5,
            BinaryOp::BitXor => 6,
            BinaryOp::ShiftLeft => 7,
            BinaryOp::ShiftRight => 8,
            BinaryOp::Equal => 9,
            BinaryOp::NotEqual => 10,
            BinaryOp::Less => 11,
            BinaryOp::LessEqual => 12,
            BinaryOp::Greater => 13,
            BinaryOp::GreaterEqual => 14,
        });
    }
    pub(super) fn parameter_mode(&mut self, value: ParameterMode) {
        self.u8(match value {
            ParameterMode::Borrow => 0,
            ParameterMode::Consume => 1,
        });
    }
    pub(super) fn capture_mode(&mut self, value: CaptureMode) -> Result<(), BinaryError> {
        let tag = match value {
            CaptureMode::Copy => 0,
            CaptureMode::Move => 1,
            CaptureMode::Ref => 2,
            CaptureMode::Pending => {
                return Err(BinaryError::new("pending capture mode is not executable"));
            }
        };
        self.u8(tag);
        Ok(())
    }
    pub(super) fn captures(
        &mut self,
        values: &[(CaptureMode, Register)],
    ) -> Result<(), BinaryError> {
        self.u32(values.len())?;
        for (mode, reg) in values {
            self.capture_mode(*mode)?;
            self.reg(*reg);
        }
        Ok(())
    }
    pub(super) fn builtin(&mut self, value: Builtin) {
        self.u8(value.bytecode_tag());
    }
    pub(super) fn pattern(&mut self, value: &Pattern) -> Result<(), BinaryError> {
        match value {
            Pattern::Spanned { pattern, span } => {
                self.u8(0);
                self.pattern(pattern)?;
                self.range(span)?;
            }
            Pattern::Wildcard => self.u8(1),
            Pattern::Binding(local) => {
                self.u8(2);
                self.id(*local);
            }
            Pattern::Bool(v) => {
                self.u8(3);
                self.u8(*v as u8);
            }
            Pattern::Integer(v) => {
                self.u8(4);
                self.u64(*v as u64);
            }
            Pattern::Float(v) => {
                self.u8(5);
                self.u64(v.to_bits());
            }
            Pattern::String(v) => {
                self.u8(6);
                self.string(v)?;
            }
            Pattern::CodePoint(v) => {
                self.u8(7);
                self.string(v)?;
            }
            Pattern::Symbol(v) => {
                self.u8(8);
                self.string(v)?;
            }
            Pattern::Variant { variant, fields } => {
                self.u8(9);
                self.id(*variant);
                self.u32(fields.len())?;
                for field in fields {
                    self.pattern(field)?;
                }
            }
        }
        Ok(())
    }
}
