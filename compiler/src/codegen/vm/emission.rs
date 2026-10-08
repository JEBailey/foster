//! De-SSA register assignment, parallel edge copies, and bytecode emission.
use super::LowerError;
use super::instructions::{lower_instruction, verification_type};
use crate::codegen::ir::{self, Block, Value};
use crate::codegen::metadata::Constant;
use crate::codegen::types::ExecutableType;
use crate::hir::FunctionId;
use crate::vm::{self, Register};
use std::collections::HashMap;
use std::ops::Range;
/// Non-executable metadata which is intentionally not part of SSA.
#[derive(Debug, Clone, Default)]
pub struct FunctionMetadata {
    pub intrinsic_stub: bool,
    pub parameters: Vec<crate::codegen::storage::ExecutableParameter>,
    pub returns_reference: bool,
    pub capture_types: Vec<ExecutableType>,
    pub result_type: Option<ExecutableType>,
}

impl FunctionMetadata {
    pub(crate) fn from_declaration(
        function: &crate::codegen::program::FunctionDeclaration,
    ) -> Self {
        Self {
            intrinsic_stub: function.intrinsic_stub,
            parameters: function.parameters.clone(),
            returns_reference: function.returns_reference,
            capture_types: function.capture_types.clone(),
            result_type: Some(function.result_type.clone()),
        }
    }
}

pub(super) enum Emission {
    Instruction(vm::Instruction, Range<usize>),
    Jump(Block, Range<usize>),
    JumpIfFalse {
        condition: Register,
        target: Block,
        span: Range<usize>,
    },
}

/// Assign every SSA definition a register and materialize block arguments as parallel edge copies.
///
/// Critical conditional edges are split during linearization.  This keeps block parameters out of
/// the bytecode format without making frontends reason about mutable registers.
pub fn lower_function(
    function: &ir::Function,
    signatures: &HashMap<FunctionId, ir::Signature>,
    constants: &mut Vec<Constant>,
    metadata: FunctionMetadata,
) -> Result<vm::BytecodeFunction, LowerError> {
    function
        .verify(signatures)
        .map_err(|error| LowerError(format!("invalid shared IR: {error}")))?;
    lower_verified_function(function, constants, metadata)
}

/// Internal lowering for bodies taken directly from a verified SharedProgram.
/// The public lower_function entry point still validates arbitrary SSA input.
pub(super) fn lower_verified_function(
    function: &ir::Function,
    constants: &mut Vec<Constant>,
    metadata: FunctionMetadata,
) -> Result<vm::BytecodeFunction, LowerError> {
    let mut registers = function
        .values
        .hints()
        .map(|home| home.map(Register))
        .collect::<Vec<_>>();
    let mut next = function
        .values
        .hints()
        .flatten()
        .copied()
        .max()
        .map_or(0, |register| register.saturating_add(1));
    for parameter in function
        .captures
        .iter()
        .map(|capture| &capture.value)
        .chain(&function.parameters)
    {
        assign(&mut registers, *parameter, &mut next)?;
    }
    for index in 0..function.values.len() {
        assign(
            &mut registers,
            function.values.value(index).unwrap(),
            &mut next,
        )?;
    }
    let mut emissions = Vec::new();
    let mut labels = vec![None; function.blocks.len()];

    for seed in &function.entry_seeds {
        if function.values.hint(seed.index()).is_none() {
            lower_instruction(
                &ir::Instruction::Constant {
                    destination: *seed,
                    value: ir::Constant::Unit,
                },
                &registers,
                constants,
                &mut emissions,
                Range::default(),
            )?;
        }
    }

    emit_copies(
        &mut emissions,
        &mut next,
        &registers,
        &function.blocks[function.entry.0 as usize].parameters,
        &function.entry_arguments,
        Range::default(),
    )?;
    if !emissions.is_empty() || function.entry != Block(0) {
        emissions.push(Emission::Jump(function.entry, Range::default()));
    }

    for (block_index, block) in function.blocks.iter().enumerate() {
        labels[block_index] = Some(emissions.len());
        for (instruction, span) in block
            .instructions
            .iter()
            .map(|entry| (&entry.instruction, &entry.span))
        {
            lower_instruction(
                instruction,
                &registers,
                constants,
                &mut emissions,
                span.clone(),
            )?;
        }
        match &block.terminator {
            ir::Terminator::Jump { target, arguments } => {
                emit_copies(
                    &mut emissions,
                    &mut next,
                    &registers,
                    &function.blocks[target.0 as usize].parameters,
                    arguments,
                    block.terminator_span.clone(),
                )?;
                emissions.push(Emission::Jump(*target, block.terminator_span.clone()));
            }
            ir::Terminator::Branch {
                condition,
                then_target,
                then_arguments,
                else_target,
                else_arguments,
            } => {
                if *then_target == Block((block_index + 1) as u32)
                    && !copies_needed(
                        &registers,
                        &function.blocks[then_target.0 as usize].parameters,
                        then_arguments,
                    )
                    && !copies_needed(
                        &registers,
                        &function.blocks[else_target.0 as usize].parameters,
                        else_arguments,
                    )
                {
                    emissions.push(Emission::JumpIfFalse {
                        condition: reg(&registers, *condition),
                        target: *else_target,
                        span: block.terminator_span.clone(),
                    });
                    continue;
                }
                // The false edge gets a private label after the true-edge copies.
                let false_label = Block(labels.len() as u32);
                labels.push(None);
                emissions.push(Emission::JumpIfFalse {
                    condition: reg(&registers, *condition),
                    target: false_label,
                    span: block.terminator_span.clone(),
                });
                emit_copies(
                    &mut emissions,
                    &mut next,
                    &registers,
                    &function.blocks[then_target.0 as usize].parameters,
                    then_arguments,
                    block.terminator_span.clone(),
                )?;
                emissions.push(Emission::Jump(*then_target, block.terminator_span.clone()));
                labels[false_label.0 as usize] = Some(emissions.len());
                emit_copies(
                    &mut emissions,
                    &mut next,
                    &registers,
                    &function.blocks[else_target.0 as usize].parameters,
                    else_arguments,
                    block.terminator_span.clone(),
                )?;
                emissions.push(Emission::Jump(*else_target, block.terminator_span.clone()));
            }
            ir::Terminator::Unreachable => {
                return Err(LowerError(
                    "native-only unreachable terminator in VM lowering".into(),
                ));
            }
            ir::Terminator::Return(value) => {
                emissions.push(Emission::Instruction(
                    vm::Instruction::Return {
                        source: reg(&registers, *value),
                    },
                    block.terminator_span.clone(),
                ));
            }
        }
    }

    let lowered = emissions
        .into_iter()
        .map(|emission| match emission {
            Emission::Instruction(instruction, span) => Ok((instruction, span)),
            Emission::Jump(target, span) => Ok((
                vm::Instruction::Jump {
                    target: label(&labels, target)?,
                },
                span,
            )),
            Emission::JumpIfFalse {
                condition,
                target,
                span,
            } => Ok((
                vm::Instruction::JumpIfFalse {
                    condition,
                    target: label(&labels, target)?,
                },
                span,
            )),
        })
        .collect::<Result<Vec<_>, LowerError>>()?;
    let (instructions, spans) = lowered.into_iter().unzip();
    let parameters = if metadata.parameters.is_empty() {
        function
            .signature
            .parameters
            .iter()
            .copied()
            .map(|ty| crate::codegen::storage::ExecutableParameter {
                ty: verification_type(ty),
                mode: crate::ast::ParameterMode::Borrow,
                mutable: false,
            })
            .collect::<Vec<_>>()
    } else {
        metadata.parameters
    };
    u16::try_from(parameters.len())
        .map_err(|_| LowerError("too many function parameters".into()))?;
    Ok(vm::BytecodeFunction {
        name: function.name.clone(),
        intrinsic_stub: metadata.intrinsic_stub,
        parameters,
        returns_reference: metadata.returns_reference,
        captures: u16::try_from(function.captures.len())
            .map_err(|_| LowerError("too many function captures".into()))?,
        capture_types: if metadata.capture_types.is_empty() {
            function
                .captures
                .iter()
                .map(|capture| capture.ty)
                .map(verification_type)
                .collect()
        } else {
            metadata.capture_types
        },
        result_type: metadata
            .result_type
            .unwrap_or_else(|| verification_type(function.signature.result)),
        registers: next,
        body: crate::codegen::storage::InstructionBody::try_from_parts(instructions, spans)
            .map_err(|e| LowerError(e.to_string()))?,
    })
}

