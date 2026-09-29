//! Shared slot IR used during construction and after VM SSA destruction.
//! Slots express observable storage identity; they are not machine registers.

pub mod analysis;
pub mod lifetimes;
pub mod schema;
pub mod verification;
use std::collections::HashMap;
use std::ops::Range;

use crate::ast::{BinaryOp, UnaryOp};
use crate::codegen::types::{ExecutableType, Specialization};
use crate::hir::{FunctionId, RecordId, VariantId};
use crate::intrinsics::Builtin;
use crate::types::DispatchSlot;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Slot(pub u16);

#[derive(Debug, Clone, PartialEq)]
pub enum Instruction {
    /// Releases this frame's ownership of an inline register or promoted slot.
    ///
    /// A promoted register detaches rather than writing through its slot because
    /// reference captures may still own and observe that slot.
    Drop {
        register: Slot,
    },
    LoadConstant {
        destination: Slot,
        constant: u16,
    },
    Move {
        destination: Slot,
        source: Slot,
    },
    Unary {
        destination: Slot,
        operator: UnaryOp,
        operand: Slot,
    },
    Binary {
        destination: Slot,
        operator: BinaryOp,
        left: Slot,
        right: Slot,
    },
    MakeList {
        destination: Slot,
        element_type: ExecutableType,
        elements: Vec<Slot>,
    },
    Index {
        destination: Slot,
        object: Slot,
        index: Slot,
    },
    MakeRecord {
        destination: Slot,
        record: RecordId,
        type_arguments: Vec<ExecutableType>,
        fields: Vec<(String, Slot)>,
    },
    MakeVariant {
        destination: Slot,
        variant: VariantId,
        type_arguments: Vec<ExecutableType>,
        payload: Vec<Slot>,
    },
    LoadField {
        destination: Slot,
        object: Slot,
        field: String,
        by_reference: bool,
    },
    StoreField {
        object: Slot,
        field: String,
        source: Slot,
    },
    StoreIndex {
        object: Slot,
        index: Slot,
        source: Slot,
    },
    MakeReference {
        destination: Slot,
        pointee_type: ExecutableType,
        object: Slot,
        index: Slot,
    },
    MakeWholeReference {
        destination: Slot,
        pointee_type: ExecutableType,
        object: Slot,
    },
    MakeFieldReference {
        destination: Slot,
        pointee_type: ExecutableType,
        object: Slot,
        field: String,
    },
    MoveOut {
        by_reference: bool,
        destination: Slot,
        source: Slot,
    },
    Push {
        destination: Slot,
        object: Slot,
        value: Slot,
    },
    Append {
        destination: Slot,
        object: Slot,
        value: Slot,
    },
    Contains {
        destination: Slot,
        value: Slot,
        candidates: Vec<Slot>,
    },
    Builtin {
        destination: Slot,
        builtin: Builtin,
        arguments: Vec<Slot>,
    },
    SpawnRemote {
        destination: Slot,
        value: Slot,
    },
    SpawnRemoteBorrow {
        destination: Slot,
        source: Slot,
    },
    RemoteCall {
        destination: Slot,
        remote: Slot,
        function: FunctionId,
        arguments: Vec<(crate::ast::ParameterMode, Slot)>,
    },
    Await {
        destination: Slot,
        future: Slot,
    },
    MatchPattern {
        destination: Slot,
        subject: Slot,
        pattern: crate::hir::Pattern,
        bindings: Vec<Slot>,
    },
    Jump {
        target: usize,
    },
    JumpIfFalse {
        condition: Slot,
        target: usize,
    },
    Assert {
        condition: Slot,
        message: Option<Slot>,
    },
    Call {
        destination: Slot,
        function: FunctionId,
        specialization: Specialization,
        arguments: Vec<Slot>,
    },
    CallMethod {
        destination: Slot,
        receiver: Slot,
        function: FunctionId,
        specialization: Specialization,
        arguments: Vec<Slot>,
    },
    CallContractMethod {
        destination: Slot,
        receiver: Slot,
        slot: DispatchSlot,
        name: String,
        arguments: Vec<Slot>,
        result_type: ExecutableType,
    },
    MakeClosure {
        destination: Slot,
        function: FunctionId,
        specialization: Specialization,
        captures: Vec<(crate::hir::CaptureMode, Slot)>,
    },
    CallValue {
        destination: Slot,
        callee: Slot,
        arguments: Vec<Slot>,
    },
    CallClosure {
        destination: Slot,
        function: FunctionId,
        specialization: Specialization,
        captures: Vec<(crate::hir::CaptureMode, Slot)>,
        arguments: Vec<Slot>,
    },
    Return {
        source: Slot,
    },
}

