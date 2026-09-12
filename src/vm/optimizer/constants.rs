use std::collections::{HashMap, VecDeque};

use crate::vm::Value;

use super::super::{Constant, Instruction, Program, Register, operations};
use super::analysis::{definitions, successors};

type Constants = HashMap<Register, Constant>;

pub(super) fn fold(program: &mut Program) {
    let (constants, functions) = (&mut program.constants, &mut program.functions);
    for function in functions.values_mut() {
        // Converge before rewriting: back edges can invalidate initial facts.
        let incoming = available_constants(&function.instructions, constants);
        for (index, instruction) in function.instructions.iter_mut().enumerate() {
            let Some(known) = &incoming[index] else {
                continue;
            };
            if let Instruction::JumpIfFalse { condition, target } = instruction
                && let Some(Constant::Bool(condition)) = known.get(condition)
            {
                *instruction = Instruction::Jump {
                    target: if *condition { index + 1 } else { *target },
                };
            } else if matches!(
                instruction,
                Instruction::Unary { .. } | Instruction::Binary { .. }
            ) && let Some((destination, value)) = evaluate(instruction, known, constants)
                && let Some(constant) = intern(constants, value)
            {
                *instruction = Instruction::LoadConstant {
                    destination,
                    constant,
                };
            }
        }
    }
}

fn available_constants(
    instructions: &[Instruction],
    constants: &[Constant],
) -> Vec<Option<Constants>> {
    // None means unreachable; a missing register in a reachable map is unknown.
    // Meet retains only bit-identical constants from all incoming paths.
    let mut incoming = vec![None; instructions.len()];
    if instructions.is_empty() {
        return incoming;
    }
    incoming[0] = Some(Constants::new());
    let mut pending = VecDeque::from([0]);
    let mut queued = vec![false; instructions.len()];
    queued[0] = true;
    while let Some(index) = pending.pop_front() {
        queued[index] = false;
        let outgoing = transfer(
            &instructions[index],
            incoming[index].as_ref().unwrap().clone(),
            constants,
        );
        for next in successors(instructions, index) {
            let changed = match &mut incoming[next] {
                None => {
                    incoming[next] = Some(outgoing.clone());
                    true
                }
                Some(known) => {
                    let old_len = known.len();
                    known.retain(|register, value| {
                        outgoing
                            .get(register)
                            .is_some_and(|other| same_constant(value, other))
                    });
                    known.len() != old_len
                }
            };
            if changed && !queued[next] {
                queued[next] = true;
                pending.push_back(next);
            }
        }
    }
    incoming
}

fn evaluate(
    instruction: &Instruction,
    known: &Constants,
    constants: &[Constant],
) -> Option<(Register, Constant)> {
    let (destination, value) = match instruction {
        Instruction::LoadConstant {
            destination,
            constant,
        } => (*destination, constants[usize::from(*constant)].clone()),
        Instruction::Move {
            destination,
            source,
        } => (*destination, known.get(source)?.clone()),
        Instruction::Unary {
            destination,
            operator,
            operand,
        } => (
            *destination,
            value_constant(
                operations::unary(
                    *operator,
                    &operations::constant_value(known.get(operand)?, None, None),
                )
                .ok()?,
            )?,
        ),
        Instruction::Binary {
            destination,
            operator,
            left,
            right,
        } => (
            *destination,
            value_constant(
                operations::binary(
                    *operator,
                    &operations::constant_value(known.get(left)?, None, None),
                    &operations::constant_value(known.get(right)?, None, None),
                )
                .ok()?,
            )?,
        ),
        _ => return None,
    };
    Some((destination, value))
}

