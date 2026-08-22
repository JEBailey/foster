//! Portable, deterministic serialization for executable VM programs.

use std::collections::HashMap;
use std::fmt;
use std::ops::Range;

use la_arena::{Idx, RawIdx};

use super::{BytecodeFunction, Constant, Instruction, Program, Register, verify};
use crate::ast::{BinaryOp, ParameterMode, UnaryOp};
use crate::hir::{Builtin, CaptureMode, Function, Local, Pattern, Record, Variant, VariantType};

const MAGIC: &[u8; 8] = b"FOSTERBC";
pub const FORMAT_VERSION: u16 = 4;
const MAX_ITEMS: usize = 16_777_216;
const MAX_STRING: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryError {
    message: String,
}

impl BinaryError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for BinaryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for BinaryError {}

/// Encodes a program using the canonical ordering defined by `docs/binary-format.md`.
pub fn encode_program(program: &Program) -> Result<Vec<u8>, BinaryError> {
    verify(program).map_err(|error| BinaryError::new(format!("invalid program: {error}")))?;
    let mut w = Writer { bytes: Vec::new() };
    w.bytes.extend_from_slice(MAGIC);
    w.u16(FORMAT_VERSION);
    w.u16(0); // flags
    w.u32(program.constants.len())?;
    for value in &program.constants {
        w.constant(value)?;
    }

    let mut functions: Vec<_> = program.functions.iter().collect();
    functions.sort_by_key(|(id, _)| raw(**id));
    w.u32(functions.len())?;
    for (id, function) in functions {
        w.id(*id);
        w.function(function)?;
    }
    w.option_id(program.main);
    w.option_id(program.string_record);
    w.option_id(program.symbol_record);

    let mut records: Vec<_> = program.records.iter().collect();
    records.sort_by_key(|(id, _)| raw(**id));
    w.u32(records.len())?;
    for (id, name) in records {
        w.id(*id);
        w.string(name)?;
        let layout = program
            .record_layouts
            .get(id)
            .ok_or_else(|| BinaryError::new("record is missing its field layout"))?;
        w.u32(layout.names().len())?;
        for field in layout.names() {
            w.string(field)?;
        }
    }

    let mut methods: Vec<_> = program.methods.iter().collect();
    methods.sort_by(|((a, an), _), ((b, bn), _)| (raw(*a), an).cmp(&(raw(*b), bn)));
    w.u32(methods.len())?;
    for ((record, name), function) in methods {
        w.id(*record);
        w.string(name)?;
        w.id(*function);
    }

    let mut variant_methods: Vec<_> = program.variant_methods.iter().collect();
    variant_methods.sort_by(|((a, an), _), ((b, bn), _)| (raw(*a), an).cmp(&(raw(*b), bn)));
    w.u32(variant_methods.len())?;
    for ((variant, name), function) in variant_methods {
        w.id(*variant);
        w.string(name)?;
        w.id(*function);
    }

    let mut variants: Vec<_> = program.variants.iter().collect();
    variants.sort_by_key(|(id, _)| raw(**id));
    w.u32(variants.len())?;
    for (id, (parent, ty, alternative)) in variants {
        w.id(*id);
        w.id(*parent);
        w.string(ty)?;
        w.string(alternative)?;
    }
    Ok(w.bytes)
}

