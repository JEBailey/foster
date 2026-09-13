use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use crate::ast::{BinaryOp, UnaryOp};
use crate::codegen::types::{ExecutableType, Specialization};
use crate::hir::{FunctionId, RecordId, VariantId, VariantTypeId};
use crate::intrinsics::Builtin;
use crate::types::{DispatchSlot, NominalTypeId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Register(pub u16);

#[derive(Debug, Clone, PartialEq)]
pub enum Constant {
    Unit,
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(String),
    CodePoint(char),
    Symbol(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Instruction {
    /// Releases this frame's ownership of an inline register or promoted slot.
    ///
    /// A promoted register detaches rather than writing through its slot because
    /// reference captures may still own and observe that slot.
    Drop {
        register: Register,
    },
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
        element_type: ExecutableType,
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
        type_arguments: Vec<ExecutableType>,
        fields: Vec<(String, Register)>,
    },
    MakeVariant {
        destination: Register,
        variant: VariantId,
        type_arguments: Vec<ExecutableType>,
        payload: Vec<Register>,
    },
    LoadField {
        destination: Register,
        object: Register,
        field: String,
        by_reference: bool,
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
        pointee_type: ExecutableType,
        object: Register,
        index: Register,
    },
    MakeWholeReference {
        destination: Register,
        pointee_type: ExecutableType,
        object: Register,
    },
    MakeFieldReference {
        destination: Register,
        pointee_type: ExecutableType,
        object: Register,
        field: String,
    },
    MoveOut {
        by_reference: bool,
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
    Assert {
        condition: Register,
        message: Option<Register>,
    },
    Call {
        destination: Register,
        function: FunctionId,
        specialization: Specialization,
        arguments: Vec<Register>,
    },
    CallMethod {
        destination: Register,
        receiver: Register,
        function: FunctionId,
        specialization: Specialization,
        arguments: Vec<Register>,
    },
    CallContractMethod {
        destination: Register,
        receiver: Register,
        slot: DispatchSlot,
        name: String,
        arguments: Vec<Register>,
        result_type: ExecutableType,
    },
    MakeClosure {
        destination: Register,
        function: FunctionId,
        specialization: Specialization,
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
        specialization: Specialization,
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
            Self::Drop { register } => visit(*register),
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
                ..
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
                ..
            } => {
                visit(*destination);
                visit(*object);
                visit(*index);
            }
            Self::MakeWholeReference {
                destination,
                object,
                ..
            } => {
                visit(*destination);
                visit(*object);
            }
            Self::MakeFieldReference {
                destination,
                object,
                ..
            } => {
                visit(*destination);
                visit(*object);
            }
            Self::MoveOut {
                destination,
                source,
                ..
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
            Self::Assert { condition, message } => {
                visit(*condition);
                message.iter().copied().for_each(&mut visit);
            }
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
            }
            | Self::CallContractMethod {
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

#[derive(Debug, Clone, PartialEq)]
pub struct BytecodeFunction {
    pub name: String,
    /// A source intrinsic declaration whose executable call sites lower to `Builtin`.
    pub intrinsic_stub: bool,
    pub parameters: u16,
    pub parameter_types: Vec<ExecutableType>,
    pub parameter_modes: Vec<crate::ast::ParameterMode>,
    pub mutable_parameters: Vec<bool>,
    /// Whether `Return` transfers a live place handle instead of reading its current value.
    pub returns_reference: bool,
    pub captures: u16,
    pub capture_types: Vec<ExecutableType>,
    pub result_type: ExecutableType,
    pub registers: u16,
    pub instructions: Vec<Instruction>,
    pub instruction_spans: Vec<Range<usize>>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Program {
    pub symbols: crate::symbols::Table,
    pub drops_inserted: bool,
    pub constants: Vec<Constant>,
    pub functions: HashMap<FunctionId, BytecodeFunction>,
    pub main: Option<FunctionId>,
    /// Whether `main` receives one `std.process.Arguments` value.
    pub main_arguments: bool,
    pub string_record: Option<RecordId>,
    pub symbol_record: Option<RecordId>,
    pub list_record: Option<RecordId>,
    pub bytes_record: Option<RecordId>,
    pub byte_buffer_record: Option<RecordId>,
    pub remote_result: Option<VariantTypeId>,
    pub remote_error: Option<VariantTypeId>,
    pub records: HashMap<RecordId, RuntimeRecord>,
    pub dispatch: HashMap<(NominalTypeId, DispatchSlot), FunctionId>,
    pub variants: HashMap<VariantId, RuntimeVariant>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeRecord {
    pub name: String,
    /// Generic parameters in declaration order.
    pub parameters: Vec<String>,
    pub layout: Arc<super::value::RecordLayout>,
    /// Declared field types in the same canonical order as `layout`.
    pub field_types: Vec<ExecutableType>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeVariant {
    pub parent: VariantTypeId,
    pub type_name: Arc<str>,
    /// Generic parameters of the parent enum in declaration order.
    pub parameters: Vec<String>,
    pub alternative: Arc<str>,
    /// Enum cases currently have zero or one declared payload value.
    pub payload: Vec<ExecutableType>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProgramMetrics {
    pub functions: usize,
    pub instructions: usize,
    pub registers: usize,
    pub constants: usize,
}

impl Program {
    pub(crate) fn remote_outcome_type(&self, result: ExecutableType) -> ExecutableType {
        ExecutableType::Variant {
            variant: self.remote_result.expect("verified remote Result metadata"),
            arguments: vec![
                result,
                ExecutableType::Variant {
                    variant: self.remote_error.expect("verified RemoteError metadata"),
                    arguments: vec![],
                },
            ],
        }
    }
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