fn copies_needed(
    registers: &[Option<Register>],
    destinations: &[Value],
    sources: &[Value],
) -> bool {
    destinations
        .iter()
        .zip(sources)
        .any(|(destination, source)| reg(registers, *destination) != reg(registers, *source))
}

fn assign(
    registers: &mut [Option<Register>],
    value: Value,
    next: &mut u16,
) -> Result<(), LowerError> {
    if registers[value.index()].is_none() {
        let register = Register(*next);
        *next = next
            .checked_add(1)
            .ok_or_else(|| LowerError("shared IR needs more than 65535 VM registers".into()))?;
        registers[value.index()] = Some(register);
    }
    Ok(())
}

pub(super) fn reg(registers: &[Option<Register>], value: Value) -> Register {
    registers[value.index()].expect("all values assigned above")
}

fn label(labels: &[Option<usize>], block: Block) -> Result<usize, LowerError> {
    labels
        .get(block.0 as usize)
        .and_then(|label| *label)
        .ok_or_else(|| LowerError(format!("unresolved VM edge label b{}", block.0)))
}

pub(super) fn emit_copies(
    emissions: &mut Vec<Emission>,
    next: &mut u16,
    registers: &[Option<Register>],
    destinations: &[Value],
    sources: &[Value],
    span: Range<usize>,
) -> Result<(), LowerError> {
    let mut copies = destinations
        .iter()
        .zip(sources)
        .map(|(destination, source)| (reg(registers, *destination), reg(registers, *source)))
        .filter(|(destination, source)| destination != source)
        .collect::<Vec<_>>();
    while !copies.is_empty() {
        if let Some(index) = copies
            .iter()
            .position(|(destination, _)| !copies.iter().any(|(_, source)| source == destination))
        {
            let (destination, source) = copies.remove(index);
            emissions.push(Emission::Instruction(
                vm::Instruction::Move {
                    destination,
                    source,
                },
                span.clone(),
            ));
            continue;
        }
        let preserved = copies[0].0;
        let temporary = Register(*next);
        *next = next
            .checked_add(1)
            .ok_or_else(|| LowerError("parallel copy needs a register past r65535".into()))?;
        emissions.push(Emission::Instruction(
            vm::Instruction::Move {
                destination: temporary,
                source: preserved,
            },
            span.clone(),
        ));
        for (_, source) in &mut copies {
            if *source == preserved {
                *source = temporary;
            }
        }
    }
    Ok(())
}
