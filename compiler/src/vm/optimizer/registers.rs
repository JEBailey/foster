use std::collections::{HashMap, HashSet};

use super::super::{BytecodeFunction, Instruction, Program, Register};
use super::analysis::{definitions, liveness, uses};
use super::storage::pinned;

pub(super) fn eliminate_dead_writes(program: &mut Program) -> bool {
    let mut changed = false;
    for function in program.functions.values_mut() {
        loop {
            let live = liveness(function);
            let pinned = pinned(function);
            let keep = function
                .instructions
                .iter()
                .enumerate()
                .map(|(index, instruction)| match instruction {
                    Instruction::LoadConstant { destination, .. } => {
                        live.live_out[index].contains(destination) || pinned.contains(destination)
                    }
                    Instruction::Move { destination, .. } => {
                        live.live_out[index].contains(destination) || pinned.contains(destination)
                    }
                    _ => true,
                })
                .collect::<Vec<_>>();
            if keep.iter().all(|keep| *keep) {
                break;
            }
            changed = true;
            retain_without_jumps(function, keep);
        }
    }
    changed
}

pub(super) fn compact(program: &mut Program) -> bool {
    let mut changed = false;
    for function in program.functions.values_mut() {
        let prefix = function.captures.saturating_add(function.parameters);
        let live = liveness(function);
        let pinned = pinned(function);
        let mut registers = (0..prefix).map(Register).collect::<HashSet<_>>();
        for instruction in &function.instructions {
            instruction.visit_registers(|register| {
                registers.insert(register);
            });
        }
        let mut interference = registers
            .iter()
            .copied()
            .map(|register| (register, HashSet::new()))
            .collect::<HashMap<_, _>>();
        add_clique(
            &mut interference,
            &(0..prefix).map(Register).collect::<Vec<_>>(),
        );
        for (index, instruction) in function.instructions.iter().enumerate() {
            add_clique(
                &mut interference,
                &live.live_in[index].iter().copied().collect::<Vec<_>>(),
            );
            let definitions = definitions(instruction);
            add_clique(&mut interference, &definitions);
            add_clique(&mut interference, &uses(instruction));
            if let Instruction::LoadField {
                destination,
                object,
                ..
            } = instruction
            {
                add_edge(&mut interference, *destination, *object);
            }
            for definition in definitions {
                for live in &live.live_out[index] {
                    add_edge(&mut interference, definition, *live);
                }
            }
        }
        for pin in &pinned {
            for register in &registers {
                add_edge(&mut interference, *pin, *register);
            }
        }

        let mut mapping = (0..prefix)
            .map(|register| (Register(register), Register(register)))
            .collect::<HashMap<_, _>>();
        let mut next_color = prefix;
        let mut pinned_tail = pinned
            .iter()
            .filter(|register| register.0 >= prefix)
            .copied()
            .collect::<Vec<_>>();
        pinned_tail.sort_by_key(|register| register.0);
        for register in pinned_tail {
            mapping.insert(register, Register(next_color));
            next_color += 1;
        }
        let mut remaining = registers
            .into_iter()
            .filter(|register| !mapping.contains_key(register))
            .collect::<Vec<_>>();
        remaining
            .sort_by_key(|register| (std::cmp::Reverse(interference[register].len()), register.0));
        for register in remaining {
            let neighbor_colors = interference[&register]
                .iter()
                .filter_map(|neighbor| mapping.get(neighbor))
                .map(|register| register.0)
                .collect::<HashSet<_>>();
            let color = (0..next_color)
                .find(|color| !neighbor_colors.contains(color))
                .unwrap_or_else(|| {
                    let color = next_color;
                    next_color += 1;
                    color
                });
            mapping.insert(register, Register(color));
        }
        changed |= function.registers != next_color
            || mapping.iter().any(|(before, after)| before != after);
        for instruction in &mut function.instructions {
            rewrite_registers(instruction, &mapping);
        }
        function.registers = next_color;
    }
    changed
}

fn add_clique(graph: &mut HashMap<Register, HashSet<Register>>, registers: &[Register]) {
    for (index, left) in registers.iter().enumerate() {
        for right in &registers[index + 1..] {
            add_edge(graph, *left, *right);
        }
    }
}

fn add_edge(graph: &mut HashMap<Register, HashSet<Register>>, left: Register, right: Register) {
    if left == right {
        return;
    }
    graph.entry(left).or_default().insert(right);
    graph.entry(right).or_default().insert(left);
}

fn retain_without_jumps(function: &mut BytecodeFunction, keep: Vec<bool>) {
    if keep.iter().all(|keep| *keep) {
        return;
    }
    let mut old_to_new = vec![0; keep.len() + 1];
    let mut removed = 0;
    for (index, keep) in keep.iter().copied().enumerate() {
        old_to_new[index] = index - removed;
        if !keep {
            removed += 1;
        }
    }
    old_to_new[keep.len()] = keep.len() - removed;
    for instruction in &mut function.instructions {
        if let Instruction::Jump { target } | Instruction::JumpIfFalse { target, .. } = instruction
        {
            *target = old_to_new[*target];
        }
    }
    let mut index = 0;
    function.instructions.retain(|_| {
        let retained = keep[index];
        index += 1;
        retained
    });
    index = 0;
    function.instruction_spans.retain(|_| {
        let retained = keep[index];
        index += 1;
        retained
    });
}

pub(super) fn rewrite_registers(
    instruction: &mut Instruction,
    mapping: &HashMap<Register, Register>,
) {
    instruction.visit_registers_mut(|register| *register = mapping[register]);
}
