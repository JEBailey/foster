use std::collections::HashMap;

use crate::hir::CaptureMode;

use super::super::{Instruction, Program, Register};
use super::analysis::{definitions, uses};

pub(super) fn specialize_non_escaping(program: &mut Program) {
    for function in program.functions.values_mut() {
        let mut use_counts = HashMap::<Register, usize>::new();
        for instruction in &function.instructions {
            for register in uses(instruction) {
                *use_counts.entry(register).or_default() += 1;
            }
        }

        let candidates = function
            .instructions
            .iter()
            .enumerate()
            .filter_map(|(index, instruction)| match instruction {
                Instruction::MakeClosure {
                    destination,
                    function,
                    captures,
                } if use_counts.get(destination) == Some(&1) => {
                    Some((index, *destination, *function, captures.clone()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();

        for (creation, closure, target, captures) in candidates {
            let Some(call) =
                function
                    .instructions
                    .iter()
                    .enumerate()
                    .find_map(|(index, instruction)| match instruction {
                        Instruction::CallValue { callee, .. } if *callee == closure => Some(index),
                        _ => None,
                    })
            else {
                continue;
            };
            if call <= creation
                || !safe_to_delay_capture(
                    &function.instructions,
                    creation,
                    call,
                    closure,
                    &captures,
                )
            {
                continue;
            }
            let Instruction::CallValue {
                destination,
                arguments,
                ..
            } = function.instructions[call].clone()
            else {
                unreachable!("the call was located above")
            };
            function.instructions[creation] = Instruction::Move {
                destination: closure,
                source: closure,
            };
            function.instructions[call] = Instruction::CallClosure {
                destination,
                function: target,
                captures,
                arguments,
            };
        }
    }
}

fn safe_to_delay_capture(
    instructions: &[Instruction],
    creation: usize,
    call: usize,
    closure: Register,
    captures: &[(CaptureMode, Register)],
) -> bool {
    if captures.iter().any(|(mode, _)| *mode == CaptureMode::Move) {
        return call == creation + 1;
    }
    for instruction in &instructions[creation + 1..call] {
        if matches!(
            instruction,
            Instruction::Jump { .. }
                | Instruction::JumpIfFalse { .. }
                | Instruction::Return { .. }
                | Instruction::Call { .. }
                | Instruction::CallValue { .. }
                | Instruction::CallClosure { .. }
        ) {
            return false;
        }
        let definitions = definitions(instruction);
        if definitions.contains(&closure) {
            return false;
        }
        if captures.iter().any(|(mode, register)| {
            matches!(mode, CaptureMode::Copy | CaptureMode::Pending)
                && definitions.contains(register)
        }) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_to_specialize_a_redefined_closure_register() {
        let closure = Register(0);
        let instructions = vec![
            Instruction::Move {
                destination: closure,
                source: closure,
            },
            Instruction::LoadConstant {
                destination: closure,
                constant: 0,
            },
            Instruction::CallValue {
                destination: Register(1),
                callee: closure,
                arguments: Vec::new(),
            },
        ];

        assert!(!safe_to_delay_capture(&instructions, 0, 2, closure, &[]));
    }
}
