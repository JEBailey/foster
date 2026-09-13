//! Logical operation vocabulary used by the VM and SSA adapters.
use crate::ast::{BinaryOp, UnaryOp};
use crate::codegen::{
    ir::Value,
    types::{ExecutableType, Specialization},
};
use crate::hir::{FunctionId, RecordId, VariantId};
use crate::intrinsics::Builtin;
use crate::types::DispatchSlot;
#[derive(Debug, Clone)]
pub(crate) enum Instruction {
    /// Releases this frame's ownership of an inline register or promoted slot.
    ///
    /// A promoted register detaches rather than writing through its slot because
    /// reference captures may still own and observe that slot.
    Drop {
        register: Value,
    },
    LoadConstant {
        destination: Value,
        constant: u16,
    },
    Move {
        destination: Value,
        source: Value,
    },
    Unary {
        destination: Value,
        operator: UnaryOp,
        operand: Value,
    },
    Binary {
        destination: Value,
        operator: BinaryOp,
        left: Value,
        right: Value,
    },
    MakeList {
        destination: Value,
        element_type: ExecutableType,
        elements: Vec<Value>,
    },
    Index {
        destination: Value,
        object: Value,
        index: Value,
    },
    MakeRecord {
        destination: Value,
        record: RecordId,
        fields: Vec<(String, Value)>,
    },
    MakeVariant {
        destination: Value,
        variant: VariantId,
        payload: Vec<Value>,
    },
    LoadField {
        destination: Value,
        object: Value,
        by_reference: bool,
    },
    StoreField {
        object: Value,
        source: Value,
    },
    StoreIndex {
        object: Value,
        index: Value,
        source: Value,
    },
    MakeReference {
        destination: Value,
        pointee_type: ExecutableType,
        object: Value,
        index: Value,
    },
    MakeWholeReference {
        destination: Value,
        pointee_type: ExecutableType,
        object: Value,
    },
    MakeFieldReference {
        field: String,
        destination: Value,
        pointee_type: ExecutableType,
        object: Value,
    },
    MoveOut {
        by_reference: bool,
        destination: Value,
        source: Value,
    },
    Push {
        destination: Value,
        object: Value,
        value: Value,
    },
    Append {
        destination: Value,
        object: Value,
        value: Value,
    },
    Contains {
        destination: Value,
        value: Value,
        candidates: Vec<Value>,
    },
    Builtin {
        destination: Value,
        builtin: Builtin,
        arguments: Vec<Value>,
    },
    SpawnRemote {
        destination: Value,
        value: Value,
    },
    SpawnRemoteBorrow {
        destination: Value,
        source: Value,
    },
    RemoteCall {
        destination: Value,
        remote: Value,
        function: FunctionId,
        arguments: Vec<(crate::ast::ParameterMode, Value)>,
    },
    Await {
        destination: Value,
        future: Value,
    },
    MatchPattern {
        destination: Value,
        subject: Value,
        pattern: crate::hir::Pattern,
        bindings: Vec<Value>,
    },
    Jump {
        target: usize,
    },
    JumpIfFalse {
        condition: Value,
        target: usize,
    },
    Assert {
        condition: Value,
        message: Option<Value>,
    },
    Call {
        destination: Value,
        function: FunctionId,
        specialization: Specialization,
        arguments: Vec<Value>,
    },
    CallMethod {
        destination: Value,
        receiver: Value,
        function: FunctionId,
        specialization: Specialization,
        arguments: Vec<Value>,
    },
    CallContractMethod {
        destination: Value,
        receiver: Value,
        slot: DispatchSlot,
        name: String,
        arguments: Vec<Value>,
        result_type: ExecutableType,
    },
    MakeClosure {
        destination: Value,
        function: FunctionId,
        specialization: Specialization,
        captures: Vec<(crate::hir::CaptureMode, Value)>,
    },
    CallValue {
        destination: Value,
        callee: Value,
        arguments: Vec<Value>,
    },
    CallClosure {
        destination: Value,
        function: FunctionId,
        specialization: Specialization,
        captures: Vec<(crate::hir::CaptureMode, Value)>,
        arguments: Vec<Value>,
    },
    Return {
        source: Value,
    },
    CopyOnWrite {
        destination: Value,
        source: Value,
    },
    Edge {
        target: usize,
        arguments: Vec<(Value, Value)>,
    },
}
