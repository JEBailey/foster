use std::collections::{HashMap, HashSet};

use crate::vm::Value;

use super::super::{Constant, Instruction, Program, Register, operations};

pub(super) fn fold(program: &mut Program) {
    let (constants, functions) = (&mut program.constants, &mut program.functions);
    for function in functions.values_mut() {
        let leaders = leaders(&function.instructions);
        let mut known = HashMap::<Register, Constant>::new();
        for (index, instruction) in function.instructions.iter_mut().enumerate() {
            if leaders.contains(&index) {
                known.clear();
            }
            if let Instruction::JumpIfFalse { condition, target } = instruction
                && let Some(Constant::Bool(condition)) = known.get(condition)
            {
                *instruction = Instruction::Jump {
                    target: if *condition { index + 1 } else { *target },
                };
                continue;
            }
            let replacement = match instruction {
                Instruction::LoadConstant {
                    destination,
                    constant,
                } => {
                    known.insert(*destination, constants[usize::from(*constant)].clone());
                    None
                }
                Instruction::Move {
                    destination,
                    source,
                } => {
                    if let Some(value) = known.get(source).cloned() {
                        known.insert(*destination, value);
                    } else {
                        known.remove(destination);
                    }
                    None
                }
                Instruction::Unary {
                    destination,
                    operator,
                    operand,
                } => known.get(operand).and_then(|value| {
                    operations::unary(*operator, &operations::constant_value(value))
                        .ok()
                        .and_then(value_constant)
                        .map(|value| (*destination, value))
                }),
                Instruction::Binary {
                    destination,
                    operator,
                    left,
                    right,
                } => known
                    .get(left)
                    .zip(known.get(right))
                    .and_then(|(left, right)| {
                        operations::binary(
                            *operator,
                            &operations::constant_value(left),
                            &operations::constant_value(right),
                        )
                        .ok()
                        .and_then(value_constant)
                        .map(|value| (*destination, value))
                    }),
                Instruction::Call { destination, .. }
                | Instruction::CallValue { destination, .. }
                | Instruction::CallClosure { destination, .. } => {
                    known.clear();
                    known.remove(destination);
                    None
                }
                Instruction::MakeClosure {
                    destination,
                    captures,
                    ..
                } => {
                    for (_, register) in captures {
                        known.remove(register);
                    }
                    known.remove(destination);
                    None
                }
                Instruction::MatchPattern {
                    destination,
                    bindings,
                    ..
                } => {
                    known.remove(destination);
                    for binding in bindings {
                        known.remove(binding);
                    }
                    None
                }
                _ => {
                    if let Some(destination) = destination(instruction) {
                        known.remove(&destination);
                    }
                    None
                }
            };
            if let Some((destination, value)) = replacement {
                if let Some(constant) = intern(constants, value.clone()) {
                    *instruction = Instruction::LoadConstant {
                        destination,
                        constant,
                    };
                    known.insert(destination, value);
                } else {
                    known.remove(&destination);
                }
            }
        }
    }
}

pub(super) fn deduplicate(program: &mut Program) {
    let old = std::mem::take(&mut program.constants);
    let mut unique = Vec::<Constant>::new();
    for function in program.functions.values_mut() {
        for instruction in &mut function.instructions {
            if let Instruction::LoadConstant { constant, .. } = instruction {
                let value = &old[usize::from(*constant)];
                let index = unique
                    .iter()
                    .position(|candidate| same_constant(candidate, value))
                    .unwrap_or_else(|| {
                        unique.push(value.clone());
                        unique.len() - 1
                    });
                *constant = index as u16;
            }
        }
    }
    program.constants = unique;
}

fn leaders(instructions: &[Instruction]) -> HashSet<usize> {
    let mut leaders = HashSet::from([0]);
    for (index, instruction) in instructions.iter().enumerate() {
        match instruction {
            Instruction::Jump { target } | Instruction::JumpIfFalse { target, .. } => {
                leaders.insert(*target);
                if index + 1 < instructions.len() {
                    leaders.insert(index + 1);
                }
            }
            Instruction::Return { .. } if index + 1 < instructions.len() => {
                leaders.insert(index + 1);
            }
            _ => {}
        }
    }
    leaders
}

fn destination(instruction: &Instruction) -> Option<Register> {
    match instruction {
        Instruction::LoadConstant { destination, .. }
        | Instruction::Move { destination, .. }
        | Instruction::Unary { destination, .. }
        | Instruction::Binary { destination, .. }
        | Instruction::MakeList { destination, .. }
        | Instruction::Index { destination, .. }
        | Instruction::MakeRecord { destination, .. }
        | Instruction::MakeVariant { destination, .. }
        | Instruction::LoadField { destination, .. }
        | Instruction::MatchPattern { destination, .. }
        | Instruction::Call { destination, .. }
        | Instruction::MakeClosure { destination, .. }
        | Instruction::CallValue { destination, .. }
        | Instruction::CallClosure { destination, .. } => Some(*destination),
        Instruction::Jump { .. } | Instruction::JumpIfFalse { .. } | Instruction::Return { .. } => {
            None
        }
        _ => None,
    }
}

fn value_constant(value: Value) -> Option<Constant> {
    match value {
        Value::Unit => Some(Constant::Unit),
        Value::Bool(value) => Some(Constant::Bool(value)),
        Value::Integer(value) => Some(Constant::Integer(value)),
        Value::Float(value) => Some(Constant::Float(value)),
        Value::String(value) => Some(Constant::String(value)),
        Value::Symbol(value) => Some(Constant::Symbol(value)),
        _ => None,
    }
}

fn intern(constants: &mut Vec<Constant>, value: Constant) -> Option<u16> {
    if let Some(index) = constants
        .iter()
        .position(|constant| same_constant(constant, &value))
    {
        return u16::try_from(index).ok();
    }
    let index = u16::try_from(constants.len()).ok()?;
    constants.push(value);
    Some(index)
}

fn same_constant(left: &Constant, right: &Constant) -> bool {
    match (left, right) {
        (Constant::Float(left), Constant::Float(right)) => left.to_bits() == right.to_bits(),
        _ => left == right,
    }
}
