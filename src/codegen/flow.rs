//! Logical flow evidence for an immutable shared SSA function.
//!
//! Reachability, definition availability, and an explicitly unknown logical type are distinct.
//! These facts describe types at uses, not physical representation or ownership permission.

use super::{
    ir::{Block, Value},
    types::ExecutableType,
};
use std::collections::HashMap;

/// Logical callable evidence retained before representation erasure.
#[derive(Debug, Clone)]
pub struct FunctionSchema {
    pub name: String,
    pub parameters: Vec<crate::types::Parameter<ExecutableType>>,
    pub captures: Vec<ExecutableType>,
    pub result_type: ExecutableType,
    pub returns_reference: bool,
    pub intrinsic_stub: bool,
}

/// An instruction position; the position after a block's instructions denotes its terminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Site {
    pub block: Block,
    pub instruction: usize,
}

#[derive(Debug)]
pub enum ValueFact<'a> {
    Unreachable,
    Unavailable,
    /// The value is not an operand at this site; no use-site assertion is made.
    NotAnOperand,
    Known(&'a ExecutableType),
}

#[derive(Debug, Clone)]
pub(crate) enum PointFacts {
    Unreachable,
    Reachable(HashMap<Value, Option<ExecutableType>>),
}

/// Created by analysis and published with the exact sealed graph it describes.
#[derive(Debug, Clone)]
pub struct FunctionFacts {
    pub(crate) points: Vec<Vec<PointFacts>>,
    pub(crate) values: Vec<Vec<ExecutableType>>,
}

impl FunctionFacts {
    pub fn reachable(&self, site: Site) -> bool {
        matches!(
            self.points
                .get(site.block.0 as usize)
                .and_then(|block| block.get(site.instruction)),
            Some(PointFacts::Reachable(_))
        )
    }
    pub fn at(&self, site: Site, value: Value) -> ValueFact<'_> {
        match &self.points[site.block.0 as usize][site.instruction] {
            PointFacts::Unreachable => ValueFact::Unreachable,
            PointFacts::Reachable(values) => match values.get(&value) {
                Some(Some(ty)) => ValueFact::Known(ty),
                Some(None) => ValueFact::Unavailable,
                None => ValueFact::NotAnOperand,
            },
        }
    }
    pub fn value_types(&self, value: Value) -> &[ExecutableType] {
        &self.values[value.index()]
    }
    pub fn types(&self) -> impl Iterator<Item = &ExecutableType> {
        self.values.iter().flatten().chain(
            self.points
                .iter()
                .flatten()
                .flat_map(|point| match point {
                    PointFacts::Reachable(values) => Some(values.values()),
                    PointFacts::Unreachable => None,
                })
                .flatten()
                .flatten(),
        )
    }
}

