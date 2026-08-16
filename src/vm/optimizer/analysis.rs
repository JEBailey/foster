use std::collections::HashSet;

use super::super::{BytecodeFunction, Instruction, Register};

pub(super) struct Liveness {
    pub(super) live_in: Vec<HashSet<Register>>,
    pub(super) live_out: Vec<HashSet<Register>>,
}

pub(super) fn successors(instructions: &[Instruction], index: usize) -> Vec<usize> {
    match &instructions[index] {
        Instruction::Jump { target } => vec![*target],
        Instruction::JumpIfFalse { target, .. } => {
            let mut successors = vec![*target];
            if index + 1 < instructions.len() {
                successors.push(index + 1);
            }
            successors
        }
        Instruction::Return { .. } => Vec::new(),
        _ if index + 1 < instructions.len() => vec![index + 1],
        _ => Vec::new(),
    }
}

pub(super) fn definitions(instruction: &Instruction) -> Vec<Register> {
    match instruction {
        Instruction::Drop {
            register: destination,
        }
        | Instruction::LoadConstant { destination, .. }
        | Instruction::Move { destination, .. }
        | Instruction::Unary { destination, .. }
        | Instruction::Binary { destination, .. }
        | Instruction::MakeList { destination, .. }
        | Instruction::Index { destination, .. }
        | Instruction::MakeRecord { destination, .. }
        | Instruction::MakeVariant { destination, .. }
        | Instruction::LoadField { destination, .. }
        | Instruction::MakeReference { destination, .. }
        | Instruction::MoveOut { destination, .. }
        | Instruction::Push { destination, .. }
        | Instruction::Append { destination, .. }
        | Instruction::Contains { destination, .. }
        | Instruction::Builtin { destination, .. }
        | Instruction::SpawnRemote { destination, .. }
        | Instruction::SpawnRemoteBorrow { destination, .. }
        | Instruction::RemoteCall { destination, .. }
        | Instruction::Await { destination, .. }
        | Instruction::Call { destination, .. }
        | Instruction::CallMethod { destination, .. }
        | Instruction::MakeClosure { destination, .. }
        | Instruction::CallValue { destination, .. }
        | Instruction::CallClosure { destination, .. } => vec![*destination],
        Instruction::MatchPattern {
            destination,
            bindings,
            ..
        } => {
            let mut definitions = Vec::with_capacity(bindings.len() + 1);
            definitions.push(*destination);
            definitions.extend(bindings);
            definitions
        }
        Instruction::StoreField { .. }
        | Instruction::StoreIndex { .. }
        | Instruction::Jump { .. }
        | Instruction::JumpIfFalse { .. }
        | Instruction::Return { .. } => Vec::new(),
    }
}

pub(super) fn uses(instruction: &Instruction) -> Vec<Register> {
    let mut uses = Vec::new();
    match instruction {
        Instruction::Drop { .. } => {}
        Instruction::Move { source, .. } => uses.push(*source),
        Instruction::Unary { operand, .. } => uses.push(*operand),
        Instruction::Binary { left, right, .. } => {
            uses.push(*left);
            uses.push(*right);
        }
        Instruction::MakeList { elements, .. } => uses.extend(elements),
        Instruction::Index { object, index, .. } => {
            uses.push(*object);
            uses.push(*index);
        }
        Instruction::MakeRecord { fields, .. } => {
            uses.extend(fields.iter().map(|(_, register)| register));
        }
        Instruction::MakeVariant { payload, .. } => uses.extend(payload),
        Instruction::LoadField { object, .. } => uses.push(*object),
        Instruction::StoreField { object, source, .. } => {
            uses.push(*object);
            uses.push(*source);
        }
        Instruction::StoreIndex {
            object,
            index,
            source,
        } => {
            uses.push(*object);
            uses.push(*index);
            uses.push(*source);
        }
        Instruction::MakeReference { object, index, .. } => {
            uses.push(*object);
            uses.push(*index);
        }
        Instruction::MoveOut { source, .. } => uses.push(*source),
        Instruction::Push { object, value, .. } | Instruction::Append { object, value, .. } => {
            uses.push(*object);
            uses.push(*value);
        }
        Instruction::Contains {
            value, candidates, ..
        } => {
            uses.push(*value);
            uses.extend(candidates);
        }
        Instruction::Builtin { arguments, .. } => uses.extend(arguments),
        Instruction::SpawnRemote { value, .. } => uses.push(*value),
        Instruction::SpawnRemoteBorrow { source, .. } => uses.push(*source),
        Instruction::RemoteCall {
            remote, arguments, ..
        } => {
            uses.push(*remote);
            uses.extend(arguments.iter().map(|(_, register)| register));
        }
        Instruction::Await { future, .. } => uses.push(*future),
        Instruction::MatchPattern { subject, .. } => uses.push(*subject),
        Instruction::JumpIfFalse { condition, .. } => uses.push(*condition),
        Instruction::Call { arguments, .. } => uses.extend(arguments),
        Instruction::CallMethod {
            receiver,
            arguments,
            ..
        } => {
            uses.push(*receiver);
            uses.extend(arguments);
        }
        Instruction::MakeClosure { captures, .. } => {
            uses.extend(captures.iter().map(|(_, register)| register));
        }
        Instruction::CallValue {
            callee, arguments, ..
        } => {
            uses.push(*callee);
            uses.extend(arguments);
        }
        Instruction::CallClosure {
            captures,
            arguments,
            ..
        } => {
            uses.extend(captures.iter().map(|(_, register)| register));
            uses.extend(arguments);
        }
        Instruction::Return { source } => uses.push(*source),
        Instruction::LoadConstant { .. } | Instruction::Jump { .. } => {}
    }
    uses
}

pub(super) fn liveness(function: &BytecodeFunction) -> Liveness {
    let count = function.instructions.len();
    let mut live_in = vec![HashSet::new(); count];
    let mut live_out = vec![HashSet::new(); count];
    loop {
        let mut changed = false;
        for index in (0..count).rev() {
            let next_out = successors(&function.instructions, index)
                .into_iter()
                .flat_map(|successor| live_in[successor].iter().copied())
                .collect::<HashSet<_>>();
            let definitions = definitions(&function.instructions[index])
                .into_iter()
                .collect::<HashSet<_>>();
            let mut next_in = uses(&function.instructions[index])
                .into_iter()
                .collect::<HashSet<_>>();
            next_in.extend(
                next_out
                    .iter()
                    .filter(|register| !definitions.contains(register))
                    .copied(),
            );
            changed |= next_out != live_out[index] || next_in != live_in[index];
            live_out[index] = next_out;
            live_in[index] = next_in;
        }
        if !changed {
            return Liveness { live_in, live_out };
        }
    }
}
