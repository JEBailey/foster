//! Semantics-preserving rewrites over Foster's executable register IR.

#[cfg(test)]
use super::Instruction;
use super::Program;
use crate::error::FosterError;

pub(crate) use crate::codegen::storage::analysis;
mod closures;
mod constants;
mod control_flow;
mod copies;
use crate::codegen::storage::lifetimes as drops;
#[cfg(test)]
mod effects;
// Legacy register algorithms remain only as test oracles during migration.
#[cfg(test)]
mod inlining;
mod registers;
mod storage;

/// Optimizes a complete bytecode program while retaining one source span per instruction.
/// Returns validation or lowering errors without changing the input program.
pub fn optimize(program: &mut Program) -> Result<(), FosterError> {
    let shared = crate::codegen::sealing::seal_program(program.clone())
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
    use crate::compiler::profile::measure;
    measure("vm.cfg", || control_flow::simplify(program));
    let copies_changed = measure("vm.copies", || copies::propagate(program));
    let dead_writes_changed = measure("vm.dead_writes", || {
        registers::eliminate_dead_writes(program)
    });
    if copies_changed || dead_writes_changed {
        measure("vm.cfg", || control_flow::simplify(program));
    }
    if measure("vm.closures", || closures::specialize_non_escaping(program)) {
        measure("vm.dead_writes", || {
            registers::eliminate_dead_writes(program)
        });
        measure("vm.cfg", || control_flow::simplify(program));
    }
    // Coloring can turn copies into self-moves. Only rebuild interference if
    // cleanup actually changes the graph after coloring.
    if measure("vm.registers", || registers::compact(program))
        && measure("vm.cfg", || control_flow::simplify(program))
    {
        measure("vm.registers", || registers::compact(program));
    }
    measure("vm.constants", || constants::deduplicate(program));
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
    fn local_cleanup_preserves_reference_origins_in_the_same_function() {
        use crate::codegen::types::ExecutableType;
        use vm::Register as R;
        let id = crate::hir::FunctionId::from_raw(la_arena::RawIdx::from_u32(0));
        let mut program = Program::default();
        program.metadata.constants = vec![
            Constant::Integer(5),
            Constant::Integer(7),
            Constant::Integer(99),
        ];
        program.metadata.main = Some(id);
        let instructions = vec![
            Instruction::LoadConstant {
                destination: R(0),
                constant: 0,
            },
            Instruction::MakeWholeReference {
                destination: R(1),
                object: R(0),
                pointee_type: ExecutableType::Integer,
            },
            Instruction::LoadConstant {
                destination: R(4),
                constant: 1,
            },
            Instruction::Move {
                destination: R(0),
                source: R(4),
            },
            Instruction::LoadConstant {
                destination: R(2),
                constant: 2,
            },
            Instruction::Move {
                destination: R(3),
                source: R(2),
            },
            Instruction::Return { source: R(1) },
        ];
        program.functions.insert(
            id,
            vm::BytecodeFunction {
                name: "main".into(),
                intrinsic_stub: false,
                parameters: 0,
                parameter_types: vec![],
                parameter_modes: vec![],
                mutable_parameters: vec![],
                returns_reference: false,
                captures: 0,
                capture_types: vec![],
                result_type: ExecutableType::Integer,
                registers: 5,
                instruction_spans: vec![0..0; instructions.len()],
                instructions,
            },
        );
        vm::verify(&program).unwrap();
        assert_eq!(
            Machine::new(&program).run_main().unwrap(),
            Value::Integer(7)
        );
        finish_backend(&mut program);
        finalize_register_drops(&mut program);
        vm::verify(&program).unwrap();
        assert_eq!(
            Machine::new(&program).run_main().unwrap(),
            Value::Integer(7)
        );
        assert!(program.functions[&id].registers < 5);
        assert!(!program.metadata.constants.contains(&Constant::Integer(99)));
    }

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
        assert!(program.functions[&id].registers < original.registers);
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
