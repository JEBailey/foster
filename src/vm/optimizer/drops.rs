use std::collections::{HashMap, HashSet};

use super::super::{BytecodeFunction, Instruction, Program, Register};
use super::analysis::{definitions, liveness, uses};

pub(super) fn insert(program: &mut Program) {
    let mut protected = protected_slots(program);
    for (id, function) in &mut program.functions {
        insert_function(function, protected.remove(id).unwrap_or_default());
    }
}

fn protected_slots(program: &Program) -> HashMap<crate::hir::FunctionId, HashSet<Register>> {
    let mut protected = HashMap::<_, HashSet<_>>::new();
    for (caller, function) in &program.functions {
        // Parameters can contain projected references. Assigning such a
        // parameter writes through its existing slot, so its slot identity is
        // needed even when its previous value is dead in ordinary SSA terms.
        let prefix = function.captures.saturating_add(function.parameters);
        for instruction in &function.instructions {
            for destination in definitions(instruction) {
                if destination.0 < prefix {
                    protected.entry(*caller).or_default().insert(destination);
                }
            }
        }
        for instruction in &function.instructions {
            if let Instruction::MakeReference { object, .. } = instruction {
                // PlaceHandle is weak; retain its origin slot for this frame.
                protected.entry(*caller).or_default().insert(*object);
            }
            match instruction {
                Instruction::MakeClosure {
                    function: target,
                    captures,
                    ..
                }
                | Instruction::CallClosure {
                    function: target,
                    captures,
                    ..
                } => {
                    for (index, (mode, source)) in captures.iter().enumerate() {
                        if *mode == crate::hir::CaptureMode::Ref {
                            protected.entry(*caller).or_default().insert(*source);
                            protected
                                .entry(*target)
                                .or_default()
                                .insert(Register(index as u16));
                        }
                    }
                }
                Instruction::CallMethod {
                    function: target, ..
                }
                | Instruction::RemoteCall {
                    function: target, ..
                } => {
                    let receiver = program.functions[target].captures;
                    protected
                        .entry(*target)
                        .or_default()
                        .insert(Register(receiver));
                }
                _ => {}
            }
        }
    }
    protected
}