/// Decodes, bounds-checks, and verifies a serialized program.
pub fn decode_program(bytes: &[u8]) -> Result<Program, BinaryError> {
    let mut r = Reader { bytes, offset: 0 };
    if r.take(8)? != MAGIC {
        return Err(BinaryError::new("not a Foster bytecode file"));
    }
    let version = r.u16()?;
    if version != FORMAT_VERSION {
        return Err(BinaryError::new(format!(
            "unsupported Foster bytecode version {version}"
        )));
    }
    let flags = r.u16()?;
    if flags != 0 {
        return Err(BinaryError::new(format!(
            "unsupported Foster bytecode flags 0x{flags:04x}"
        )));
    }
    let constants = r.vec(|r| r.constant())?;
    let functions = r.map(|r| Ok((r.id::<Function>()?, r.function()?)))?;
    let main = r.option_id::<Function>()?;
    let string_record = r.option_id::<Record>()?;
    let symbol_record = r.option_id::<Record>()?;
    let record_entries = r.vec(|r| {
        Ok((
            r.id::<Record>()?,
            r.string()?,
            std::sync::Arc::new(super::value::RecordLayout::new(r.vec(|r| r.string())?)),
        ))
    })?;
    let records = record_entries
        .iter()
        .map(|(id, name, _)| (*id, name.clone()))
        .collect();
    let record_layouts = record_entries
        .into_iter()
        .map(|(id, _, fields)| (id, fields))
        .collect();
    let methods = r.map(|r| Ok(((r.id::<Record>()?, r.string()?), r.id::<Function>()?)))?;
    let variant_methods =
        r.map(|r| Ok(((r.id::<VariantType>()?, r.string()?), r.id::<Function>()?)))?;
    let variants = r.map(|r| {
        Ok((
            r.id::<Variant>()?,
            (
                r.id::<VariantType>()?,
                std::sync::Arc::from(r.string()?),
                std::sync::Arc::from(r.string()?),
            ),
        ))
    })?;
    if r.offset != bytes.len() {
        return Err(BinaryError::new(
            "trailing bytes after Foster bytecode program",
        ));
    }
    let program = Program {
        constants,
        functions,
        main,
        string_record,
        symbol_record,
        records,
        record_layouts,
        methods,
        variant_methods,
        variants,
    };
    verify(&program)
        .map_err(|error| BinaryError::new(format!("invalid Foster bytecode: {error}")))?;
    Ok(program)
}

fn raw<T>(id: Idx<T>) -> u32 {
    id.into_raw().into_u32()
}
fn id<T>(value: u32) -> Idx<T> {
    Idx::from_raw(RawIdx::from_u32(value))
}

const BUILTINS: &[Builtin] = &[
    Builtin::Print,
    Builtin::Println,
    Builtin::CodePoint,
    Builtin::FromCodePoint,
    Builtin::ParseFloat,
    Builtin::ByteValid,
    Builtin::ByteUnchecked,
    Builtin::BytesEmpty,
    Builtin::BytesFromList,
    Builtin::BytesConcat,
    Builtin::BytesSlice,
    Builtin::BytesToList,
    Builtin::BytesHex,
    Builtin::BytesFromHex,
    Builtin::StringUtf8,
    Builtin::BytesUtf8Valid,
    Builtin::BytesDecodeUtf8,
    Builtin::ByteBufferEmpty,
    Builtin::ByteBufferWithCapacity,
    Builtin::ByteBufferPush,
    Builtin::ByteBufferExtend,
    Builtin::ByteBufferClear,
    Builtin::ByteBufferTruncate,
    Builtin::ByteBufferReserve,
    Builtin::ByteBufferFreeze,
    Builtin::ByteBufferSnapshot,
    Builtin::IoReadText,
    Builtin::IoWriteText,
    Builtin::IoReadBytes,
    Builtin::IoWriteBytes,
    Builtin::IoListDirectory,
    Builtin::IoExists,
    Builtin::IoIsFile,
    Builtin::IoIsDirectory,
    Builtin::IoJoin,
    Builtin::IoParent,
    Builtin::IoFileName,
    Builtin::IoExtension,
    Builtin::IoCanonicalize,
    Builtin::IoCurrentDirectory,
    Builtin::TcpListen,
    Builtin::TcpConnect,
    Builtin::TcpAccept,
    Builtin::TcpRead,
    Builtin::TcpWrite,
    Builtin::TcpReadBytes,
    Builtin::TcpWriteBytes,
    Builtin::TcpSetTimeout,
    Builtin::TcpCloseListener,
    Builtin::TcpCloseConnection,
];
fn builtin_tag(value: Builtin) -> u8 {
    BUILTINS.iter().position(|item| *item == value).unwrap() as u8
}
fn builtin_from_tag(tag: u8) -> Option<Builtin> {
    BUILTINS.get(tag as usize).copied()
}