fn transfer(instruction: &Instruction, mut known: Constants, constants: &[Constant]) -> Constants {
    let value = evaluate(instruction, &known, constants);
    if matches!(
        instruction,
        Instruction::Call { .. } | Instruction::CallValue { .. } | Instruction::CallClosure { .. }
    ) {
        known.clear();
    }
    // Kill definitions even when evaluation fails or an operand is unknown.
    for definition in definitions(instruction) {
        known.remove(&definition);
    }
    if let Instruction::MakeClosure { captures, .. } = instruction {
        for (_, register) in captures {
            known.remove(register);
        }
    }
    if let Some((destination, value)) = value {
        known.insert(destination, value);
    }
    known
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

fn value_constant(value: Value) -> Option<Constant> {
    match value {
        Value::Unit => Some(Constant::Unit),
        Value::Bool(value) => Some(Constant::Bool(value)),
        Value::Integer(value) => Some(Constant::Integer(value)),
        Value::Float(value) => Some(Constant::Float(value)),
        Value::Record { .. } if value.string_bytes().is_some() => Some(Constant::String(
            std::str::from_utf8(value.string_bytes()?).ok()?.to_owned(),
        )),
        Value::CodePoint(value) => Some(Constant::CodePoint(value)),
        value if value.symbol_bytes().is_some() => Some(Constant::Symbol(
            String::from_utf8(value.symbol_bytes()?.to_vec()).ok()?,
        )),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::BinaryOp;

    fn load(register: u16, constant: u16) -> Instruction {
        Instruction::LoadConstant {
            destination: Register(register),
            constant,
        }
    }
    fn ret() -> Instruction {
        Instruction::Return {
            source: Register(0),
        }
    }
    fn diamond() -> Vec<Instruction> {
        vec![
            Instruction::JumpIfFalse {
                condition: Register(2),
                target: 3,
            },
            load(0, 0),
            Instruction::Jump { target: 4 },
            load(0, 1),
            ret(),
        ]
    }

    #[test]
    fn joins_require_bit_identical_constants() {
        for (left, right, agrees) in [
            (Constant::Integer(42), Constant::Integer(42), true),
            (Constant::Integer(42), Constant::Integer(43), false),
            (Constant::Float(0.0), Constant::Float(-0.0), false),
            (Constant::Float(f64::NAN), Constant::Float(f64::NAN), true),
        ] {
            let facts = available_constants(&diamond(), &[left, right]);
            assert_eq!(
                facts[4].as_ref().unwrap().contains_key(&Register(0)),
                agrees
            );
        }
    }

    #[test]
    fn loop_back_edges_invalidate_constants_but_keep_invariants() {
        let instructions = vec![
            load(0, 0),
            load(1, 0),
            Instruction::JumpIfFalse {
                condition: Register(2),
                target: 5,
            },
            load(0, 1),
            Instruction::Jump { target: 2 },
            ret(),
        ];
        let facts =
            available_constants(&instructions, &[Constant::Integer(1), Constant::Integer(2)]);
        let header = facts[2].as_ref().unwrap();
        assert!(!header.contains_key(&Register(0)));
        assert_eq!(header.get(&Register(1)), Some(&Constant::Integer(1)));
    }

    #[test]
    fn unreachable_predecessors_do_not_destroy_facts() {
        let instructions = vec![
            load(0, 0),
            Instruction::Jump { target: 3 },
            load(0, 1),
            ret(),
        ];
        let facts = available_constants(
            &instructions,
            &[Constant::Integer(42), Constant::Integer(0)],
        );
        assert!(facts[2].is_none());
        assert_eq!(
            facts[3].as_ref().unwrap().get(&Register(0)),
            Some(&Constant::Integer(42))
        );
    }

    #[test]
    fn calls_and_failed_evaluation_kill_stale_facts() {
        let known = Constants::from([(Register(0), Constant::Integer(1))]);
        let unknown = Instruction::Binary {
            destination: Register(0),
            operator: BinaryOp::Add,
            left: Register(0),
            right: Register(1),
        };
        assert!(transfer(&unknown, known.clone(), &[]).is_empty());
        let zero = Constants::from([(Register(0), Constant::Integer(0))]);
        let division = Instruction::Binary {
            destination: Register(0),
            operator: BinaryOp::Divide,
            left: Register(0),
            right: Register(0),
        };
        assert!(transfer(&division, zero, &[]).is_empty());
        let call = Instruction::CallValue {
            destination: Register(1),
            callee: Register(2),
            arguments: vec![],
        };
        assert!(transfer(&call, known, &[]).is_empty());
    }
}
