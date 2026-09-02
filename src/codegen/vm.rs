//! De-SSA lowering from shared executable IR to portable VM bytecode.

use std::collections::HashMap;
use std::fmt;
use std::ops::Range;

use crate::codegen::ir::{self, Block, Type, Value};
use crate::hir::FunctionId;
use crate::vm::{self, Register, VerificationType};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LowerError(String);

impl fmt::Display for LowerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for LowerError {}

/// Non-executable metadata which is intentionally not part of SSA.
#[derive(Debug, Clone, Default)]
pub struct FunctionMetadata {
    pub intrinsic_stub: bool,
    pub parameter_modes: Vec<crate::ast::ParameterMode>,
    pub mutable_parameters: Vec<bool>,
    pub returns_reference: bool,
}

enum Emission {
    Instruction(vm::Instruction),
    Jump(Block),
    JumpIfFalse { condition: Register, target: Block },
}

/// Assign every SSA definition a register and materialize block arguments as parallel edge copies.
///
/// Critical conditional edges are split during linearization.  This keeps block parameters out of
/// the bytecode format without making frontends reason about mutable registers.
pub fn lower_function(
    function: &ir::Function,
    signatures: &HashMap<FunctionId, ir::Signature>,
    constants: &mut Vec<vm::Constant>,
    metadata: FunctionMetadata,
) -> Result<vm::BytecodeFunction, LowerError> {
    function
        .verify(signatures)
        .map_err(|error| LowerError(format!("invalid shared IR: {error}")))?;

    let mut registers = vec![None; function.value_types.len()];
    let mut next = 0_u16;
    for parameter in &function.parameters {
        assign(&mut registers, *parameter, &mut next)?;
    }
    for index in 0..function.value_types.len() {
        assign(&mut registers, Value(index as u32), &mut next)?;
    }
    let mut emissions = Vec::new();
    let mut labels = vec![None; function.blocks.len()];

    emit_copies(
        &mut emissions,
        &mut next,
        &registers,
        &function.blocks[function.entry.0 as usize].parameters,
        &function.entry_arguments,
    )?;
    emissions.push(Emission::Jump(function.entry));

    for (block_index, block) in function.blocks.iter().enumerate() {
        labels[block_index] = Some(emissions.len());
        for instruction in &block.instructions {
            lower_instruction(instruction, &registers, constants, &mut emissions)?;
        }
        match &block.terminator {
            ir::Terminator::Jump { target, arguments } => {
                emit_copies(
                    &mut emissions,
                    &mut next,
                    &registers,
                    &function.blocks[target.0 as usize].parameters,
                    arguments,
                )?;
                emissions.push(Emission::Jump(*target));
            }
            ir::Terminator::Branch {
                condition,
                then_target,
                then_arguments,
                else_target,
                else_arguments,
            } => {
                // The false edge gets a private label after the true-edge copies.
                let false_label = Block(labels.len() as u32);
                labels.push(None);
                emissions.push(Emission::JumpIfFalse {
                    condition: reg(&registers, *condition),
                    target: false_label,
                });
                emit_copies(
                    &mut emissions,
                    &mut next,
                    &registers,
                    &function.blocks[then_target.0 as usize].parameters,
                    then_arguments,
                )?;
                emissions.push(Emission::Jump(*then_target));
                labels[false_label.0 as usize] = Some(emissions.len());
                emit_copies(
                    &mut emissions,
                    &mut next,
                    &registers,
                    &function.blocks[else_target.0 as usize].parameters,
                    else_arguments,
                )?;
                emissions.push(Emission::Jump(*else_target));
            }
            ir::Terminator::Return(value) => {
                emissions.push(Emission::Instruction(vm::Instruction::Return {
                    source: reg(&registers, *value),
                }));
            }
        }
    }

    let instructions = emissions
        .into_iter()
        .map(|emission| match emission {
            Emission::Instruction(instruction) => Ok(instruction),
            Emission::Jump(target) => Ok(vm::Instruction::Jump {
                target: label(&labels, target)?,
            }),
            Emission::JumpIfFalse { condition, target } => Ok(vm::Instruction::JumpIfFalse {
                condition,
                target: label(&labels, target)?,
            }),
        })
        .collect::<Result<Vec<_>, LowerError>>()?;
    let spans = vec![Range::default(); instructions.len()];
    let parameter_types = function
        .signature
        .parameters
        .iter()
        .copied()
        .map(verification_type)
        .collect::<Vec<_>>();
    let parameter_count = parameter_types.len();
    Ok(vm::BytecodeFunction {
        name: function.name.clone(),
        intrinsic_stub: metadata.intrinsic_stub,
        parameters: u16::try_from(parameter_count)
            .map_err(|_| LowerError("too many function parameters".into()))?,
        parameter_types,
        parameter_modes: if metadata.parameter_modes.is_empty() {
            vec![crate::ast::ParameterMode::Borrow; parameter_count]
        } else {
            metadata.parameter_modes
        },
        mutable_parameters: if metadata.mutable_parameters.is_empty() {
            vec![false; parameter_count]
        } else {
            metadata.mutable_parameters
        },
        returns_reference: metadata.returns_reference,
        captures: 0,
        capture_types: Vec::new(),
        result_type: verification_type(function.signature.result),
        registers: next,
        instructions,
        instruction_spans: spans,
    })
}

