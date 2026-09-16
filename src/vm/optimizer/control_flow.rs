use std::collections::HashSet;

use super::super::{BytecodeFunction, Instruction, Program};

pub(super) fn simplify(program: &mut Program) -> bool {
    let mut changed = false;
    for function in program.functions.values_mut() {
        let pinned = super::storage::pinned(function);
        changed |= redirect_jump_chains(function);
        changed |= retain(function, reachable(&function.instructions));
        let keep = function
            .instructions
            .iter()
            .enumerate()
            .map(|(index, instruction)| {
                !matches!(instruction, Instruction::Move { destination, source } if destination == source && !pinned.contains(destination))
                    && !matches!(instruction, Instruction::Jump { target } if *target == index + 1)
            })
            .collect();
        changed |= retain(function, keep);
        changed |= redirect_jump_chains(function);
    }
    changed
}

fn redirect_jump_chains(function: &mut BytecodeFunction) -> bool {
    let mut changed = false;
    let targets = function
        .instructions
        .iter()
        .map(|instruction| match instruction {
            Instruction::Jump { target } => Some(*target),
            _ => None,
        })
        .collect::<Vec<_>>();
    for instruction in &mut function.instructions {
        let target = match instruction {
            Instruction::Jump { target } | Instruction::JumpIfFalse { target, .. } => target,
            _ => continue,
        };
        let mut seen = HashSet::new();
        let original = *target;
        while let Some(Some(next)) = targets.get(*target) {
            if !seen.insert(*target) {
                break;
            }
            *target = *next;
        }
        changed |= original != *target;
    }
    changed
}

fn reachable(instructions: &[Instruction]) -> Vec<bool> {
    let mut reachable = vec![false; instructions.len()];
    let mut pending = vec![0];
    while let Some(index) = pending.pop() {
        if index >= instructions.len() || std::mem::replace(&mut reachable[index], true) {
            continue;
        }
        match &instructions[index] {
            Instruction::Jump { target } => pending.push(*target),
            Instruction::JumpIfFalse { target, .. } => {
                pending.push(*target);
                pending.push(index + 1);
            }
            Instruction::Return { .. } => {}
            _ => pending.push(index + 1),
        }
    }
    reachable
}

fn retain(function: &mut BytecodeFunction, keep: Vec<bool>) -> bool {
    if keep.iter().all(|keep| *keep) {
        return false;
    }
    let old_len = function.instructions.len();
    let mut next_kept = vec![None; old_len + 1];
    let mut next = None;
    for index in (0..old_len).rev() {
        if keep[index] {
            next = Some(index);
        }
        next_kept[index] = next;
    }
    let mut old_to_new = vec![None; old_len];
    let mut instructions = Vec::new();
    let mut spans = Vec::new();
    for (old, (instruction, span)) in function
        .instructions
        .drain(..)
        .zip(function.instruction_spans.drain(..))
        .enumerate()
    {
        if keep[old] {
            old_to_new[old] = Some(instructions.len());
            instructions.push(instruction);
            spans.push(span);
        }
    }
    for instruction in &mut instructions {
        if let Instruction::Jump { target } | Instruction::JumpIfFalse { target, .. } = instruction
        {
            let retained = next_kept[*target].expect("reachable jump target has a successor");
            *target = old_to_new[retained].expect("retained instruction has a new index");
        }
    }
    function.instructions = instructions;
    function.instruction_spans = spans;
    true
}
