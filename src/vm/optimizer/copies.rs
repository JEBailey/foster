use std::collections::HashMap;

use crate::hir::CaptureMode;

use super::super::{BytecodeFunction, Instruction, Program, Register};
use super::analysis::{definitions, successors};

type Copies = HashMap<Register, Register>;

pub(super) fn propagate(program: &mut Program) {
    for function in program.functions.values_mut() {
        let incoming = available_copies(function);
        for (index, instruction) in function.instructions.iter_mut().enumerate() {
            rewrite_uses(instruction, &incoming[index]);
        }
    }
}

fn available_copies(function: &BytecodeFunction) -> Vec<Copies> {
    let count = function.instructions.len();
    let mut predecessors = vec![Vec::new(); count];
    for index in 0..count {
        for successor in successors(&function.instructions, index) {
            predecessors[successor].push(index);
        }
    }
    let mut incoming = vec![Copies::new(); count];
    let mut outgoing = vec![Copies::new(); count];
    loop {
        let mut changed = false;
        for index in 0..count {
            let next_in = if index == 0 || predecessors[index].is_empty() {
                Copies::new()
            } else {
                intersect(
                    predecessors[index]
                        .iter()
                        .map(|previous| &outgoing[*previous]),
                )
            };
            let next_out = transfer(&function.instructions[index], next_in.clone());
            changed |= next_in != incoming[index] || next_out != outgoing[index];
            incoming[index] = next_in;
            outgoing[index] = next_out;
        }
        if !changed {
            return incoming;
        }
    }
}

fn transfer(instruction: &Instruction, mut copies: Copies) -> Copies {
    if matches!(
        instruction,
        Instruction::Call { .. } | Instruction::CallValue { .. } | Instruction::CallClosure { .. }
    ) {
        copies.clear();
    }
    for definition in definitions(instruction) {
        kill(&mut copies, definition);
    }
    if let Instruction::MakeClosure { captures, .. } = instruction {
        for (mode, register) in captures {
            if *mode == CaptureMode::Move {
                kill(&mut copies, *register);
            }
        }
    }
    if let Instruction::Move {
        destination,
        source,
    } = instruction
    {
        let source = resolve(&copies, *source);
        if *destination != source {
            copies.insert(*destination, source);
        }
    }
    copies
}

fn intersect<'a>(mut maps: impl Iterator<Item = &'a Copies>) -> Copies {
    let Some(first) = maps.next() else {
        return Copies::new();
    };
    let mut intersection = first.clone();
    for map in maps {
        intersection.retain(|destination, source| map.get(destination) == Some(source));
    }
    intersection
}

fn kill(copies: &mut Copies, register: Register) {
    copies.retain(|destination, source| *destination != register && *source != register);
}

fn resolve(copies: &Copies, mut register: Register) -> Register {
    while let Some(source) = copies.get(&register) {
        register = *source;
    }
    register
}

fn rewrite_uses(instruction: &mut Instruction, copies: &Copies) {
    let rewrite = |register: &mut Register| *register = resolve(copies, *register);
    match instruction {
        Instruction::Move { source, .. } => rewrite(source),
        Instruction::Unary { operand, .. } => rewrite(operand),
        Instruction::Binary { left, right, .. } => {
            rewrite(left);
            rewrite(right);
        }
        Instruction::MakeList { elements, .. } => elements.iter_mut().for_each(rewrite),
        Instruction::Index { object, index, .. } => {
            rewrite(object);
            rewrite(index);
        }
        Instruction::MakeRecord { fields, .. } => {
            fields
                .iter_mut()
                .for_each(|(_, register)| rewrite(register));
        }
        Instruction::MakeVariant { payload, .. } => payload.iter_mut().for_each(rewrite),
        Instruction::LoadField { object, .. } => rewrite(object),
        Instruction::MatchPattern { subject, .. } => rewrite(subject),
        Instruction::JumpIfFalse { condition, .. } => rewrite(condition),
        Instruction::Call { arguments, .. } => arguments.iter_mut().for_each(rewrite),
        Instruction::MakeClosure { captures, .. } => {
            captures.iter_mut().for_each(|(mode, register)| {
                if *mode == CaptureMode::Copy {
                    rewrite(register);
                }
            });
        }
        Instruction::CallValue {
            callee, arguments, ..
        } => {
            rewrite(callee);
            arguments.iter_mut().for_each(rewrite);
        }
        Instruction::CallClosure {
            captures,
            arguments,
            ..
        } => {
            captures.iter_mut().for_each(|(mode, register)| {
                if *mode == CaptureMode::Copy {
                    rewrite(register);
                }
            });
            arguments.iter_mut().for_each(rewrite);
        }
        Instruction::Return { source } => rewrite(source),
        Instruction::LoadConstant { .. } | Instruction::Jump { .. } => {}
        _ => {}
    }
}