pub(crate) mod engine;
pub(crate) fn ssa_instruction(
    instruction: &crate::codegen::ir::PortableInstruction,
) -> engine::Instruction {
    match instruction {
        crate::codegen::ir::PortableInstruction::Drop { value: register } => {
            engine::Instruction::Drop {
                register: *register,
            }
        }
        crate::codegen::ir::PortableInstruction::LoadConstant {
            destination,
            constant,
        } => engine::Instruction::LoadConstant {
            destination: *destination,
            constant: *constant,
        },
        crate::codegen::ir::PortableInstruction::Move {
            destination,
            source,
        } => engine::Instruction::Move {
            destination: *destination,
            source: *source,
        },
        crate::codegen::ir::PortableInstruction::Unary {
            destination,
            operator,
            operand,
        } => engine::Instruction::Unary {
            destination: *destination,
            operator: *operator,
            operand: *operand,
        },
        crate::codegen::ir::PortableInstruction::Binary {
            destination,
            operator,
            left,
            right,
        } => engine::Instruction::Binary {
            destination: *destination,
            operator: *operator,
            left: *left,
            right: *right,
        },
        crate::codegen::ir::PortableInstruction::MakeList {
            destination,
            element_type,
            elements,
        } => engine::Instruction::MakeList {
            destination: *destination,
            element_type: element_type.clone(),
            elements: elements.clone(),
        },
        crate::codegen::ir::PortableInstruction::Index {
            destination,
            object,
            index,
        } => engine::Instruction::Index {
            destination: *destination,
            object: *object,
            index: *index,
        },
        crate::codegen::ir::PortableInstruction::MakeRecord {
            destination,
            record,
            type_arguments: _,
            fields,
        } => engine::Instruction::MakeRecord {
            destination: *destination,
            record: *record,
            fields: fields.clone(),
        },
        crate::codegen::ir::PortableInstruction::MakeVariant {
            destination,
            variant,
            type_arguments,
            payload,
        } => engine::Instruction::MakeVariant {
            destination: *destination,
            variant: *variant,
            type_arguments: type_arguments.clone(),
            payload: payload.clone(),
        },
        crate::codegen::ir::PortableInstruction::LoadField {
            destination,
            object,
            field: _,
            by_reference,
        } => engine::Instruction::LoadField {
            destination: *destination,
            object: *object,
            by_reference: *by_reference,
        },
        crate::codegen::ir::PortableInstruction::StoreField {
            object,
            field: _,
            source,
        } => engine::Instruction::StoreField {
            object: *object,
            source: *source,
        },
        crate::codegen::ir::PortableInstruction::StoreIndex {
            object,
            index,
            source,
        } => engine::Instruction::StoreIndex {
            object: *object,
            index: *index,
            source: *source,
        },
        crate::codegen::ir::PortableInstruction::MakeReference {
            destination,
            pointee_type,
            object,
            index,
        } => engine::Instruction::MakeReference {
            destination: *destination,
            pointee_type: pointee_type.clone(),
            object: *object,
            index: *index,
        },
        crate::codegen::ir::PortableInstruction::MakeWholeReference {
            destination,
            pointee_type,
            object,
        } => engine::Instruction::MakeWholeReference {
            destination: *destination,
            pointee_type: pointee_type.clone(),
            object: *object,
        },
        crate::codegen::ir::PortableInstruction::MakeFieldReference {
            destination,
            pointee_type,
            object,
            field,
        } => engine::Instruction::MakeFieldReference {
            destination: *destination,
            pointee_type: pointee_type.clone(),
            object: *object,
            field: field.clone(),
        },
        crate::codegen::ir::PortableInstruction::MoveOut {
            by_reference,
            destination,
            source,
        } => engine::Instruction::MoveOut {
            by_reference: *by_reference,
            destination: *destination,
            source: *source,
        },
        crate::codegen::ir::PortableInstruction::Push {
            destination,
            object,
            value,
        } => engine::Instruction::Push {
            destination: *destination,
            object: *object,
            value: *value,
        },
        crate::codegen::ir::PortableInstruction::Append {
            destination,
            object,
            value,
        } => engine::Instruction::Append {
            destination: *destination,
            object: *object,
            value: *value,
        },
        crate::codegen::ir::PortableInstruction::Contains {
            destination,
            value,
            candidates,
        } => engine::Instruction::Contains {
            destination: *destination,
            value: *value,
            candidates: candidates.clone(),
        },
        crate::codegen::ir::PortableInstruction::Builtin {
            destination,
            builtin,
            arguments,
        } => engine::Instruction::Builtin {
            destination: *destination,
            builtin: *builtin,
            arguments: arguments.clone(),
        },
        crate::codegen::ir::PortableInstruction::SpawnRemote { destination, value } => {
            engine::Instruction::SpawnRemote {
                destination: *destination,
                value: *value,
            }
        }
        crate::codegen::ir::PortableInstruction::SpawnRemoteBorrow {
            destination,
            source,
        } => engine::Instruction::SpawnRemoteBorrow {
            destination: *destination,
            source: *source,
        },
        crate::codegen::ir::PortableInstruction::RemoteCall {
            destination,
            remote,
            function,
            arguments,
        } => engine::Instruction::RemoteCall {
            destination: *destination,
            remote: *remote,
            function: *function,
            arguments: arguments.clone(),
        },
        crate::codegen::ir::PortableInstruction::Await {
            destination,
            future,
        } => engine::Instruction::Await {
            destination: *destination,
            future: *future,
        },
        crate::codegen::ir::PortableInstruction::MatchPattern {
            destination,
            subject,
            pattern,
            bindings,
        } => engine::Instruction::MatchPattern {
            destination: *destination,
            subject: *subject,
            pattern: pattern.clone(),
            bindings: bindings.clone(),
        },
        crate::codegen::ir::PortableInstruction::Assert { condition, message } => {
            engine::Instruction::Assert {
                condition: *condition,
                message: *message,
            }
        }
        crate::codegen::ir::PortableInstruction::Call {
            destination,
            function,
            specialization,
            arguments,
        } => engine::Instruction::Call {
            destination: *destination,
            function: *function,
            specialization: specialization.clone(),
            arguments: arguments.clone(),
        },
        crate::codegen::ir::PortableInstruction::CallMethod {
            destination,
            receiver,
            function,
            specialization,
            arguments,
        } => engine::Instruction::CallMethod {
            destination: *destination,
            receiver: *receiver,
            function: *function,
            specialization: specialization.clone(),
            arguments: arguments.clone(),
        },
        crate::codegen::ir::PortableInstruction::CallContractMethod {
            destination,
            receiver,
            slot,
            name,
            arguments,
            result_type,
        } => engine::Instruction::CallContractMethod {
            destination: *destination,
            receiver: *receiver,
            slot: *slot,
            name: name.clone(),
            arguments: arguments.clone(),
            result_type: result_type.clone(),
        },
        crate::codegen::ir::PortableInstruction::MakeClosure {
            destination,
            function,
            specialization,
            captures,
        } => engine::Instruction::MakeClosure {
            destination: *destination,
            function: *function,
            specialization: specialization.clone(),
            captures: captures.clone(),
        },
        crate::codegen::ir::PortableInstruction::CallValue {
            destination,
            callee,
            arguments,
        } => engine::Instruction::CallValue {
            destination: *destination,
            callee: *callee,
            arguments: arguments.clone(),
        },
        crate::codegen::ir::PortableInstruction::CallClosure {
            destination,
            function,
            specialization,
            captures,
            arguments,
        } => engine::Instruction::CallClosure {
            destination: *destination,
            function: *function,
            specialization: specialization.clone(),
            captures: captures.clone(),
            arguments: arguments.clone(),
        },
        crate::codegen::ir::PortableInstruction::CopyOnWrite {
            destination,
            source,
        } => engine::Instruction::CopyOnWrite {
            destination: *destination,
            source: *source,
        },
    }
}

