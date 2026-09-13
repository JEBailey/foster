use std::collections::HashSet;

use super::super::{BytecodeFunction, Instruction, Register};

pub(crate) struct Liveness {
    pub(crate) live_in: Vec<HashSet<Register>>,
    pub(crate) live_out: Vec<HashSet<Register>>,
}

pub(crate) fn successors(instructions: &[Instruction], index: usize) -> Vec<usize> {
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

pub(crate) fn definitions(instruction: &Instruction) -> Vec<Register> {
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
        | Instruction::MakeWholeReference { destination, .. }
        | Instruction::MakeFieldReference { destination, .. }
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
        | Instruction::CallContractMethod { destination, .. }
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
        | Instruction::Assert { .. }
        | Instruction::Return { .. } => Vec::new(),
    }
}

pub(crate) fn uses(instruction: &Instruction) -> Vec<Register> {
    let mut uses = Vec::new();
    match instruction {
        Instruction::Drop { register } => uses.push(*register),
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
        Instruction::MakeWholeReference { object, .. } => uses.push(*object),
        Instruction::MakeFieldReference { object, .. } => uses.push(*object),
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
        Instruction::Assert { condition, message } => {
            uses.push(*condition);
            uses.extend(message);
        }
        Instruction::Call { arguments, .. } => uses.extend(arguments),
        Instruction::CallMethod {
            receiver,
            arguments,
            ..
        }
        | Instruction::CallContractMethod {
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

pub(crate) fn liveness(function: &BytecodeFunction) -> Liveness {
    liveness_with_exit_uses(function, &HashSet::new())
}

pub(crate) fn liveness_with_exit_uses(
    function: &BytecodeFunction,
    exit_uses: &HashSet<Register>,
) -> Liveness {
    let count = function.instructions.len();
    let successors = (0..count)
        .map(|index| successors(&function.instructions, index))
        .collect::<Vec<_>>();
    let mut predecessors = vec![Vec::new(); count];
    let mut use_sets = Vec::with_capacity(count);
    let mut def_sets = Vec::with_capacity(count);
    let mut words = 0;
    for (index, instruction) in function.instructions.iter().enumerate() {
        for &successor in &successors[index] {
            predecessors[successor].push(index);
        }
        let mut reads = uses(instruction);
        if matches!(instruction, Instruction::Return { .. }) {
            reads.extend(exit_uses);
        }
        let writes = definitions(instruction);
        for register in reads.iter().chain(&writes) {
            words = words.max(usize::from(register.0) / 64 + 1);
        }
        use_sets.push(reads);
        def_sets.push(writes);
    }
    if words == 0 {
        return Liveness {
            live_in: vec![HashSet::new(); count],
            live_out: vec![HashSet::new(); count],
        };
    }
    // Cap the four dense matrices at 32 MiB. Very large, sparse functions
    // retain sparse sets, but still use cached operands and a worklist.
    if count.saturating_mul(words) > 1 << 20 {
        return sparse_liveness(&successors, &predecessors, use_sets, def_sets);
    }
    let mut use_bits = vec![0u64; count * words];
    let mut def_bits = vec![0u64; count * words];
    for index in 0..count {
        for register in &use_sets[index] {
            use_bits[index * words + usize::from(register.0) / 64] |= 1 << (register.0 % 64);
        }
        for register in &def_sets[index] {
            def_bits[index * words + usize::from(register.0) / 64] |= 1 << (register.0 % 64);
        }
    }
    drop(use_sets);
    drop(def_sets);
    // Dense rows eliminate allocation/hashing in the fixed-point loop. Start
    // with every instruction so unreachable components retain the old semantics.
    let mut live_in = vec![0u64; count * words];
    let mut live_out = vec![0u64; count * words];
    let mut pending = (0..count).rev().collect::<std::collections::VecDeque<_>>();
    let mut queued = vec![true; count];
    while let Some(index) = pending.pop_front() {
        queued[index] = false;
        let mut changed = false;
        for word in 0..words {
            let row = index * words + word;
            let outgoing = successors[index].iter().fold(0, |bits, successor| {
                bits | live_in[successor * words + word]
            });
            let incoming = use_bits[row] | (outgoing & !def_bits[row]);
            live_out[row] = outgoing;
            changed |= live_in[row] != incoming;
            live_in[row] = incoming;
        }
        if changed {
            for &predecessor in &predecessors[index] {
                if !queued[predecessor] {
                    queued[predecessor] = true;
                    pending.push_back(predecessor);
                }
            }
        }
    }
    // Convert once at the boundary; SSA sealing, drop insertion and register
    // allocation keep their existing set API.
    Liveness {
        live_in: register_sets(&live_in, words),
        live_out: register_sets(&live_out, words),
    }
}

fn sparse_liveness(
    successors: &[Vec<usize>],
    predecessors: &[Vec<usize>],
    uses: Vec<Vec<Register>>,
    definitions: Vec<Vec<Register>>,
) -> Liveness {
    let count = successors.len();
    let uses = uses
        .into_iter()
        .map(|values| values.into_iter().collect::<HashSet<_>>())
        .collect::<Vec<_>>();
    let definitions = definitions
        .into_iter()
        .map(|values| values.into_iter().collect::<HashSet<_>>())
        .collect::<Vec<_>>();
    let mut live_in = vec![HashSet::new(); count];
    let mut live_out = vec![HashSet::new(); count];
    let mut pending = (0..count).rev().collect::<std::collections::VecDeque<_>>();
    let mut queued = vec![true; count];
    while let Some(index) = pending.pop_front() {
        queued[index] = false;
        let outgoing = successors[index]
            .iter()
            .flat_map(|&next| live_in[next].iter().copied())
            .collect::<HashSet<_>>();
        let mut incoming = uses[index].clone();
        incoming.extend(outgoing.difference(&definitions[index]).copied());
        live_out[index] = outgoing;
        if incoming != live_in[index] {
            live_in[index] = incoming;
            for &previous in &predecessors[index] {
                if !queued[previous] {
                    queued[previous] = true;
                    pending.push_back(previous);
                }
            }
        }
    }
    Liveness { live_in, live_out }
}

fn register_sets(rows: &[u64], words: usize) -> Vec<HashSet<Register>> {
    rows.chunks_exact(words)
        .map(|row| {
            let mut registers =
                HashSet::with_capacity(row.iter().map(|word| word.count_ones() as usize).sum());
            for (index, &word) in row.iter().enumerate() {
                let mut remaining = word;
                while remaining != 0 {
                    registers.insert(Register(
                        (index * 64 + remaining.trailing_zeros() as usize) as u16,
                    ));
                    remaining &= remaining - 1;
                }
            }
            registers
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn reference(function: &BytecodeFunction, exit_uses: &HashSet<Register>) -> Liveness {
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
                if matches!(function.instructions[index], Instruction::Return { .. }) {
                    next_in.extend(exit_uses.iter().copied());
                }
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

    #[test]
    fn bitset_worklist_matches_reference_on_loops_joins_and_unreachable_code() {
        let compilation = crate::compile("func main() -> Int { 0 }").unwrap();
        let program = crate::vm::compile_library(&compilation).unwrap();
        let template = program
            .functions
            .values()
            .find(|f| f.name == "main")
            .unwrap();
        let mut seed = 7u32;
        for case in 0..32 {
            let mut next = || {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                seed
            };
            let mut function = template.clone();
            function.instructions = (0..64)
                .map(|_| {
                    let opcode = next() % 7;
                    let destination = Register((next() % 128) as u16);
                    let source = Register((next() % 128) as u16);
                    let target = next() as usize % 64;
                    match opcode {
                        0 => Instruction::LoadConstant {
                            destination,
                            constant: 0,
                        },
                        1 => Instruction::Move {
                            destination,
                            source,
                        },
                        2 => Instruction::Binary {
                            destination,
                            operator: crate::ast::BinaryOp::Add,
                            left: source,
                            right: Register(64),
                        },
                        3 => Instruction::JumpIfFalse {
                            condition: source,
                            target,
                        },
                        4 => Instruction::Jump { target },
                        5 => Instruction::Return { source },
                        _ => Instruction::Drop { register: source },
                    }
                })
                .collect();
            let exits = HashSet::from([Register(0), Register(63), Register(127)]);
            let actual = liveness_with_exit_uses(&function, &exits);
            let expected = reference(&function, &exits);
            assert_eq!(
                actual.live_in, expected.live_in,
                "incoming facts in case {case}"
            );
            assert_eq!(
                actual.live_out, expected.live_out,
                "outgoing facts in case {case}"
            );
        }
    }

    #[test]
    fn handles_empty_functions_and_sparse_high_registers() {
        let compilation = crate::compile("func main() -> Int { 0 }").unwrap();
        let program = crate::vm::compile_library(&compilation).unwrap();
        let mut function = program
            .functions
            .values()
            .find(|f| f.name == "main")
            .unwrap()
            .clone();
        function.instructions.clear();
        assert!(liveness(&function).live_in.is_empty());
        function.instructions = vec![Instruction::Jump { target: 0 }];
        assert_eq!(liveness(&function).live_in, vec![HashSet::new()]);
        function.instructions = vec![Instruction::Return {
            source: Register(65534),
        }];
        let exits = HashSet::from([Register(65535)]);
        let actual = liveness_with_exit_uses(&function, &exits);
        assert_eq!(actual.live_in, reference(&function, &exits).live_in);
        assert_eq!(actual.live_out, vec![HashSet::new()]);
    }
    #[test]
    fn sparse_fallback_matches_reference_for_large_register_layouts() {
        let compilation = crate::compile("func main() -> Int { 0 }").unwrap();
        let program = crate::vm::compile_library(&compilation).unwrap();
        let mut function = program
            .functions
            .values()
            .find(|f| f.name == "main")
            .unwrap()
            .clone();
        function.instructions = vec![
            Instruction::Move {
                destination: Register(65534),
                source: Register(65533)
            };
            2048
        ];
        function.instructions.push(Instruction::Return {
            source: Register(65534),
        });
        let actual = liveness(&function);
        let expected = reference(&function, &HashSet::new());
        assert_eq!(actual.live_in, expected.live_in);
        assert_eq!(actual.live_out, expected.live_out);
    }
}
