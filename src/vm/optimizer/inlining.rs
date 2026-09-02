use std::collections::HashMap;

use crate::hir::FunctionId;

use super::super::{BytecodeFunction, Instruction, Program, Register};
use super::registers::rewrite_registers;

const INLINE_INSTRUCTION_LIMIT: usize = 16;
const CALLER_INSTRUCTION_BUDGET: usize = 128;

pub(super) fn inline_small_leaf_functions(program: &mut Program) {
    let candidates = program
        .functions
        .iter()
        .filter(|(_, function)| eligible(function))
        .map(|(id, function)| (*id, function.clone()))
        .collect::<HashMap<_, _>>();

    for (caller_id, caller) in &mut program.functions {
        inline_calls(*caller_id, caller, &candidates);
    }
}

fn eligible(function: &BytecodeFunction) -> bool {
    function.captures == 0
        // Mutable borrows are caller-backed places at runtime. A Move into an
        // inlined parameter register would turn that binding into a value copy.
        && function.mutable_parameters.iter().all(|mutable| !mutable)
        && function.instructions.len() <= INLINE_INSTRUCTION_LIMIT
        && matches!(
            function.instructions.last(),
            Some(Instruction::Return { .. })
        )
        && function.instructions[..function.instructions.len() - 1]
            .iter()
            .all(|instruction| {
                !matches!(
                    instruction,
                    Instruction::Jump { .. }
                        | Instruction::JumpIfFalse { .. }
                        | Instruction::Return { .. }
                        | Instruction::Call { .. }
                        | Instruction::CallValue { .. }
                        | Instruction::CallClosure { .. }
                        | Instruction::MakeClosure { .. }
                )
            })
}

fn inline_calls(
    caller_id: FunctionId,
    caller: &mut BytecodeFunction,
    candidates: &HashMap<FunctionId, BytecodeFunction>,
) {
    let old_len = caller.instructions.len();
    let mut old_to_new = vec![0; old_len];
    let mut instructions = Vec::new();
    let mut spans = Vec::new();
    for (old_index, (instruction, span)) in caller
        .instructions
        .drain(..)
        .zip(caller.instruction_spans.drain(..))
        .enumerate()
    {
        old_to_new[old_index] = instructions.len();
        let Instruction::Call {
            destination,
            function,
            arguments,
            ..
        } = &instruction
        else {
            instructions.push(instruction);
            spans.push(span);
            continue;
        };
        let Some(callee) = candidates.get(function).filter(|callee| {
            *function != caller_id
                && arguments.len() == usize::from(callee.parameters)
                && instructions.len() + INLINE_INSTRUCTION_LIMIT <= CALLER_INSTRUCTION_BUDGET
        }) else {
            instructions.push(instruction);
            spans.push(span);
            continue;
        };
        let Some(next_registers) = caller.registers.checked_add(callee.registers) else {
            instructions.push(instruction);
            spans.push(span);
            continue;
        };
        let base = caller.registers;
        let mapping = (0..callee.registers)
            .map(|register| (Register(register), Register(base + register)))
            .collect::<HashMap<_, _>>();
        for (parameter, argument) in arguments.iter().enumerate() {
            instructions.push(Instruction::Move {
                destination: mapping[&Register(parameter as u16)],
                source: *argument,
            });
            spans.push(span.clone());
        }
        for callee_instruction in &callee.instructions[..callee.instructions.len() - 1] {
            let mut callee_instruction = callee_instruction.clone();
            rewrite_registers(&mut callee_instruction, &mapping);
            instructions.push(callee_instruction);
            spans.push(span.clone());
        }
        let Instruction::Return { source } = callee.instructions.last().unwrap() else {
            unreachable!("eligible callees end in return")
        };
        instructions.push(Instruction::Move {
            destination: *destination,
            source: mapping[source],
        });
        spans.push(span);
        caller.registers = next_registers;
    }
    for instruction in &mut instructions {
        if let Instruction::Jump { target } | Instruction::JumpIfFalse { target, .. } = instruction
        {
            *target = old_to_new[*target];
        }
    }
    caller.instructions = instructions;
    caller.instruction_spans = spans;
}