struct Writer {
    bytes: Vec<u8>,
}
impl Writer {
    fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }
    fn u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }
    fn u32(&mut self, value: usize) -> Result<(), BinaryError> {
        let value = u32::try_from(value)
            .map_err(|_| BinaryError::new("collection exceeds format limit"))?;
        self.bytes.extend_from_slice(&value.to_le_bytes());
        Ok(())
    }
    fn u32_value(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }
    fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }
    fn id<T>(&mut self, value: Idx<T>) {
        self.u32_value(raw(value));
    }
    fn reg(&mut self, value: Register) {
        self.u16(value.0);
    }
    fn string(&mut self, value: &str) -> Result<(), BinaryError> {
        self.u32(value.len())?;
        self.bytes.extend_from_slice(value.as_bytes());
        Ok(())
    }
    fn option_id<T>(&mut self, value: Option<Idx<T>>) {
        match value {
            Some(id) => {
                self.u8(1);
                self.id(id);
            }
            None => self.u8(0),
        }
    }
    fn regs(&mut self, values: &[Register]) -> Result<(), BinaryError> {
        self.u32(values.len())?;
        for value in values {
            self.reg(*value);
        }
        Ok(())
    }
    fn constant(&mut self, value: &Constant) -> Result<(), BinaryError> {
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
    fn function(&mut self, f: &BytecodeFunction) -> Result<(), BinaryError> {
        self.string(&f.name)?;
        self.u16(f.parameters);
        self.u32(f.parameter_modes.len())?;
        for mode in &f.parameter_modes {
            self.parameter_mode(*mode);
        }
        self.u32(f.mutable_parameters.len())?;
        for value in &f.mutable_parameters {
            self.u8(*value as u8);
        }
        self.u16(f.captures);
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
    fn range(&mut self, value: &Range<usize>) -> Result<(), BinaryError> {
        self.u32(value.start)?;
        self.u32(value.end)
    }
    fn instruction(&mut self, i: &Instruction) -> Result<(), BinaryError> {
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
                fields,
            } => {
                self.u8(7);
                self.reg(*destination);
                self.id(*record);
                self.u32(fields.len())?;
                for (name, value) in fields {
                    self.string(name)?;
                    self.reg(*value);
                }
            }
            Instruction::MakeVariant {
                destination,
                variant,
                payload,
            } => {
                self.u8(8);
                self.reg(*destination);
                self.id(*variant);
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
                arguments,
            } => {
                self.u8(25);
                self.reg(*destination);
                self.id(*function);
                self.regs(arguments)?;
            }
            Instruction::CallMethod {
                destination,
                receiver,
                function,
                arguments,
            } => {
                self.u8(26);
                self.reg(*destination);
                self.reg(*receiver);
                self.id(*function);
                self.regs(arguments)?;
            }
            Instruction::CallContractMethod {
                destination,
                receiver,
                name,
                arguments,
            } => {
                self.u8(27);
                self.reg(*destination);
                self.reg(*receiver);
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
        }
        Ok(())
    }
    fn unary(&mut self, value: UnaryOp) {
        self.u8(match value {
            UnaryOp::Negate => 0,
            UnaryOp::Not => 1,
            UnaryOp::BitNot => 2,
        });
    }
    fn binary(&mut self, value: BinaryOp) {
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
    fn parameter_mode(&mut self, value: ParameterMode) {
        self.u8(match value {
            ParameterMode::Borrow => 0,
            ParameterMode::Consume => 1,
        });
    }
    fn capture_mode(&mut self, value: CaptureMode) -> Result<(), BinaryError> {
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
    fn captures(&mut self, values: &[(CaptureMode, Register)]) -> Result<(), BinaryError> {
        self.u32(values.len())?;
        for (mode, reg) in values {
            self.capture_mode(*mode)?;
            self.reg(*reg);
        }
        Ok(())
    }
    fn builtin(&mut self, value: Builtin) {
        self.u8(builtin_tag(value));
    }
    fn pattern(&mut self, value: &Pattern) -> Result<(), BinaryError> {
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

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], BinaryError> {
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
    fn u8(&mut self) -> Result<u8, BinaryError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, BinaryError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, BinaryError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, BinaryError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn count(&mut self) -> Result<usize, BinaryError> {
        let value = self.u32()? as usize;
        if value > MAX_ITEMS {
            Err(BinaryError::new("collection exceeds decoder limit"))
        } else {
            Ok(value)
        }
    }
    fn id<T>(&mut self) -> Result<Idx<T>, BinaryError> {
        Ok(id(self.u32()?))
    }
    fn reg(&mut self) -> Result<Register, BinaryError> {
        Ok(Register(self.u16()?))
    }
    fn bool(&mut self) -> Result<bool, BinaryError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            tag => Err(BinaryError::new(format!("invalid boolean {tag}"))),
        }
    }
    fn string(&mut self) -> Result<String, BinaryError> {
        let count = self.u32()? as usize;
        if count > MAX_STRING {
            return Err(BinaryError::new("string exceeds decoder limit"));
        }
        String::from_utf8(self.take(count)?.to_vec())
            .map_err(|_| BinaryError::new("invalid UTF-8 string"))
    }
    fn option_id<T>(&mut self) -> Result<Option<Idx<T>>, BinaryError> {
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.id()?)),
            tag => Err(BinaryError::new(format!("invalid option tag {tag}"))),
        }
    }
    fn vec<T>(
        &mut self,
        mut read: impl FnMut(&mut Self) -> Result<T, BinaryError>,
    ) -> Result<Vec<T>, BinaryError> {
        let count = self.count()?;
        (0..count).map(|_| read(self)).collect()
    }
    fn map<K: Eq + std::hash::Hash, V>(
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
    fn regs(&mut self) -> Result<Vec<Register>, BinaryError> {
        self.vec(|r| r.reg())
    }
    fn constant(&mut self) -> Result<Constant, BinaryError> {
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
    fn range(&mut self) -> Result<Range<usize>, BinaryError> {
        Ok(self.u32()? as usize..self.u32()? as usize)
    }
    fn function(&mut self) -> Result<BytecodeFunction, BinaryError> {
        Ok(BytecodeFunction {
            name: self.string()?,
            parameters: self.u16()?,
            parameter_modes: self.vec(|r| r.parameter_mode())?,
            mutable_parameters: self.vec(|r| r.bool())?,
            captures: self.u16()?,
            registers: self.u16()?,
            instructions: self.vec(|r| r.instruction())?,
            instruction_spans: self.vec(|r| r.range())?,
        })
    }
    fn instruction(&mut self) -> Result<Instruction, BinaryError> {
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
            tag => {
                return Err(BinaryError::new(format!(
                    "unknown instruction opcode {tag}"
                )));
            }
        })
    }
    fn unary(&mut self) -> Result<UnaryOp, BinaryError> {
        Ok(match self.u8()? {
            0 => UnaryOp::Negate,
            1 => UnaryOp::Not,
            2 => UnaryOp::BitNot,
            t => return Err(BinaryError::new(format!("unknown unary operator {t}"))),
        })
    }
    fn binary(&mut self) -> Result<BinaryOp, BinaryError> {
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
    fn parameter_mode(&mut self) -> Result<ParameterMode, BinaryError> {
        match self.u8()? {
            0 => Ok(ParameterMode::Borrow),
            1 => Ok(ParameterMode::Consume),
            t => Err(BinaryError::new(format!("unknown parameter mode {t}"))),
        }
    }
    fn capture_mode(&mut self) -> Result<CaptureMode, BinaryError> {
        match self.u8()? {
            0 => Ok(CaptureMode::Copy),
            1 => Ok(CaptureMode::Move),
            2 => Ok(CaptureMode::Ref),
            t => Err(BinaryError::new(format!("unknown capture mode {t}"))),
        }
    }
    fn captures(&mut self) -> Result<Vec<(CaptureMode, Register)>, BinaryError> {
        self.vec(|r| Ok((r.capture_mode()?, r.reg()?)))
    }
    fn builtin(&mut self) -> Result<Builtin, BinaryError> {
        builtin_from_tag(self.u8()?).ok_or_else(|| BinaryError::new("unknown builtin tag"))
    }
    fn pattern(&mut self) -> Result<Pattern, BinaryError> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::{Machine, compile};

    #[test]
    fn round_trips_and_executes_compiled_program() {
        let source = "type Choice = | Left(Int) | Right(Int)\n\
            func unwrap(value: Choice) -> Int { branch value { Choice.Left(number) -> number _ -> 0 } }\n\
            func main() -> Int {\n values = [20, 22]\n unwrap(Choice.Left(values[0] + values[1]))\n }";
        let compilation = crate::compile(source).unwrap();
        let program = compile(&compilation).unwrap();
        let bytes = encode_program(&program).unwrap();
        let decoded = decode_program(&bytes).unwrap();
        assert_eq!(program, decoded);
        assert_eq!(
            Machine::new(&decoded).run_main().unwrap(),
            crate::vm::Value::Integer(42)
        );
        assert_eq!(bytes, encode_program(&decoded).unwrap());
    }

    #[test]
    fn rejects_invalid_envelopes() {
        assert!(decode_program(b"not bytecode").is_err());
        let compilation = crate::compile("func main() -> Int { 42 }").unwrap();
        let program = compile(&compilation).unwrap();
        let mut bytes = encode_program(&program).unwrap();
        bytes[8] = 99;
        assert!(
            decode_program(&bytes)
                .unwrap_err()
                .to_string()
                .contains("version")
        );
    }
}
