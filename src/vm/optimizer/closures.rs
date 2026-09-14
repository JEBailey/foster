use std::collections::HashMap;

use crate::hir::CaptureMode;

use super::super::{Instruction, Program, Register};
use super::analysis::{definitions, uses};

pub(super) fn specialize_non_escaping(program: &mut Program) {
    for function in program.functions.values_mut() {
        let mut use_counts = HashMap::<Register, usize>::new();
        for instruction in &function.instructions {
            if matches!(instruction, Instruction::Drop { .. }) {
                continue;
            }
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
                    specialization,
                    captures,
                } if use_counts.get(destination) == Some(&1) => Some((
                    index,
                    *destination,
                    *function,
                    specialization.clone(),
                    captures.clone(),
                )),
                _ => None,
            })
            .collect::<Vec<_>>();

        for (creation, closure, target, specialization, captures) in candidates {
            let mut aliases = vec![closure];
            let mut transports = Vec::new();
            let mut found = None;
            for (index, instruction) in function.instructions.iter().enumerate().skip(creation + 1)
            {
                match instruction {
                    Instruction::Move {
                        destination,
                        source,
                    } if aliases.contains(source) && use_counts.get(destination) == Some(&1) => {
                        aliases.push(*destination);
                        transports.push(index);
                    }
                    Instruction::CallValue { callee, .. } if aliases.contains(callee) => {
                        found = Some(index);
                        break;
                    }
                    Instruction::Drop { .. } => {}
                    _ if uses(instruction)
                        .iter()
                        .chain(definitions(instruction).iter())
                        .any(|register| aliases.contains(register)) =>
                    {
                        break;
                    }
                    _ => {}
                }
            }
            let Some(call) = found else {
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
            let captured_registers = captures
                .iter()
                .map(|(_, register)| *register)
                .collect::<Vec<_>>();
            function.instructions[call] = Instruction::CallClosure {
                destination,
                function: target,
                specialization,
                captures,
                arguments,
            };
            // The closure's captured ownership is represented by source slots
            // until the direct call. Remove transport-only closure slots and
            // let final register liveness place the new releases.
            for transport in &transports {
                function.instructions[*transport] = Instruction::Move {
                    destination: closure,
                    source: closure,
                };
            }
            let mut active_aliases = aliases;
            for (index, instruction) in function
                .instructions
                .iter_mut()
                .enumerate()
                .skip(creation + 1)
            {
                if let Instruction::Drop { register } = instruction
                    && (active_aliases.contains(register)
                        || (index < call && captured_registers.contains(register)))
                {
                    *instruction = Instruction::Move {
                        destination: *register,
                        source: *register,
                    };
                } else if !transports.contains(&index) {
                    for register in definitions(instruction) {
                        active_aliases.retain(|alias| *alias != register);
                    }
                }
            }
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
    let moving = captures.iter().any(|(mode, _)| *mode == CaptureMode::Move);
    let mut aliases = vec![closure];
    for instruction in &instructions[creation + 1..call] {
        if let Instruction::Drop { register } = instruction {
            if aliases.contains(register) || captures.iter().any(|(_, source)| source == register) {
                continue;
            }
            // An unrelated release may run a destructor. Keep capture timing
            // unchanged across that observable operation.
            return false;
        }
        if let Instruction::Move {
            destination,
            source,
        } = instruction
            && aliases.contains(source)
        {
            aliases.push(*destination);
            continue;
        }
        if moving {
            return false;
        }
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
