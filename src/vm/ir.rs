use std::collections::HashMap;
use std::ops::Range;

use crate::ast::{BinaryOp, UnaryOp};
use crate::hir::{Builtin, FunctionId, RecordId, VariantId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Register(pub u16);

#[derive(Debug, Clone, PartialEq)]
pub enum Constant {
    Unit,
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(String),
    Symbol(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Instruction {
    LoadConstant {
        destination: Register,
        constant: u16,
    },
    Move {
        destination: Register,
        source: Register,
    },
    Unary {
        destination: Register,
        operator: UnaryOp,
        operand: Register,
    },
    Binary {
        destination: Register,
        operator: BinaryOp,
        left: Register,
        right: Register,
    },
    MakeList {
        destination: Register,
        elements: Vec<Register>,
    },
    Index {
        destination: Register,
        object: Register,
        index: Register,
    },
    MakeRecord {
        destination: Register,
        record: RecordId,
        fields: Vec<(String, Register)>,
    },
    MakeVariant {
        destination: Register,
        variant: VariantId,
        payload: Vec<Register>,
    },
    LoadField {
        destination: Register,
        object: Register,
        field: String,
    },
    StoreField {
        object: Register,
        field: String,
        source: Register,
    },
    StoreIndex {
        object: Register,
        index: Register,
        source: Register,
    },
    MakeReference {
        destination: Register,
        object: Register,
        index: Register,
    },
    MoveOut {
        destination: Register,
        source: Register,
    },
    Push {
        destination: Register,
        object: Register,
        value: Register,
    },
    Append {
        destination: Register,
        object: Register,
        value: Register,
    },
    Contains {
        destination: Register,
        value: Register,
        candidates: Vec<Register>,
    },
    Builtin {
        destination: Register,
        builtin: Builtin,
        arguments: Vec<Register>,
    },
    SpawnRemote {
        destination: Register,
        value: Register,
    },
    SpawnRemoteBorrow {
        destination: Register,
        source: Register,
    },
    RemoteCall {
        destination: Register,
        remote: Register,
        function: FunctionId,
        arguments: Vec<(crate::ast::ParameterMode, Register)>,
    },
    Await {
        destination: Register,
        future: Register,
    },
    MatchPattern {
        destination: Register,
        subject: Register,
        pattern: crate::hir::Pattern,
        bindings: Vec<Register>,
    },
    Jump {
        target: usize,
    },
    JumpIfFalse {
        condition: Register,
        target: usize,
    },
    Call {
        destination: Register,
        function: FunctionId,
        arguments: Vec<Register>,
    },
    CallMethod {
        destination: Register,
        receiver: Register,
        function: FunctionId,
        arguments: Vec<Register>,
    },
    MakeClosure {
        destination: Register,
        function: FunctionId,
        captures: Vec<(crate::hir::CaptureMode, Register)>,
    },
    CallValue {
        destination: Register,
        callee: Register,
        arguments: Vec<Register>,
    },
    CallClosure {
        destination: Register,
        function: FunctionId,
        captures: Vec<(crate::hir::CaptureMode, Register)>,
        arguments: Vec<Register>,
    },
    Return {
        source: Register,
    },
}

impl Instruction {
    pub(crate) fn visit_registers(&self, mut visit: impl FnMut(Register)) {
        match self {
            Self::LoadConstant { destination, .. } => visit(*destination),
            Self::Move {
                destination,
                source,
            } => {
                visit(*destination);
                visit(*source);
            }
            Self::Unary {
                destination,
                operand,
                ..
            } => {
                visit(*destination);
                visit(*operand);
            }
            Self::Binary {
                destination,
                left,
                right,
                ..
            } => {
                visit(*destination);
                visit(*left);
                visit(*right);
            }
            Self::MakeList {
                destination,
                elements,
            } => {
                visit(*destination);
                elements.iter().copied().for_each(&mut visit);
            }
            Self::Index {
                destination,
                object,
                index,
            } => {
                visit(*destination);
                visit(*object);
                visit(*index);
            }
            Self::MakeRecord {
                destination,
                fields,
                ..
            } => {
                visit(*destination);
                fields.iter().for_each(|(_, register)| visit(*register));
            }
            Self::MakeVariant {
                destination,
                payload,
                ..
            } => {
                visit(*destination);
                payload.iter().copied().for_each(&mut visit);
            }
            Self::LoadField {
                destination,
                object,
                ..
            } => {
                visit(*destination);
                visit(*object);
            }
            Self::StoreField { object, source, .. } => {
                visit(*object);
                visit(*source);
            }
            Self::StoreIndex {
                object,
                index,
                source,
            } => {
                visit(*object);
                visit(*index);
                visit(*source);
            }
            Self::MakeReference {
                destination,
                object,
                index,
            } => {
                visit(*destination);
                visit(*object);
                visit(*index);
            }
            Self::MoveOut {
                destination,
                source,
            } => {
                visit(*destination);
                visit(*source);
            }
            Self::Push {
                destination,
                object,
                value,
            }
            | Self::Append {
                destination,
                object,
                value,
            } => {
                visit(*destination);
                visit(*object);
                visit(*value);
            }
            Self::Contains {
                destination,
                value,
                candidates,
            } => {
                visit(*destination);
                visit(*value);
                candidates.iter().copied().for_each(&mut visit);
            }
            Self::Builtin {
                destination,
                arguments,
                ..
            } => {
                visit(*destination);
                arguments.iter().copied().for_each(&mut visit);
            }
            Self::SpawnRemote { destination, value } => {
                visit(*destination);
                visit(*value);
            }
            Self::SpawnRemoteBorrow {
                destination,
                source,
            } => {
                visit(*destination);
                visit(*source);
            }
            Self::RemoteCall {
                destination,
                remote,
                arguments,
                ..
            } => {
                visit(*destination);
                visit(*remote);
                arguments.iter().for_each(|(_, register)| visit(*register));
            }
            Self::Await {
                destination,
                future,
            } => {
                visit(*destination);
                visit(*future);
            }
            Self::MatchPattern {
                destination,
                subject,
                bindings,
                ..
            } => {
                visit(*destination);
                visit(*subject);
                bindings.iter().copied().for_each(&mut visit);
            }
            Self::Jump { .. } => {}
            Self::JumpIfFalse { condition, .. } => visit(*condition),
            Self::Call {
                destination,
                arguments,
                ..
            } => {
                visit(*destination);
                arguments.iter().copied().for_each(visit);
            }
            Self::CallMethod {
                destination,
                receiver,
                arguments,
                ..
            } => {
                visit(*destination);
                visit(*receiver);
                arguments.iter().copied().for_each(&mut visit);
            }
            Self::MakeClosure {
                destination,
                captures,
                ..
            } => {
                visit(*destination);
                captures.iter().for_each(|(_, register)| visit(*register));
            }
            Self::CallValue {
                destination,
                callee,
                arguments,
            } => {
                visit(*destination);
                visit(*callee);
                arguments.iter().copied().for_each(&mut visit);
            }
            Self::CallClosure {
                destination,
                captures,
                arguments,
                ..
            } => {
                visit(*destination);
                captures.iter().for_each(|(_, register)| visit(*register));
                arguments.iter().copied().for_each(&mut visit);
            }
            Self::Return { source } => visit(*source),
        }
    }
}

#[derive(Debug, Clone)]
pub struct BytecodeFunction {
    pub name: String,
    pub parameters: u16,
    pub captures: u16,
    pub registers: u16,
    pub instructions: Vec<Instruction>,
    pub instruction_spans: Vec<Range<usize>>,
}

#[derive(Debug, Clone, Default)]
pub struct Program {
    pub constants: Vec<Constant>,
    pub functions: HashMap<FunctionId, BytecodeFunction>,
    pub main: Option<FunctionId>,
    pub records: HashMap<RecordId, String>,
    pub variants: HashMap<VariantId, (String, String)>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProgramMetrics {
    pub functions: usize,
    pub instructions: usize,
    pub registers: usize,
    pub constants: usize,
}

impl Program {
    pub fn metrics(&self) -> ProgramMetrics {
        ProgramMetrics {
            functions: self.functions.len(),
            instructions: self
                .functions
                .values()
                .map(|function| function.instructions.len())
                .sum(),
            registers: self
                .functions
                .values()
                .map(|function| usize::from(function.registers))
                .sum(),
            constants: self.constants.len(),
        }
    }
}
