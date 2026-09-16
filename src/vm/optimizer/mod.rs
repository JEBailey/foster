//! Semantics-preserving rewrites over Foster's executable register IR.

use super::{Instruction, Program};
use crate::error::FosterError;

pub(crate) mod analysis;
mod closures;
mod constants;
mod control_flow;
mod copies;
mod drops;
#[cfg(test)]
mod effects;
// Legacy register algorithms remain only as test oracles during migration.
#[cfg(test)]
mod inlining;
mod registers;

/// Optimizes a complete bytecode program while retaining one source span per instruction.
/// Returns validation or lowering errors without changing the input program.
pub fn optimize(program: &mut Program) -> Result<(), FosterError> {
    let shared = crate::codegen::vm::seal_program(program.clone())
        .map_err(|error| FosterError::runtime(format!("shared SSA sealing failed: {error}")))?
        .optimized()?;
    let mut lowered = crate::codegen::vm::lower_shared_program(shared)
        .map_err(|error| FosterError::runtime(format!("shared VM lowering failed: {error}")))?;
    finish_backend(&mut lowered);
    finalize_register_drops(&mut lowered);
    *program = lowered;
    Ok(())
}

pub(crate) fn finish_backend(program: &mut Program) {
    // Exclude storage-sensitive functions from register and closure rewrites.
    // Restore them before remapping the shared constant pool.
    let deferred_ids = program
        .functions
        .iter()
        .filter(|(_, function)| storage_identity_barrier(function))
        .map(|(id, _)| *id)
        .collect::<Vec<_>>();
    let deferred = deferred_ids
        .iter()
        .copied()
        .filter_map(|id| program.functions.remove(&id).map(|function| (id, function)))
        .collect::<Vec<_>>();

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

/// Structural rewrites still require proof that storage identity is unobservable.
/// This is intentionally stronger than the instruction-level folding barriers.
fn storage_identity_barrier(function: &super::BytecodeFunction) -> bool {
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

/// Account for edge-copy temporaries and representation rewrites introduced
/// after shared ownership lowering. Existing explicit releases are retained.
pub(crate) fn finalize_register_drops(program: &mut Program) {
    program.drops_inserted = false;
    drops::insert(program);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::{self, Constant, Machine, Value};

    #[test]
    fn repeated_optimized_builds_have_identical_constant_numbering() {
        let compilation = crate::compile(
            "func first() -> Int { 20 + 22 }
             func second() -> Int { 3 * 7 }
             func main() -> Int { first() + second() }",
        )
        .unwrap();
        let first = vm::encode_program(&vm::compile(&compilation).unwrap()).unwrap();
        for _ in 0..4 {
            let next = vm::encode_program(&vm::compile(&compilation).unwrap()).unwrap();
            assert!(
                first == next,
                "optimized bytecode depends on function map ordering"
            );
        }
    }

    #[test]
    fn folds_scalars_inside_mutating_functions_without_reusing_places() {
        let compilation = crate::compile(
            "func update[g: group Int](value: ref[g] Int) -> Int [mut g] {
                 let before = 20 + 22
                 value = value + 1
                 let after = 3 * 4
                 before + after + value
             }
             func main() -> Int {
                 let value = 5
                 update(ref value) + value
             }",
        )
        .unwrap();
        let mut program =
            vm::compile_with_options(&compilation, vm::CompileOptions { optimize: false }).unwrap();
        let id = *program
            .functions
            .iter()
            .find(|(_, f)| f.name == "update")
            .unwrap()
            .0;
        let original = program.functions[&id].clone();
        let count = |function: &vm::BytecodeFunction| {
            function
                .instructions
                .iter()
                .filter(|i| matches!(i, Instruction::Binary { .. }))
                .count()
        };
        let expected = Machine::new(&program).run_main().unwrap();
        optimize(&mut program).unwrap();
        vm::verify(&program).unwrap();
        assert!(count(&program.functions[&id]) < count(&original));
        assert_eq!(program.functions[&id].registers, original.registers);
        assert!(
            program.functions[&id]
                .instruction_spans
                .iter()
                .all(|span| original.instruction_spans.contains(span))
        );
        assert_eq!(Machine::new(&program).run_main().unwrap(), expected);
        assert_eq!(expected, Value::Integer(66));
    }

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
        optimize(&mut program).unwrap();
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
        assert_eq!(after.instructions.len(), after.instruction_spans.len());
        assert!(
            after
                .instruction_spans
                .iter()
                .all(|span| before.instruction_spans.contains(span))
        );
        assert!(after.instructions.iter().any(|i| matches!(i, Instruction::LoadConstant { constant, .. } if program.metadata.constants[usize::from(*constant)] == Constant::Integer(7))));
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