fn assign(
    registers: &mut [Option<Register>],
    value: Value,
    next: &mut u16,
) -> Result<(), LowerError> {
    if registers[value.0 as usize].is_none() {
        let register = Register(*next);
        *next = next
            .checked_add(1)
            .ok_or_else(|| LowerError("shared IR needs more than 65535 VM registers".into()))?;
        registers[value.0 as usize] = Some(register);
    }
    Ok(())
}

fn reg(registers: &[Option<Register>], value: Value) -> Register {
    registers[value.0 as usize].expect("all values assigned above")
}

fn label(labels: &[Option<usize>], block: Block) -> Result<usize, LowerError> {
    labels
        .get(block.0 as usize)
        .and_then(|label| *label)
        .ok_or_else(|| LowerError(format!("unresolved VM edge label b{}", block.0)))
}

fn emit_copies(
    emissions: &mut Vec<Emission>,
    next: &mut u16,
    registers: &[Option<Register>],
    destinations: &[Value],
    sources: &[Value],
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
            emissions.push(Emission::Instruction(vm::Instruction::Move {
                destination,
                source,
            }));
            continue;
        }
        let preserved = copies[0].0;
        let temporary = Register(*next);
        *next = next
            .checked_add(1)
            .ok_or_else(|| LowerError("parallel copy needs a register past r65535".into()))?;
        emissions.push(Emission::Instruction(vm::Instruction::Move {
            destination: temporary,
            source: preserved,
        }));
        for (_, source) in &mut copies {
            if *source == preserved {
                *source = temporary;
            }
        }
    }
    Ok(())
}

fn lower_instruction(
    instruction: &ir::Instruction,
    registers: &[Option<Register>],
    constants: &mut Vec<vm::Constant>,
    emissions: &mut Vec<Emission>,
) -> Result<(), LowerError> {
    let instruction = match instruction {
        ir::Instruction::Constant { destination, value } => {
            let value = match value {
                ir::Constant::Unit => vm::Constant::Unit,
                ir::Constant::Bool(value) => vm::Constant::Bool(*value),
                ir::Constant::Integer(value) => vm::Constant::Integer(*value),
                ir::Constant::Float(value) => vm::Constant::Float(*value),
                ir::Constant::CodePoint(value) => vm::Constant::CodePoint(*value),
                ir::Constant::RuntimeString(_) => {
                    return Err(LowerError(
                        "runtime-string addresses must be legalized before VM lowering".into(),
                    ));
                }
            };
            let constant = constants
                .iter()
                .position(|existing| existing == &value)
                .unwrap_or_else(|| {
                    constants.push(value);
                    constants.len() - 1
                });
            vm::Instruction::LoadConstant {
                destination: reg(registers, *destination),
                constant: u16::try_from(constant)
                    .map_err(|_| LowerError("too many VM constants".into()))?,
            }
        }
        ir::Instruction::Unary {
            destination,
            operator,
            operand,
        } => vm::Instruction::Unary {
            destination: reg(registers, *destination),
            operator: *operator,
            operand: reg(registers, *operand),
        },
        ir::Instruction::IntegerExtend {
            destination,
            operand,
        } => vm::Instruction::Move {
            destination: reg(registers, *destination),
            source: reg(registers, *operand),
        },
        ir::Instruction::Binary {
            destination,
            operator,
            left,
            right,
        } => vm::Instruction::Binary {
            destination: reg(registers, *destination),
            operator: *operator,
            left: reg(registers, *left),
            right: reg(registers, *right),
        },
        ir::Instruction::Call {
            destination,
            function,
            arguments,
        } => vm::Instruction::Call {
            destination: reg(registers, *destination),
            function: *function,
            arguments: arguments
                .iter()
                .map(|value| reg(registers, *value))
                .collect(),
        },
        ir::Instruction::RuntimeCall { helper, .. } => {
            return Err(LowerError(format!(
                "runtime helper `{helper}` has no portable VM opcode"
            )));
        }
        ir::Instruction::Assert { condition, message } => vm::Instruction::Assert {
            condition: reg(registers, *condition),
            message: message.map(|value| reg(registers, value)),
        },
    };
    emissions.push(Emission::Instruction(instruction));
    Ok(())
}