impl Instruction {
    pub fn visit_registers(&self, mut visit: impl FnMut(Slot)) {
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

impl Instruction {
    pub fn visit_registers_mut(&mut self, mut visit: impl FnMut(&mut Slot)) {
        match self {
            Self::Drop { register } => visit(register),
            Self::LoadConstant { destination, .. } => visit(destination),
            Self::Move {
                destination,
                source,
            } => {
                visit(destination);
                visit(source);
            }
            Self::Unary {
                destination,
                operand,
                ..
            } => {
                visit(destination);
                visit(operand);
            }
            Self::Binary {
                destination,
                left,
                right,
                ..
            } => {
                visit(destination);
                visit(left);
                visit(right);
            }
            Self::MakeList {
                destination,
                elements,
                ..
            } => {
                visit(destination);
                elements.iter_mut().for_each(&mut visit);
            }
            Self::Index {
                destination,
                object,
                index,
            } => {
                visit(destination);
                visit(object);
                visit(index);
            }
            Self::MakeRecord {
                destination,
                fields,
                ..
            } => {
                visit(destination);
                fields.iter_mut().for_each(|(_, register)| visit(register));
            }
            Self::MakeVariant {
                destination,
                payload,
                ..
            } => {
                visit(destination);
                payload.iter_mut().for_each(&mut visit);
            }
            Self::LoadField {
                destination,
                object,
                ..
            } => {
                visit(destination);
                visit(object);
            }
            Self::StoreField { object, source, .. } => {
                visit(object);
                visit(source);
            }
            Self::StoreIndex {
                object,
                index,
                source,
            } => {
                visit(object);
                visit(index);
                visit(source);
            }
            Self::MakeReference {
                destination,
                object,
                index,
                ..
            } => {
                visit(destination);
                visit(object);
                visit(index);
            }
            Self::MakeWholeReference {
                destination,
                object,
                ..
            } => {
                visit(destination);
                visit(object);
            }
            Self::MakeFieldReference {
                destination,
                object,
                ..
            } => {
                visit(destination);
                visit(object);
            }
            Self::MoveOut {
                destination,
                source,
                ..
            } => {
                visit(destination);
                visit(source);
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
                visit(destination);
                visit(object);
                visit(value);
            }
            Self::Contains {
                destination,
                value,
                candidates,
            } => {
                visit(destination);
                visit(value);
                candidates.iter_mut().for_each(&mut visit);
            }
            Self::Builtin {
                destination,
                arguments,
                ..
            } => {
                visit(destination);
                arguments.iter_mut().for_each(&mut visit);
            }
            Self::SpawnRemote { destination, value } => {
                visit(destination);
                visit(value);
            }
            Self::SpawnRemoteBorrow {
                destination,
                source,
            } => {
                visit(destination);
                visit(source);
            }
            Self::RemoteCall {
                destination,
                remote,
                arguments,
                ..
            } => {
                visit(destination);
                visit(remote);
                arguments
                    .iter_mut()
                    .for_each(|(_, register)| visit(register));
            }
            Self::Await {
                destination,
                future,
            } => {
                visit(destination);
                visit(future);
            }
            Self::MatchPattern {
                destination,
                subject,
                bindings,
                ..
            } => {
                visit(destination);
                visit(subject);
                bindings.iter_mut().for_each(&mut visit);
            }
            Self::Jump { .. } => {}
            Self::JumpIfFalse { condition, .. } => visit(condition),
            Self::Assert { condition, message } => {
                visit(condition);
                message.iter_mut().for_each(&mut visit);
            }
            Self::Call {
                destination,
                arguments,
                ..
            } => {
                visit(destination);
                arguments.iter_mut().for_each(visit);
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
                visit(destination);
                visit(receiver);
                arguments.iter_mut().for_each(&mut visit);
            }
            Self::MakeClosure {
                destination,
                captures,
                ..
            } => {
                visit(destination);
                captures
                    .iter_mut()
                    .for_each(|(_, register)| visit(register));
            }
            Self::CallValue {
                destination,
                callee,
                arguments,
            } => {
                visit(destination);
                visit(callee);
                arguments.iter_mut().for_each(&mut visit);
            }
            Self::CallClosure {
                destination,
                captures,
                arguments,
                ..
            } => {
                visit(destination);
                captures
                    .iter_mut()
                    .for_each(|(_, register)| visit(register));
                arguments.iter_mut().for_each(&mut visit);
            }
            Self::Return { source } => visit(source),
        }
    }
}

/// One executable parameter, including its ownership and mutation contract.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutableParameter {
    pub ty: ExecutableType,
    pub mode: crate::ast::ParameterMode,
    pub mutable: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    pub name: String,
    /// A source intrinsic declaration whose executable call sites lower to `Builtin`.
    pub intrinsic_stub: bool,
    pub parameters: Vec<ExecutableParameter>,
    /// Whether `Return` transfers a live place handle instead of reading its current value.
    pub returns_reference: bool,
    pub captures: u16,
    pub capture_types: Vec<ExecutableType>,
    pub result_type: ExecutableType,
    pub registers: u16,
    pub body: InstructionBody,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Program {
    pub metadata: crate::codegen::metadata::ProgramMetadata,
    pub drops_inserted: bool,
    pub functions: HashMap<FunctionId, Function>,
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
            constants: self.metadata.constants.len(),
        }
    }
}

