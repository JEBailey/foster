use std::collections::HashMap;

use crate::hir::FunctionId;

use super::super::{BytecodeFunction, Instruction, Program, Register, VerificationType};
use super::registers::rewrite_registers;

const INLINE_INSTRUCTION_LIMIT: usize = 16;
const CALLER_INSTRUCTION_BUDGET: usize = 128;
const INLINE_ROUND_LIMIT: usize = 4;

pub(super) fn inline_small_functions(program: &mut Program) {
    // Snapshot each round so iteration order cannot affect eligibility. Only
    // leaf bodies are copied: recursive cycles never become candidates, while
    // wrappers can become leaves after their callees have been expanded.
    for _ in 0..INLINE_ROUND_LIMIT {
        let candidates = program
            .functions
            .iter()
            .filter(|(_, function)| eligible(function))
            .map(|(id, function)| (*id, function.clone()))
            .collect::<HashMap<_, _>>();
        let mut changed = false;
        for (caller_id, caller) in &mut program.functions {
            changed |= inline_calls(*caller_id, caller, &candidates);
        }
        if !changed {
            break;
        }
    }
}

fn scalar(ty: &VerificationType) -> bool {
    matches!(
        ty,
        VerificationType::Unit
            | VerificationType::Bool
            | VerificationType::Integer
            | VerificationType::Float
            | VerificationType::CodePoint
            | VerificationType::Byte
    )
}