fn verification_type(ty: Type) -> VerificationType {
    match ty {
        Type::Unit => VerificationType::Unit,
        Type::Bool => VerificationType::Bool,
        Type::Int => VerificationType::Integer,
        Type::Float => VerificationType::Float,
        Type::CodePoint => VerificationType::CodePoint,
        Type::Byte => VerificationType::Byte,
        Type::String | Type::Arguments | Type::StringList => VerificationType::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::{Machine, Program};
    use la_arena::{Idx, RawIdx};

    #[test]
    fn branch_edges_receive_distinct_parallel_copies() {
        let function = ir::Function {
            name: "choose".into(),
            signature: ir::Signature {
                parameters: vec![Type::Bool, Type::Int, Type::Int],
                result: Type::Int,
            },
            parameters: vec![Value(0), Value(1), Value(2)],
            entry: Block(0),
            entry_arguments: vec![Value(0), Value(1), Value(2)],
            value_types: vec![
                Type::Bool,
                Type::Int,
                Type::Int,
                Type::Bool,
                Type::Int,
                Type::Int,
                Type::Int,
            ],
            blocks: vec![
                ir::BlockData {
                    parameters: vec![Value(3), Value(4), Value(5)],
                    instructions: vec![],
                    terminator: ir::Terminator::Branch {
                        condition: Value(3),
                        then_target: Block(1),
                        then_arguments: vec![Value(4)],
                        else_target: Block(1),
                        else_arguments: vec![Value(5)],
                    },
                },
                ir::BlockData {
                    parameters: vec![Value(6)],
                    instructions: vec![],
                    terminator: ir::Terminator::Return(Value(6)),
                },
            ],
        };
        let lowered = lower_function(
            &function,
            &HashMap::new(),
            &mut Vec::new(),
            FunctionMetadata::default(),
        )
        .unwrap();
        assert!(
            lowered
                .instructions
                .iter()
                .any(|instruction| matches!(instruction, vm::Instruction::JumpIfFalse { .. }))
        );
        assert_eq!(lowered.parameters, 3);
        assert!(lowered.registers >= 6);
    }

    #[test]
    fn parallel_copy_cycles_use_one_temporary() {
        let registers = vec![Some(Register(0)), Some(Register(1))];
        let mut emissions = Vec::new();
        let mut next = 2;
        emit_copies(
            &mut emissions,
            &mut next,
            &registers,
            &[Value(0), Value(1)],
            &[Value(1), Value(0)],
        )
        .unwrap();
        assert_eq!(next, 3);
        let moves = emissions
            .into_iter()
            .map(|emission| match emission {
                Emission::Instruction(vm::Instruction::Move {
                    destination,
                    source,
                }) => (destination, source),
                _ => panic!("parallel copies must contain only moves"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            moves,
            vec![
                (Register(2), Register(0)),
                (Register(0), Register(1)),
                (Register(1), Register(2)),
            ]
        );
    }

    #[test]
    fn de_ssa_bytecode_verifies_and_executes() {
        let function = ir::Function {
            name: "main".into(),
            signature: ir::Signature {
                parameters: vec![],
                result: Type::Int,
            },
            parameters: vec![],
            entry: Block(0),
            entry_arguments: vec![],
            value_types: vec![Type::Bool, Type::Int, Type::Int, Type::Int],
            blocks: vec![
                ir::BlockData {
                    parameters: vec![],
                    instructions: vec![
                        ir::Instruction::Constant {
                            destination: Value(0),
                            value: ir::Constant::Bool(true),
                        },
                        ir::Instruction::Constant {
                            destination: Value(1),
                            value: ir::Constant::Integer(41),
                        },
                        ir::Instruction::Constant {
                            destination: Value(2),
                            value: ir::Constant::Integer(99),
                        },
                    ],
                    terminator: ir::Terminator::Branch {
                        condition: Value(0),
                        then_target: Block(1),
                        then_arguments: vec![Value(1)],
                        else_target: Block(1),
                        else_arguments: vec![Value(2)],
                    },
                },
                ir::BlockData {
                    parameters: vec![Value(3)],
                    instructions: vec![],
                    terminator: ir::Terminator::Return(Value(3)),
                },
            ],
        };
        let mut constants = Vec::new();
        let lowered = lower_function(
            &function,
            &HashMap::new(),
            &mut constants,
            FunctionMetadata::default(),
        )
        .unwrap();
        let main: FunctionId = Idx::from_raw(RawIdx::from_u32(0));
        let mut program = Program {
            constants,
            main: Some(main),
            ..Program::default()
        };
        program.functions.insert(main, lowered);
        crate::vm::verify(&program).unwrap();
        assert_eq!(
            Machine::new(&program).run_main().unwrap(),
            vm::Value::Integer(41)
        );
    }
}
