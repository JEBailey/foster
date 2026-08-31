//! Portable, deterministic serialization for executable VM programs.

use std::collections::HashMap;
use std::fmt;
use std::ops::Range;

use la_arena::{Idx, RawIdx};

use super::{
    BytecodeFunction, Constant, Instruction, Program, Register, RuntimeRecord, RuntimeVariant,
    verify,
};
use crate::ast::{BinaryOp, ParameterMode, UnaryOp};
use crate::hir::{Builtin, CaptureMode, Function, Local, Pattern, Record, Variant, VariantType};
use crate::types::{DispatchSlot, NominalTypeId};

const MAGIC: &[u8; 8] = b"FOSTERBC";
pub const FORMAT_VERSION: u16 = 15;
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
    w.u8(u8::from(program.main_arguments));
    w.option_id(program.string_record);
    w.option_id(program.symbol_record);

    let mut records: Vec<_> = program.records.iter().collect();
    records.sort_by_key(|(id, _)| raw(**id));
    w.u32(records.len())?;
    for (id, record) in records {
        w.id(*id);
        w.string(&record.name)?;
        w.u32(record.layout.names().len())?;
        for field in record.layout.names() {
            w.string(field)?;
        }
    }

    let mut dispatch: Vec<_> = program.dispatch.iter().collect();
    dispatch.sort_by_key(|((nominal, slot), _)| (*nominal, *slot));
    w.u32(dispatch.len())?;
    for ((nominal, slot), function) in dispatch {
        w.nominal_type(*nominal);
        w.u32_value(slot.0);
        w.id(*function);
    }

    let mut variants: Vec<_> = program.variants.iter().collect();
    variants.sort_by_key(|(id, _)| raw(**id));
    w.u32(variants.len())?;
    for (id, variant) in variants {
        w.id(*id);
        w.id(variant.parent);
        w.string(&variant.type_name)?;
        w.string(&variant.alternative)?;
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
    let main_arguments = r.bool()?;
    let string_record = r.option_id::<Record>()?;
    let symbol_record = r.option_id::<Record>()?;
    let records = r.map(|r| {
        Ok((
            r.id::<Record>()?,
            RuntimeRecord {
                name: r.string()?,
                layout: std::sync::Arc::new(super::value::RecordLayout::new(
                    r.vec(|r| r.string())?,
                )),
            },
        ))
    })?;
    let dispatch = r.map(|r| {
        Ok((
            (r.nominal_type()?, DispatchSlot(r.u32()?)),
            r.id::<Function>()?,
        ))
    })?;
    let variants = r.map(|r| {
        Ok((
            r.id::<Variant>()?,
            RuntimeVariant {
                parent: r.id::<VariantType>()?,
                type_name: std::sync::Arc::from(r.string()?),
                alternative: std::sync::Arc::from(r.string()?),
            },
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
        main_arguments,
        string_record,
        symbol_record,
        records,
        dispatch,
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
    Builtin::FromCodePoint,
    Builtin::ParseFloat,
    Builtin::FormatFloat,
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
    Builtin::IoCreateDirectory,
    Builtin::IoCreateDirectoryAll,
    Builtin::IoRemoveFile,
    Builtin::IoRemoveDirectory,
    Builtin::IoRename,
    Builtin::IoCopyFile,
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
    Builtin::IoReadRange,
    Builtin::IoAppendBytes,
    Builtin::IoFileLength,
    Builtin::TimeWallNow,
    Builtin::TimeMonotonicNow,
];
fn builtin_tag(value: Builtin) -> u8 {
    BUILTINS.iter().position(|item| *item == value).unwrap() as u8
}
fn builtin_from_tag(tag: u8) -> Option<Builtin> {
    BUILTINS.get(tag as usize).copied()
}

mod read;
#[cfg(test)]
mod tests;
mod write;

use read::Reader;
use write::Writer;
