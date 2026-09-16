//! Storage identities that cannot participate in value-only register rewrites.
use super::super::{BytecodeFunction, Instruction, Register};
use super::analysis::{definitions, uses};
use std::collections::HashSet;

pub(super) fn pinned(function: &BytecodeFunction) -> HashSet<Register> {
    let prefix = function.captures.saturating_add(function.parameters);
    let mut pinned = (0..prefix).map(Register).collect::<HashSet<_>>();
    for instruction in &function.instructions {
        match instruction {
            Instruction::LoadConstant { .. }
            | Instruction::Move { .. }
            | Instruction::Unary { .. }
            | Instruction::Binary { .. }
            | Instruction::Drop { .. }
            | Instruction::Jump { .. }
            | Instruction::JumpIfFalse { .. }
            | Instruction::Return { .. }
            | Instruction::Assert { .. } => {}
            Instruction::MakeClosure { captures, .. } => {
                pinned.extend(captures.iter().filter_map(|(mode, source)| {
                    (*mode == crate::hir::CaptureMode::Ref).then_some(*source)
                }));
            }
            Instruction::CallValue {
                arguments,
                destination,
                ..
            } => {
                pinned.extend(arguments);
                pinned.insert(*destination);
            }
            // These operations can expose slots or produce projected places.
            // Pin their operands/results, not unrelated values in the function.
            _ => {
                pinned.extend(uses(instruction));
                pinned.extend(definitions(instruction));
            }
        }
    }
    loop {
        let before = pinned.len();
        for instruction in &function.instructions {
            if let Instruction::Move {
                destination,
                source,
            } = instruction
                && (pinned.contains(destination) || pinned.contains(source))
            {
                pinned.insert(*destination);
                pinned.insert(*source);
            }
        }
        if pinned.len() == before {
            return pinned;
        }
    }
}

pub(super) fn invalidates_copies(instruction: &Instruction) -> bool {
    !matches!(
        instruction,
        Instruction::LoadConstant { .. }
            | Instruction::Move { .. }
            | Instruction::Unary { .. }
            | Instruction::Binary { .. }
            | Instruction::Jump { .. }
            | Instruction::JumpIfFalse { .. }
            | Instruction::Return { .. }
            | Instruction::Assert { .. }
    )
}