fn insert_function(function: &mut BytecodeFunction, protected: HashSet<Register>) {
    if function.instructions.is_empty()
        || function
            .instructions
            .iter()
            .any(|instruction| matches!(instruction, Instruction::Drop { .. }))
    {
        return;
    }

    let live = liveness(function);
    let original = std::mem::take(&mut function.instructions);
    let original_spans = std::mem::take(&mut function.instruction_spans);
    let mut instructions = Vec::new();
    let mut spans = Vec::new();
    let mut old_to_new = vec![0; original.len()];
    let mut jump_patches = Vec::<(usize, usize)>::new();
    let mut branch_cleanups = Vec::<(usize, usize, Vec<Register>, std::ops::Range<usize>)>::new();

    let entry_span = original_spans.first().cloned().unwrap_or(0..0);
    let prefix = function.captures.saturating_add(function.parameters);
    let unused_prefix = (0..prefix)
        .map(Register)
        .filter(|register| !protected.contains(register) && !live.live_in[0].contains(register))
        .collect::<Vec<_>>();

    for (index, (instruction, span)) in original.into_iter().zip(original_spans).enumerate() {
        old_to_new[index] = instructions.len();
        if index == 0 {
            emit_drops(&mut instructions, &mut spans, &unused_prefix, &entry_span);
        }

        let dying = dying_registers(&instruction, &live.live_out[index], &protected);
        match instruction {
            Instruction::Jump { target } => {
                let emitted = instructions.len();
                instructions.push(Instruction::Jump { target: usize::MAX });
                spans.push(span);
                jump_patches.push((emitted, target));
            }
            Instruction::JumpIfFalse { condition, target } => {
                let false_drops =
                    edge_drops(&live.live_in[index], &live.live_in[target], &protected);
                let fallthrough_drops = if index + 1 < live.live_in.len() {
                    edge_drops(&live.live_in[index], &live.live_in[index + 1], &protected)
                } else {
                    Default::default()
                };
                let branch = instructions.len();
                instructions.push(Instruction::JumpIfFalse {
                    condition,
                    target: usize::MAX,
                });
                spans.push(span.clone());
                emit_drops(&mut instructions, &mut spans, &fallthrough_drops, &span);
                if false_drops.is_empty() {
                    jump_patches.push((branch, target));
                } else {
                    branch_cleanups.push((branch, target, false_drops, span));
                }
            }
            Instruction::Return { source } => {
                instructions.push(Instruction::Return { source });
                spans.push(span);
            }
            instruction => {
                instructions.push(instruction);
                spans.push(span.clone());
                emit_drops(&mut instructions, &mut spans, &dying, &span);
            }
        }
    }

    for (branch, target, dying, span) in branch_cleanups {
        let cleanup = instructions.len();
        let Instruction::JumpIfFalse {
            target: branch_target,
            ..
        } = &mut instructions[branch]
        else {
            unreachable!("branch cleanup refers to a conditional jump")
        };
        *branch_target = cleanup;
        emit_drops(&mut instructions, &mut spans, &dying, &span);
        let jump = instructions.len();
        instructions.push(Instruction::Jump { target: usize::MAX });
        spans.push(span);
        jump_patches.push((jump, target));
    }

    for (instruction, old_target) in jump_patches {
        let target = old_to_new[old_target];
        match &mut instructions[instruction] {
            Instruction::Jump {
                target: destination,
            }
            | Instruction::JumpIfFalse {
                target: destination,
                ..
            } => *destination = target,
            _ => unreachable!("jump patch refers to a non-jump instruction"),
        }
    }

    function.instructions = instructions;
    function.instruction_spans = spans;
}

fn dying_registers(
    instruction: &Instruction,
    live_out: &HashSet<Register>,
    protected: &HashSet<Register>,
) -> Vec<Register> {
    let mut dying = uses(instruction);
    dying.extend(definitions(instruction));
    dying.sort_by_key(|register| register.0);
    dying.dedup();
    dying.retain(|register| !protected.contains(register) && !live_out.contains(register));
    dying
}

fn edge_drops(
    current: &HashSet<Register>,
    successor: &HashSet<Register>,
    protected: &HashSet<Register>,
) -> Vec<Register> {
    let mut drops = current
        .difference(successor)
        .filter(|register| !protected.contains(register))
        .copied()
        .collect::<Vec<_>>();
    drops.sort_by_key(|register| register.0);
    drops
}

fn emit_drops(
    instructions: &mut Vec<Instruction>,
    spans: &mut Vec<std::ops::Range<usize>>,
    registers: &[Register],
    span: &std::ops::Range<usize>,
) {
    for register in registers {
        instructions.push(Instruction::Drop {
            register: *register,
        });
        spans.push(span.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_a_condition_on_both_branch_edges() {
        let mut function = BytecodeFunction {
            name: "branch".to_owned(),
            parameters: 1,
            parameter_modes: vec![crate::ast::ParameterMode::Borrow],
            mutable_parameters: vec![false],
            captures: 0,
            registers: 2,
            instructions: vec![
                Instruction::JumpIfFalse {
                    condition: Register(0),
                    target: 3,
                },
                Instruction::LoadConstant {
                    destination: Register(1),
                    constant: 0,
                },
                Instruction::Return {
                    source: Register(1),
                },
                Instruction::LoadConstant {
                    destination: Register(1),
                    constant: 0,
                },
                Instruction::Return {
                    source: Register(1),
                },
            ],
            instruction_spans: vec![0..1; 5],
        };

        insert_function(&mut function, HashSet::new());

        assert_eq!(
            function
                .instructions
                .iter()
                .filter(|instruction| matches!(instruction, Instruction::Drop { register } if *register == Register(0)))
                .count(),
            2
        );
    }
}