/// Analyze the sealed SSA graph using logical operands and parallel block arguments.
/// Representation types are deliberately never used to reconstruct missing logical evidence.
pub(crate) fn analyze(
    metadata: &crate::codegen::metadata::ProgramMetadata,
    schemas: &HashMap<crate::hir::FunctionId, FunctionSchema>,
    schema: &FunctionSchema,
    function: &super::ir::Function,
    write_bindings: &HashMap<Value, Value>,
) -> Result<FunctionFacts, crate::error::FosterError> {
    crate::compiler::profile::count("shared.flow_analysis");
    use super::ir::{Instruction, Terminator};
    use engine::Instruction as Op;
    let mut offsets = Vec::new();
    let mut count = 1; // implicit entry edge
    for block in &function.blocks {
        offsets.push(count);
        count += block.instructions.len()
            + match block.terminator {
                Terminator::Branch { .. } => 3,
                _ => 1,
            };
    }
    let edge = |target: Block, arguments: &[Value]| Op::Edge {
        target: offsets[target.0 as usize],
        arguments: arguments
            .iter()
            .copied()
            .zip(
                function.blocks[target.0 as usize]
                    .parameters
                    .iter()
                    .copied(),
            )
            .collect(),
    };
    let mut instructions = vec![edge(function.entry, &function.entry_arguments)];
    let mut sites = Vec::new();
    for block in &function.blocks {
        let mut positions = Vec::new();
        for instruction in &block.instructions {
            positions.push(instructions.len());
            let Instruction::Portable(portable) = &instruction.instruction else {
                return Err(crate::error::FosterError::runtime(
                    "logical flow requires pre-legalization portable SSA",
                ));
            };
            instructions.push(ssa_instruction(portable));
        }
        positions.push(instructions.len());
        match &block.terminator {
            Terminator::Return(source) => instructions.push(Op::Return { source: *source }),
            Terminator::Jump { target, arguments } => instructions.push(edge(*target, arguments)),
            Terminator::Branch {
                condition,
                then_target,
                then_arguments,
                else_target,
                else_arguments,
            } => {
                instructions.push(Op::JumpIfFalse {
                    condition: *condition,
                    target: instructions.len() + 2,
                });
                instructions.push(edge(*then_target, then_arguments));
                instructions.push(edge(*else_target, else_arguments));
            }
        }
        sites.push(positions);
    }
    let body = engine::Body {
        schema,
        write_bindings,
        instructions,
        value_count: function.values.len(),
        entry_values: function
            .captures
            .iter()
            .map(|c| c.value)
            .chain(function.parameters.iter().copied())
            .collect(),
        storage_policy: engine::StoragePolicy::ImmutableValues,
    };
    let states = engine::analyze_function_flow(
        &engine::Program {
            metadata,
            functions: schemas,
        },
        &body,
    )?;
    let mut values = vec![std::collections::BTreeSet::new(); function.values.len()];
    let points = sites
        .iter()
        .enumerate()
        .map(|(b, positions)| {
            positions
                .iter()
                .enumerate()
                .map(|(i, position)| {
                    let Some(state) = &states[*position] else {
                        return PointFacts::Unreachable;
                    };
                    // Retain definition evidence as well as use-site evidence, including unused parameters.
                    for (types, ty) in values.iter_mut().zip(&state.bindings) {
                        if let Some(ty) = ty {
                            types.insert(ty.clone());
                        }
                    }
                    let block = &function.blocks[b];
                    let operands = if let Some(instruction) = block.instructions.get(i) {
                        instruction.operands()
                    } else {
                        match &block.terminator {
                            Terminator::Return(value) => vec![*value],
                            Terminator::Jump { arguments, .. } => arguments.clone(),
                            Terminator::Branch {
                                condition,
                                then_arguments,
                                else_arguments,
                                ..
                            } => std::iter::once(*condition)
                                .chain(then_arguments.iter().copied())
                                .chain(else_arguments.iter().copied())
                                .collect(),
                        }
                    };
                    PointFacts::Reachable(
                        operands
                            .into_iter()
                            .map(|v| (v, state.bindings[v.index()].clone()))
                            .collect(),
                    )
                })
                .collect()
        })
        .collect();
    Ok(FunctionFacts {
        points,
        values: values
            .into_iter()
            .map(|s| s.into_iter().collect())
            .collect(),
    })
}

#[cfg(test)]
mod tests;