impl Function {
    pub fn parameter_count(&self) -> u16 {
        u16::try_from(self.parameters.len()).expect("executable parameter count exceeds u16")
    }
}

/// Instruction locations are built and resized together.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct InstructionBody {
    tables: InstructionTables,
}
#[derive(Debug, Clone, PartialEq, Default)]
pub struct InstructionTables {
    pub instructions: Vec<Instruction>,
    pub instruction_spans: Vec<Range<usize>>,
}
impl std::ops::Deref for InstructionBody {
    type Target = InstructionTables;
    fn deref(&self) -> &Self::Target {
        &self.tables
    }
}
impl std::ops::Deref for Function {
    type Target = InstructionBody;
    fn deref(&self) -> &Self::Target {
        &self.body
    }
}
impl InstructionBody {
    pub fn try_from_parts(
        instructions: Vec<Instruction>,
        instruction_spans: Vec<Range<usize>>,
    ) -> Result<Self, crate::error::FosterError> {
        if instructions.len() != instruction_spans.len() {
            return Err(crate::error::FosterError::runtime(
                "mismatched instruction and span tables",
            ));
        }
        Ok(Self {
            tables: InstructionTables {
                instructions,
                instruction_spans,
            },
        })
    }
    pub fn new(entries: impl IntoIterator<Item = (Instruction, Range<usize>)>) -> Self {
        let (instructions, instruction_spans) = entries.into_iter().unzip();
        Self {
            tables: InstructionTables {
                instructions,
                instruction_spans,
            },
        }
    }
    pub fn into_parts(self) -> (Vec<Instruction>, Vec<Range<usize>>) {
        (self.tables.instructions, self.tables.instruction_spans)
    }
    pub fn instructions_mut(&mut self) -> &mut [Instruction] {
        &mut self.tables.instructions
    }
    pub fn push(&mut self, instruction: Instruction, span: Range<usize>) {
        self.tables.instructions.push(instruction);
        self.tables.instruction_spans.push(span);
    }
    pub fn insert(&mut self, index: usize, instruction: Instruction, span: Range<usize>) {
        self.tables.instructions.insert(index, instruction);
        self.tables.instruction_spans.insert(index, span);
    }
    pub fn retain(&mut self, mut keep: impl FnMut(&Instruction) -> bool) {
        let entries = std::mem::take(self).into_parts();
        *self = Self::new(
            entries
                .0
                .into_iter()
                .zip(entries.1)
                .filter(|(instruction, _)| keep(instruction)),
        );
    }
}

#[cfg(test)]
mod construction_tests {
    use super::*;
    #[test]
    fn bodies_reject_misaligned_locations_and_keep_edits_paired() {
        let instruction = Instruction::Return { source: Slot(0) };
        assert!(InstructionBody::try_from_parts(vec![instruction.clone()], vec![]).is_err());
        let mut body = InstructionBody::new([(instruction.clone(), 1..2)]);
        body.insert(0, Instruction::Drop { register: Slot(0) }, 3..4);
        body.push(instruction, 5..6);
        body.retain(|i| !matches!(i, Instruction::Drop { .. }));
        assert_eq!(body.instruction_spans, vec![1..2, 5..6]);
        assert_eq!(body.instructions.len(), 2);
    }
}
