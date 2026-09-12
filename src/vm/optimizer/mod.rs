//! Semantics-preserving rewrites over Foster's executable register IR.

use super::{Instruction, Program};

pub(crate) mod analysis;
mod closures;
mod constants;
mod control_flow;
mod copies;
mod drops;
mod inlining;
mod registers;

/// Optimizes a complete bytecode program while retaining one source span per instruction.
pub fn optimize(program: &mut Program) {
    // Exclude barrier functions from local rewrites and inlining candidates.
    // Restore them before remapping the shared constant pool.
    let deferred_ids = program
        .functions
        .iter()
        .filter(|(_, function)| optimization_barrier(function))
        .map(|(id, _)| *id)
        .collect::<Vec<_>>();
    let deferred = deferred_ids
        .into_iter()
        .filter_map(|id| program.functions.remove(&id).map(|function| (id, function)))
        .collect::<Vec<_>>();
    inlining::inline_small_functions(program);
    constants::fold(program);
    control_flow::simplify(program);
    copies::propagate(program);
    registers::eliminate_dead_writes(program);
    control_flow::simplify(program);
    closures::specialize_non_escaping(program);
    registers::eliminate_dead_writes(program);
    control_flow::simplify(program);
    registers::compact(program);
    control_flow::simplify(program);
    registers::compact(program);
    program.functions.extend(deferred);
    constants::deduplicate(program);
}

fn optimization_barrier(function: &super::BytecodeFunction) -> bool {
    function.mutable_parameters.iter().any(|mutable| *mutable)
        || function.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::StoreField { .. }
                    | Instruction::LoadField {
                        by_reference: true,
                        ..
                    }
                    | Instruction::StoreIndex { .. }
                    | Instruction::MakeReference { .. }
                    | Instruction::MakeWholeReference { .. }
                    | Instruction::MakeFieldReference { .. }
                    | Instruction::MoveOut { .. }
                    | Instruction::Push { .. }
                    | Instruction::Append { .. }
                    | Instruction::Contains { .. }
                    | Instruction::Builtin { .. }
                    | Instruction::SpawnRemote { .. }
                    | Instruction::SpawnRemoteBorrow { .. }
                    | Instruction::RemoteCall { .. }
                    | Instruction::Await { .. }
                    | Instruction::CallMethod { .. }
                    | Instruction::CallContractMethod { .. }
            )
        })
}

/// Inserts deterministic register releases after all representational rewrites.
pub(crate) fn insert_drops(program: &mut Program) {
    drops::insert(program);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::{self, Constant, Machine, Value};

    #[test]
    fn direct_and_contract_calls_preserve_receiver_storage() {
        let compilation = crate::compile(
            "type Probe = { func done(self) -> Bool }
             type Counter = & Probe & { value: Int }
             impl Counter { func done(self: Counter) -> Bool { false } }
             func check(value: Probe) -> Bool { value.done() }
             func main() -> Int {
                 let counter = Counter { value: 42 }
                 assert(not counter.done())
                 assert(not check(counter))
                 counter.value
             }",
        )
        .unwrap();
        for optimize in [false, true] {
            let program =
                vm::compile_with_options(&compilation, vm::CompileOptions { optimize }).unwrap();
            vm::verify(&program).unwrap();
            assert_eq!(
                Machine::new(&program).run_main().unwrap(),
                Value::Integer(42)
            );
        }
    }

    #[test]
    fn barrier_functions_keep_constants_without_disabling_pure_optimization() {
        let compilation = crate::compile(
            "func answer() -> Int { 20 + 22 }
             func replace[g: group Int](value: ref[g] Int) -> Int [mut g] {
                 value = 7
                 value
             }
             func main() -> Int {
                 let value = 0
                 replace(ref value)
                 answer() + value
             }",
        )
        .unwrap();
        let mut program =
            vm::compile_with_options(&compilation, vm::CompileOptions { optimize: false }).unwrap();
        let baseline = Machine::new(&program).run_main().unwrap();
        let barrier_id = *program
            .functions
            .iter()
            .find(|(_, f)| f.name == "replace")
            .unwrap()
            .0;
        let before = program.functions[&barrier_id].clone();
        optimize(&mut program);
        vm::verify(&program).unwrap();
        assert_eq!(Machine::new(&program).run_main().unwrap(), baseline);
        assert_eq!(baseline, Value::Integer(49));
        let answer = program
            .functions
            .values()
            .find(|f| f.name == "answer")
            .unwrap();
        assert!(
            !answer
                .instructions
                .iter()
                .any(|i| matches!(i, Instruction::Binary { .. }))
        );
        let after = &program.functions[&barrier_id];
        assert_eq!(before.instructions.len(), after.instructions.len());
        assert_eq!(before.instruction_spans, after.instruction_spans);
        assert!(after.instructions.iter().any(|i| matches!(i, Instruction::LoadConstant { constant, .. } if program.constants[usize::from(*constant)] == Constant::Integer(7))));
        let bytes = vm::encode_program(&program).unwrap();
        let decoded = vm::decode_program(&bytes).unwrap();
        assert_eq!(Machine::new(&decoded).run_main().unwrap(), baseline);
    }

    #[test]
    fn folds_arithmetic_after_agreeing_branch_assignments() {
        let compilation = crate::compile(
            "func answer(condition: Bool) -> Int {
                 let value = branch { condition -> 40 _ -> 40 }
                 value + 2
             }
             func main() -> Int { answer(true) + answer(false) }",
        )
        .unwrap();
        let program = vm::compile(&compilation).unwrap();
        vm::verify(&program).unwrap();
        let answer = program
            .functions
            .values()
            .find(|f| f.name == "answer")
            .unwrap();
        assert!(
            !answer
                .instructions
                .iter()
                .any(|i| matches!(i, Instruction::Binary { .. }))
        );
        assert_eq!(
            Machine::new(&program).run_main().unwrap(),
            Value::Integer(84)
        );
    }
}
