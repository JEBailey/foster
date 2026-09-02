use super::*;

pub(super) struct Reader<'a> {
    pub(super) bytes: &'a [u8],
    pub(super) offset: usize,
}
impl<'a> Reader<'a> {
    pub(super) fn take(&mut self, count: usize) -> Result<&'a [u8], BinaryError> {
        let end = self
            .offset
            .checked_add(count)
            .ok_or_else(|| BinaryError::new("offset overflow"))?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| BinaryError::new("truncated Foster bytecode"))?;
        self.offset = end;
        Ok(value)
    }
    pub(super) fn u8(&mut self) -> Result<u8, BinaryError> {
        Ok(self.take(1)?[0])
    }
    pub(super) fn u16(&mut self) -> Result<u16, BinaryError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub(super) fn u32(&mut self) -> Result<u32, BinaryError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub(super) fn u64(&mut self) -> Result<u64, BinaryError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub(super) fn count(&mut self) -> Result<usize, BinaryError> {
        let value = self.u32()? as usize;
        if value > MAX_ITEMS {
            Err(BinaryError::new("collection exceeds decoder limit"))
        } else {
            Ok(value)
        }
    }
    pub(super) fn id<T>(&mut self) -> Result<Idx<T>, BinaryError> {
        Ok(id(self.u32()?))
    }
    pub(super) fn reg(&mut self) -> Result<Register, BinaryError> {
        Ok(Register(self.u16()?))
    }
    pub(super) fn nominal_type(&mut self) -> Result<NominalTypeId, BinaryError> {
        match self.u8()? {
            0 => Ok(NominalTypeId::Record(self.id::<Record>()?)),
            1 => Ok(NominalTypeId::Variant(self.id::<VariantType>()?)),
            tag => Err(BinaryError::new(format!("invalid nominal type {tag}"))),
        }
    }
    pub(super) fn bool(&mut self) -> Result<bool, BinaryError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            tag => Err(BinaryError::new(format!("invalid boolean {tag}"))),
        }
    }
    pub(super) fn string(&mut self) -> Result<String, BinaryError> {
        let count = self.u32()? as usize;
        if count > MAX_STRING {
            return Err(BinaryError::new("string exceeds decoder limit"));
        }
        String::from_utf8(self.take(count)?.to_vec())
            .map_err(|_| BinaryError::new("invalid UTF-8 string"))
    }
    pub(super) fn option_id<T>(&mut self) -> Result<Option<Idx<T>>, BinaryError> {
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.id()?)),
            tag => Err(BinaryError::new(format!("invalid option tag {tag}"))),
        }
    }
    pub(super) fn vec<T>(
        &mut self,
        mut read: impl FnMut(&mut Self) -> Result<T, BinaryError>,
    ) -> Result<Vec<T>, BinaryError> {
        let count = self.count()?;
        (0..count).map(|_| read(self)).collect()
    }
    pub(super) fn map<K: Eq + std::hash::Hash, V>(
        &mut self,
        mut read: impl FnMut(&mut Self) -> Result<(K, V), BinaryError>,
    ) -> Result<HashMap<K, V>, BinaryError> {
        let count = self.count()?;
        let mut map = HashMap::with_capacity(count);
        for _ in 0..count {
            let (key, value) = read(self)?;
            if map.insert(key, value).is_some() {
                return Err(BinaryError::new("duplicate map key"));
            }
        }
        Ok(map)
    }
    pub(super) fn regs(&mut self) -> Result<Vec<Register>, BinaryError> {
        self.vec(|r| r.reg())
    }
    pub(super) fn constant(&mut self) -> Result<Constant, BinaryError> {
        Ok(match self.u8()? {
            0 => Constant::Unit,
            1 => Constant::Bool(self.bool()?),
            2 => Constant::Integer(self.u64()? as i64),
            3 => Constant::Float(f64::from_bits(self.u64()?)),
            4 => Constant::String(self.string()?),
            5 => Constant::CodePoint(
                char::from_u32(self.u32()?)
                    .ok_or_else(|| BinaryError::new("invalid Unicode scalar"))?,
            ),
            6 => Constant::Symbol(self.string()?),
            tag => return Err(BinaryError::new(format!("unknown constant tag {tag}"))),
        })
    }
    pub(super) fn range(&mut self) -> Result<Range<usize>, BinaryError> {
        Ok(self.u32()? as usize..self.u32()? as usize)
    }
    pub(super) fn function(&mut self) -> Result<BytecodeFunction, BinaryError> {
        Ok(BytecodeFunction {
            name: self.string()?,
            intrinsic_stub: self.bool()?,
            parameters: self.u16()?,
            parameter_types: self.vec(|r| r.verification_type(0))?,
            parameter_modes: self.vec(|r| r.parameter_mode())?,
            mutable_parameters: self.vec(|r| r.bool())?,
            returns_reference: self.bool()?,
            captures: self.u16()?,
            capture_types: self.vec(|r| r.verification_type(0))?,
            result_type: self.verification_type(0)?,
            registers: self.u16()?,
            instructions: self.vec(|r| r.instruction())?,
            instruction_spans: self.vec(|r| r.range())?,
        })
    }
    fn verification_type(&mut self, depth: usize) -> Result<VerificationType, BinaryError> {
        if depth >= 64 {
            return Err(BinaryError::new(
                "verification type nesting exceeds 64 levels",
            ));
        }
        let nested = |reader: &mut Self| reader.verification_type(depth + 1);
        Ok(match self.u8()? {
            0 => VerificationType::Unknown,
            1 => VerificationType::Unit,
            2 => VerificationType::Bool,
            3 => VerificationType::Integer,
            4 => VerificationType::Float,
            5 => VerificationType::CodePoint,
            6 => VerificationType::Byte,
            7 => VerificationType::Bytes,
            8 => VerificationType::ByteBuffer,
            9 => VerificationType::List(Box::new(nested(self)?)),
            10 => VerificationType::Reference(Box::new(nested(self)?)),
            11 => VerificationType::Remote(Box::new(nested(self)?)),
            12 => VerificationType::Future(Box::new(nested(self)?)),
            13 => VerificationType::Function {
                parameters: self.vec(|reader| nested(reader))?,
                parameter_modes: self.vec(|reader| reader.parameter_mode())?,
                result: Box::new(nested(self)?),
            },
            14 => VerificationType::Record(self.id::<Record>()?),
            15 => VerificationType::Variant(self.id::<VariantType>()?),
            16 => VerificationType::Union(self.vec(|reader| nested(reader))?),
            tag => {
                return Err(BinaryError::new(format!(
                    "unknown verification type tag {tag}"
                )));
            }
        })
    }
    pub(super) fn instruction(&mut self) -> Result<Instruction, BinaryError> {
        macro_rules! r {
            () => {
                self.reg()?
            };
        }
        macro_rules! id {
            ($t:ty) => {
                self.id::<$t>()?
            };
        }
        Ok(match self.u8()? {
            0 => Instruction::Drop { register: r!() },
            1 => Instruction::LoadConstant {
                destination: r!(),
                constant: self.u16()?,
            },
            2 => Instruction::Move {
                destination: r!(),
                source: r!(),
            },
            3 => Instruction::Unary {
                destination: r!(),
                operator: self.unary()?,
                operand: r!(),
            },
            4 => Instruction::Binary {
                destination: r!(),
                operator: self.binary()?,
                left: r!(),
                right: r!(),
            },
            5 => Instruction::MakeList {
                destination: r!(),
                elements: self.regs()?,
            },
            6 => Instruction::Index {
                destination: r!(),
                object: r!(),
                index: r!(),
            },
            7 => Instruction::MakeRecord {
                destination: r!(),
                record: id!(Record),
                fields: self.vec(|r| Ok((r.string()?, r.reg()?)))?,
            },
            8 => Instruction::MakeVariant {
                destination: r!(),
                variant: id!(Variant),
                payload: self.regs()?,
            },
            9 => Instruction::LoadField {
                destination: r!(),
                object: r!(),
                field: self.string()?,
                by_reference: self.u8()? != 0,
            },
            10 => Instruction::StoreField {
                object: r!(),
                field: self.string()?,
                source: r!(),
            },
            11 => Instruction::StoreIndex {
                object: r!(),
                index: r!(),
                source: r!(),
            },
            12 => Instruction::MakeReference {
                destination: r!(),
                object: r!(),
                index: r!(),
            },
            13 => Instruction::MoveOut {
                destination: r!(),
                source: r!(),
            },
            14 => Instruction::Push {
                destination: r!(),
                object: r!(),
                value: r!(),
            },
            15 => Instruction::Append {
                destination: r!(),
                object: r!(),
                value: r!(),
            },
            16 => Instruction::Contains {
                destination: r!(),
                value: r!(),
                candidates: self.regs()?,
            },
            17 => Instruction::Builtin {
                destination: r!(),
                builtin: self.builtin()?,
                arguments: self.regs()?,
            },
            18 => Instruction::SpawnRemote {
                destination: r!(),
                value: r!(),
            },
            19 => Instruction::SpawnRemoteBorrow {
                destination: r!(),
                source: r!(),
            },
            20 => Instruction::RemoteCall {
                destination: r!(),
                remote: r!(),
                function: id!(Function),
                arguments: self.vec(|r| Ok((r.parameter_mode()?, r.reg()?)))?,
            },
            21 => Instruction::Await {
                destination: r!(),
                future: r!(),
            },
            22 => Instruction::MatchPattern {
                destination: r!(),
                subject: r!(),
                pattern: self.pattern()?,
                bindings: self.regs()?,
            },
            23 => Instruction::Jump {
                target: self.u32()? as usize,
            },
            24 => Instruction::JumpIfFalse {
                condition: r!(),
                target: self.u32()? as usize,
            },
            25 => Instruction::Call {
                destination: r!(),
                function: id!(Function),
                arguments: self.regs()?,
            },
            26 => Instruction::CallMethod {
                destination: r!(),
                receiver: r!(),
                function: id!(Function),
                arguments: self.regs()?,
            },
            27 => Instruction::CallContractMethod {
                destination: r!(),
                receiver: r!(),
                slot: DispatchSlot(self.u32()?),
                name: self.string()?,
                arguments: self.regs()?,
            },
            28 => Instruction::MakeClosure {
                destination: r!(),
                function: id!(Function),
                captures: self.captures()?,
            },
            29 => Instruction::CallValue {
                destination: r!(),
                callee: r!(),
                arguments: self.regs()?,
            },
            30 => Instruction::CallClosure {
                destination: r!(),
                function: id!(Function),
                captures: self.captures()?,
                arguments: self.regs()?,
            },
            31 => Instruction::Return { source: r!() },
            32 => Instruction::MakeFieldReference {
                destination: r!(),
                object: r!(),
                field: self.string()?,
            },
            33 => Instruction::Assert {
                condition: r!(),
                message: match self.u8()? {
                    0 => None,
                    1 => Some(r!()),
                    tag => {
                        return Err(BinaryError::new(format!(
                            "invalid assertion-message option tag {tag}"
                        )));
                    }
                },
            },
            34 => Instruction::MakeWholeReference {
                destination: r!(),
                object: r!(),
            },
            tag => {
                return Err(BinaryError::new(format!(
                    "unknown instruction opcode {tag}"
                )));
            }
        })
    }
    pub(super) fn unary(&mut self) -> Result<UnaryOp, BinaryError> {
        Ok(match self.u8()? {
            0 => UnaryOp::Negate,
            1 => UnaryOp::Not,
            2 => UnaryOp::BitNot,
            t => return Err(BinaryError::new(format!("unknown unary operator {t}"))),
        })
    }
    pub(super) fn binary(&mut self) -> Result<BinaryOp, BinaryError> {
        Ok(match self.u8()? {
            0 => BinaryOp::Add,
            1 => BinaryOp::Subtract,
            2 => BinaryOp::Multiply,
            3 => BinaryOp::Divide,
            4 => BinaryOp::BitAnd,
            5 => BinaryOp::BitOr,
            6 => BinaryOp::BitXor,
            7 => BinaryOp::ShiftLeft,
            8 => BinaryOp::ShiftRight,
            9 => BinaryOp::Equal,
            10 => BinaryOp::NotEqual,
            11 => BinaryOp::Less,
            12 => BinaryOp::LessEqual,
            13 => BinaryOp::Greater,
            14 => BinaryOp::GreaterEqual,
            t => return Err(BinaryError::new(format!("unknown binary operator {t}"))),
        })
    }
    pub(super) fn parameter_mode(&mut self) -> Result<ParameterMode, BinaryError> {
        match self.u8()? {
            0 => Ok(ParameterMode::Borrow),
            1 => Ok(ParameterMode::Consume),
            t => Err(BinaryError::new(format!("unknown parameter mode {t}"))),
        }
    }
    pub(super) fn capture_mode(&mut self) -> Result<CaptureMode, BinaryError> {
        match self.u8()? {
            0 => Ok(CaptureMode::Copy),
            1 => Ok(CaptureMode::Move),
            2 => Ok(CaptureMode::Ref),
            t => Err(BinaryError::new(format!("unknown capture mode {t}"))),
        }
    }
    pub(super) fn captures(&mut self) -> Result<Vec<(CaptureMode, Register)>, BinaryError> {
        self.vec(|r| Ok((r.capture_mode()?, r.reg()?)))
    }
    pub(super) fn builtin(&mut self) -> Result<Builtin, BinaryError> {
        Builtin::from_bytecode_tag(self.u8()?)
            .ok_or_else(|| BinaryError::new("unknown builtin tag"))
    }
    pub(super) fn pattern(&mut self) -> Result<Pattern, BinaryError> {
        Ok(match self.u8()? {
            0 => Pattern::Spanned {
                pattern: Box::new(self.pattern()?),
                span: self.range()?,
            },
            1 => Pattern::Wildcard,
            2 => Pattern::Binding(self.id::<Local>()?),
            3 => Pattern::Bool(self.bool()?),
            4 => Pattern::Integer(self.u64()? as i64),
            5 => Pattern::Float(f64::from_bits(self.u64()?)),
            6 => Pattern::String(self.string()?),
            7 => Pattern::CodePoint(self.string()?),
            8 => Pattern::Symbol(self.string()?),
            9 => Pattern::Variant {
                variant: self.id::<Variant>()?,
                fields: self.vec(|r| r.pattern())?,
            },
            t => return Err(BinaryError::new(format!("unknown pattern tag {t}"))),
        })
    }
}