fn eligible(function: &BytecodeFunction) -> bool {
    if function.captures != 0
        || function.returns_reference
        || super::optimization_barrier(function)
        || function.instructions.len() > INLINE_INSTRUCTION_LIMIT
        || !matches!(
            function.instructions.last(),
            Some(Instruction::Return { .. })
        )
    {
        return false;
    }
    let branching = function.instructions.iter().any(|instruction| {
        matches!(
            instruction,
            Instruction::Jump { .. } | Instruction::JumpIfFalse { .. }
        )
    });
    // Broaden control-flow inlining for scalar bodies first. Aggregate branches
    // require modelling path-dependent ownership and cleanup when removing a frame.
    if branching && (!function.parameter_types.iter().all(scalar) || !scalar(&function.result_type))
    {
        return false;
    }
    function
        .instructions
        .iter()
        .enumerate()
        .all(|(index, instruction)| {
            if branching
                && !matches!(
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
            {
                return false;
            }
            match instruction {
                Instruction::Jump { target } | Instruction::JumpIfFalse { target, .. } => {
                    *target > index && *target < function.instructions.len()
                }
                Instruction::Call { .. }
                | Instruction::CallValue { .. }
                | Instruction::CallClosure { .. }
                | Instruction::MakeClosure { .. } => false,
                _ => true,
            }
        })
}

fn expansion_size(callee: &BytecodeFunction) -> usize {
    // Every early return needs a result move and a jump to the continuation.
    usize::from(callee.parameters)
        + callee.instructions.len()
        + callee
            .instructions
            .iter()
            .filter(|instruction| matches!(instruction, Instruction::Return { .. }))
            .count()
        - 1
}

fn inline_calls(
    caller_id: FunctionId,
    caller: &mut BytecodeFunction,
    candidates: &HashMap<FunctionId, BytecodeFunction>,
) -> bool {
    let old_len = caller.instructions.len();
    let mut projected_len = old_len;
    let mut old_to_new = vec![0; old_len];
    let mut caller_jumps = Vec::new();
    let mut instructions = Vec::new();
    let mut spans = Vec::new();
    let mut changed = false;
    for (old_index, (instruction, span)) in caller
        .instructions
        .drain(..)
        .zip(caller.instruction_spans.drain(..))
        .enumerate()
    {
        old_to_new[old_index] = instructions.len();
        let candidate = match &instruction {
            Instruction::Call {
                destination,
                function,
                arguments,
                ..
            } => candidates
                .get(function)
                .filter(|callee| {
                    *function != caller_id
                        && arguments.len() == usize::from(callee.parameters)
                        && projected_len - 1 + expansion_size(callee) <= CALLER_INSTRUCTION_BUDGET
                })
                .and_then(|callee| {
                    caller
                        .registers
                        .checked_add(callee.registers)
                        .map(|next| (callee, *destination, arguments, next))
                }),
            _ => None,
        };
        let Some((callee, destination, arguments, next_registers)) = candidate else {
            if matches!(
                instruction,
                Instruction::Jump { .. } | Instruction::JumpIfFalse { .. }
            ) {
                caller_jumps.push(instructions.len());
            }
            instructions.push(instruction);
            spans.push(span);
            continue;
        };
        let mapping = (0..callee.registers)
            .map(|register| (Register(register), Register(caller.registers + register)))
            .collect::<HashMap<_, _>>();
        let continuation = instructions.len() + expansion_size(callee);
        for (parameter, argument) in arguments.iter().enumerate() {
            instructions.push(Instruction::Move {
                destination: mapping[&Register(parameter as u16)],
                source: *argument,
            });
            spans.push(span.clone());
        }
        let mut offset = instructions.len();
        let callee_offsets = callee
            .instructions
            .iter()
            .enumerate()
            .map(|(index, instruction)| {
                let start = offset;
                offset += if matches!(instruction, Instruction::Return { .. })
                    && index + 1 < callee.instructions.len()
                {
                    2
                } else {
                    1
                };
                start
            })
            .collect::<Vec<_>>();
        for (index, instruction) in callee.instructions.iter().enumerate() {
            if let Instruction::Return { source } = instruction {
                instructions.push(Instruction::Move {
                    destination,
                    source: mapping[source],
                });
                spans.push(span.clone());
                if index + 1 < callee.instructions.len() {
                    instructions.push(Instruction::Jump {
                        target: continuation,
                    });
                    spans.push(span.clone());
                }
            } else {
                let mut instruction = instruction.clone();
                rewrite_registers(&mut instruction, &mapping);
                if let Instruction::Jump { target } | Instruction::JumpIfFalse { target, .. } =
                    &mut instruction
                {
                    *target = callee_offsets[*target];
                }
                instructions.push(instruction);
                spans.push(span.clone());
            }
        }
        projected_len += expansion_size(callee) - 1;
        caller.registers = next_registers;
        changed = true;
    }
    // Callee branches already use new offsets. Only original caller branches
    // refer to old instruction indices and need this final patching step.
    for index in caller_jumps {
        if let Instruction::Jump { target } | Instruction::JumpIfFalse { target, .. } =
            &mut instructions[index]
        {
            *target = old_to_new[*target];
        }
    }
    caller.instructions = instructions;
    caller.instruction_spans = spans;
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::{self, Constant, Machine, Value};

    fn run_both(source: &str, expected: i64) -> Program {
        let compilation = crate::compile(source).unwrap();
        for optimize in [false, true] {
            let program =
                vm::compile_with_options(&compilation, vm::CompileOptions { optimize }).unwrap();
            vm::verify(&program).unwrap();
            assert_eq!(
                Machine::new(&program).run_main().unwrap(),
                Value::Integer(expected)
            );
            assert!(
                program
                    .functions
                    .values()
                    .all(|f| f.instructions.len() == f.instruction_spans.len())
            );
        }
        vm::compile(&compilation).unwrap()
    }

    #[test]
    fn inlines_branches_and_early_returns_inside_caller_branches() {
        let program = run_both(
            "func choose(flag: Bool) -> Int { return 20 if flag
                 22 }
             func dispatch(flag: Bool) -> Int { branch { flag -> choose(false) _ -> choose(true) } }
             func main() -> Int { dispatch(true) + dispatch(false) }",
            42,
        );
        let dispatch = program
            .functions
            .values()
            .find(|f| f.name == "dispatch")
            .unwrap();
        assert!(
            !dispatch
                .instructions
                .iter()
                .any(|i| matches!(i, Instruction::Call { .. }))
        );
    }

    #[test]
    fn successive_rounds_inline_wrapper_chains() {
        let program = run_both(
            "func first(value: Int) -> Int { value + 1 }
             func second(value: Int) -> Int { first(value) }
             func third(value: Int) -> Int { second(value) }
             func main() -> Int { third(41) }",
            42,
        );
        assert!(
            !program.functions[&program.main.unwrap()]
                .instructions
                .iter()
                .any(|i| matches!(i, Instruction::Call { .. }))
        );
    }

    #[test]
    fn leaves_recursive_cycles_as_calls() {
        let program = run_both(
            "func left(value: Int) -> Int { return 0 if value == 0
                 right(value - 1) }
             func right(value: Int) -> Int { return 0 if value == 0
                 left(value - 1) }
             func main() -> Int { left(4) }",
            0,
        );
        for name in ["left", "right", "main"] {
            let function = program.functions.values().find(|f| f.name == name).unwrap();
            assert!(
                function
                    .instructions
                    .iter()
                    .any(|i| matches!(i, Instruction::Call { .. }))
            );
        }
    }

    #[test]
    fn preserves_arithmetic_failure_in_an_inlined_branch() {
        let compilation = crate::compile(
            "func choose(flag: Bool, value: Int) -> Int { return value + 1 if flag
                 0 }
             func main() -> Int { choose(true, 9223372036854775807) }",
        )
        .unwrap();
        for optimize in [false, true] {
            let program =
                vm::compile_with_options(&compilation, vm::CompileOptions { optimize }).unwrap();
            vm::verify(&program).unwrap();
            assert!(
                Machine::new(&program)
                    .run_main()
                    .unwrap_err()
                    .message
                    .contains("overflow")
            );
        }
    }

    #[test]
    fn respects_actual_growth_and_register_limits() {
        let compilation = crate::compile(
            "func leaf(value: Int) -> Int { value + 1 } func main() -> Int { leaf(41) }",
        )
        .unwrap();
        let program = vm::compile_library(&compilation).unwrap();
        let (&caller_id, template) = program
            .functions
            .iter()
            .find(|(_, f)| f.name == "main")
            .unwrap();
        let (callee_id, callee) = program
            .functions
            .iter()
            .find(|(_, f)| f.name == "leaf")
            .unwrap();
        let candidates = HashMap::from([(*callee_id, callee.clone())]);
        let mut caller = template.clone();
        while caller.instructions.len() < CALLER_INSTRUCTION_BUDGET {
            caller.instructions.push(Instruction::LoadConstant {
                destination: Register(0),
                constant: 0,
            });
            caller.instruction_spans.push(0..0);
        }
        let before = caller.clone();
        assert!(!inline_calls(caller_id, &mut caller, &candidates));
        assert_eq!(caller, before);
        let mut caller = template.clone();
        caller.registers = u16::MAX;
        let before = caller.clone();
        assert!(!inline_calls(caller_id, &mut caller, &candidates));
        assert_eq!(caller, before);
        assert!(
            program
                .constants
                .iter()
                .any(|c| matches!(c, Constant::Integer(41)))
        );
    }

    #[test]
    fn rejects_back_edges_and_reference_results() {
        let compilation = crate::compile("func main() -> Int { 42 }").unwrap();
        let program = vm::compile_library(&compilation).unwrap();
        let mut function = program
            .functions
            .values()
            .find(|f| f.name == "main")
            .unwrap()
            .clone();
        assert!(eligible(&function));
        function.returns_reference = true;
        assert!(!eligible(&function));
        function.returns_reference = false;
        function
            .instructions
            .insert(0, Instruction::Jump { target: 0 });
        assert!(!eligible(&function));
    }
}
