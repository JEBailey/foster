//! Foster's typed register-bytecode backend.
//!
//! The structured instruction representation intentionally doubles as the
//! executable format while the language semantics are still evolving.

mod binary;
mod builtins;
mod compiler;
mod host;
mod ir;
mod machine;
mod operations;
mod optimizer;
mod patterns;
mod runtime;
mod value;
mod verifier;

pub use binary::{BinaryError, FORMAT_VERSION, decode_program, encode_program};
pub use compiler::{CompileOptions, compile, compile_with_options};
pub use host::HostContext;
pub use ir::{
    BytecodeFunction, Constant, Instruction, Program, ProgramMetrics, Register, RuntimeRecord,
    RuntimeVariant,
};
pub use machine::Machine;
pub use optimizer::optimize;
pub use runtime::Capture;
pub use value::Value;
pub use verifier::verify;

pub fn run(compilation: &crate::compiler::Compilation) -> Result<Value, crate::error::FosterError> {
    run_with_options(compilation, CompileOptions::default())
}

pub fn run_with_options(
    compilation: &crate::compiler::Compilation,
    options: CompileOptions,
) -> Result<Value, crate::error::FosterError> {
    let program = compile_with_options(compilation, options)?;
    verify(&program)?;
    Machine::new(&program).run_main()
}

pub fn run_with_arguments(
    compilation: &crate::compiler::Compilation,
    options: CompileOptions,
    arguments: &crate::entry::CommandArguments,
) -> Result<Value, crate::error::FosterError> {
    let program = compile_with_options(compilation, options)?;
    verify(&program)?;
    Machine::new(&program).run_main_with_arguments(arguments)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::Value;

    #[test]
    fn executes_typed_hir_through_register_bytecode() {
        let compilation = crate::compile(
            "func add(left: Int, right: Int) -> Int { left + right }\n\
             func main() -> Int { add(20, 22) }",
        )
        .unwrap();
        let program = compile(&compilation).unwrap();
        verify(&program).unwrap();
        assert_eq!(
            Machine::new(&program).run_main().unwrap(),
            Value::Integer(42)
        );
        assert!(
            program.functions.values().all(|function| {
                function.instructions.len() == function.instruction_spans.len()
            })
        );
    }

    #[test]
    fn returned_references_preserve_their_live_origin() {
        let source = r#"
func preserve[g: group Int](value: ref[g] Int) -> ref[g] Int {
    ref value
}

func set[g: group Int](value: ref[g] Int, replacement: Int) -> Int [mut g] {
    value = replacement
}

func main() -> Int {
    let values = [10, 20]
    let selected = preserve(ref values[0])
    set(selected, 42)
    values.head
}
"#;
        let compilation = crate::compile(source).unwrap();
        let program =
            compile_with_options(&compilation, CompileOptions { optimize: false }).unwrap();
        let preserve = program
            .functions
            .values()
            .find(|function| function.name == "preserve")
            .unwrap();
        let main = &program.functions[&program.main.unwrap()];

        assert!(preserve.returns_reference);
        assert!(!main.returns_reference);
        assert_eq!(
            Machine::new(&program).run_main().unwrap(),
            Value::Integer(42)
        );
        assert_eq!(run(&compilation).unwrap(), Value::Integer(42));
    }

    #[test]
    fn returned_references_keep_structural_generation_checks() {
        let compilation = crate::compile(
            "func select(values: List<Int>) -> Int { values[0] }\n\
             func main() -> Int {\n\
                 let values = [10, 20]\n\
                 let selected = select(values)\n\
                 values.push(30)\n\
                 selected\n\
             }",
        )
        .unwrap();
        let mut program =
            compile_with_options(&compilation, CompileOptions { optimize: false }).unwrap();
        for function in program.functions.values_mut() {
            remove_drops(function);
        }
        let (_, select) = program
            .functions
            .iter_mut()
            .find(|(_, function)| function.name == "select")
            .unwrap();
        select.returns_reference = true;
        for instruction in &mut select.instructions {
            if let Instruction::Index {
                destination,
                object,
                index,
            } = instruction
            {
                *instruction = Instruction::MakeReference {
                    destination: *destination,
                    object: *object,
                    index: *index,
                };
            }
        }

        let error = Machine::new(&program).run_main().unwrap_err();
        assert!(
            error
                .message
                .contains("reference was invalidated by structural mutation"),
            "{}",
            error.message
        );
    }

    #[test]
    fn returned_reference_to_destroyed_frame_storage_expires_safely() {
        let compilation = crate::compile(
            "func select() -> Int {\n\
                 let values = [10]\n\
                 values[0]\n\
             }\n\
             func main() -> Int { select() }",
        )
        .unwrap();
        let mut program =
            compile_with_options(&compilation, CompileOptions { optimize: false }).unwrap();
        for function in program.functions.values_mut() {
            remove_drops(function);
        }
        let (_, select) = program
            .functions
            .iter_mut()
            .find(|(_, function)| function.name == "select")
            .unwrap();
        select.returns_reference = true;
        for instruction in &mut select.instructions {
            if let Instruction::Index {
                destination,
                object,
                index,
            } = instruction
            {
                *instruction = Instruction::MakeReference {
                    destination: *destination,
                    object: *object,
                    index: *index,
                };
            }
        }

        let error = Machine::new(&program).run_main().unwrap_err();
        assert!(
            error.message.contains("borrowed place has expired"),
            "{}",
            error.message
        );
    }

    fn remove_drops(function: &mut BytecodeFunction) {
        let (instructions, spans) = std::mem::take(&mut function.instructions)
            .into_iter()
            .zip(std::mem::take(&mut function.instruction_spans))
            .filter(|(instruction, _)| !matches!(instruction, Instruction::Drop { .. }))
            .unzip();
        function.instructions = instructions;
        function.instruction_spans = spans;
    }

    #[test]
    fn optimized_and_unoptimized_programs_are_semantically_equivalent() {
        let sources = [
            "func main() -> Int { branch { true -> 20 + 22 _ -> 0 } }",
            "func count(value: Int) -> Int { branch { value == 0 -> 42 _ -> count(value - 1) } }\nfunc main() -> Int { count(100) }",
            "type Pair = { left: Int, right: Int }\nfunc main() -> Int {\n let pair = Pair { left: 20, right: 22 }\n pair.left + pair.right\n}",
            "func main() -> Int {\n let offset = 2\n let multiply = [copy offset] (value: Int) -> { value * offset }\n multiply(21)\n}",
        ];

        for source in sources {
            let compilation = crate::compile(source).unwrap();
            let optimized =
                compile_with_options(&compilation, CompileOptions { optimize: true }).unwrap();
            let unoptimized =
                compile_with_options(&compilation, CompileOptions { optimize: false }).unwrap();
            verify(&optimized).unwrap();
            verify(&unoptimized).unwrap();
            assert_eq!(
                Machine::new(&optimized).run_main().unwrap(),
                Machine::new(&unoptimized).run_main().unwrap(),
                "optimization changed program behavior for {source}"
            );
        }
    }

    #[test]
    fn emits_register_drops_with_and_without_optimization() {
        let compilation =
            crate::compile("func main() -> Int {\n let values = [1, 2, 3]\n 42\n}").unwrap();
        for optimize in [false, true] {
            let program = compile_with_options(&compilation, CompileOptions { optimize }).unwrap();
            verify(&program).unwrap();
            assert!(program.functions.values().any(|function| {
                function
                    .instructions
                    .iter()
                    .any(|instruction| matches!(instruction, Instruction::Drop { .. }))
            }));
            assert_eq!(
                Machine::new(&program).run_main().unwrap(),
                Value::Integer(42)
            );
        }
    }

    #[test]
    fn optimizer_reduces_representative_bytecode() {
        let compilation = crate::compile(
            "func increment(value: Int) -> Int { value + 1 }
             func main() -> Int {
                 let unused = 100 + 200
                 let result = increment(20 + 21)
                 branch { true -> result _ -> unused }
             }",
        )
        .unwrap();
        let optimized =
            compile_with_options(&compilation, CompileOptions { optimize: true }).unwrap();
        let unoptimized =
            compile_with_options(&compilation, CompileOptions { optimize: false }).unwrap();
        let optimized_metrics = optimized.metrics();
        let unoptimized_metrics = unoptimized.metrics();

        assert!(optimized_metrics.instructions < unoptimized_metrics.instructions);
        assert!(optimized_metrics.registers < unoptimized_metrics.registers);
        assert!(optimized_metrics.constants <= unoptimized_metrics.constants);
        assert_eq!(
            Machine::new(&optimized).run_main().unwrap(),
            Machine::new(&unoptimized).run_main().unwrap()
        );
    }

    #[test]
    fn verifier_rejects_out_of_frame_registers() {
        let compilation = crate::compile("func main() -> Int { 42 }").unwrap();
        let mut program = compile(&compilation).unwrap();
        let main = program.main.unwrap();
        program.functions.get_mut(&main).unwrap().instructions[0] = Instruction::LoadConstant {
            destination: Register(u16::MAX),
            constant: 0,
        };
        assert!(verify(&program).is_err());
    }

    #[test]
    fn optimizes_constants_branches_registers_and_spans() {
        let compilation =
            crate::compile("func main() -> Int { branch { true -> 20 + 22 _ -> 0 } }").unwrap();
        let mut program = compile(&compilation).unwrap();
        optimize(&mut program);
        verify(&program).unwrap();

        let function = &program.functions[&program.main.unwrap()];
        assert_eq!(program.constants, [Constant::Integer(42)]);
        assert_eq!(function.registers, 1);
        assert_eq!(
            function.instructions.len(),
            function.instruction_spans.len()
        );
        assert!(function.instructions.iter().all(|instruction| !matches!(
            instruction,
            Instruction::Binary { .. } | Instruction::JumpIfFalse { .. }
        )));
        assert_eq!(
            Machine::new(&program).run_main().unwrap(),
            Value::Integer(42)
        );
    }

    #[test]
    fn propagates_binding_copies_and_reuses_dead_registers() {
        let compilation = crate::compile(
            "func advance(value: Int) -> Int {
                let first = value + 1
                let second = first + 2
                let third = second + 3
                third
            }
            func main() -> Int { advance(36) }",
        )
        .unwrap();
        let program = compile(&compilation).unwrap();
        verify(&program).unwrap();
        let function = program
            .functions
            .values()
            .find(|function| function.name == "advance")
            .unwrap();

        assert!(
            function
                .instructions
                .iter()
                .all(|instruction| !matches!(instruction, Instruction::Move { .. }))
        );
        assert_eq!(function.registers, 2);
        assert_eq!(
            Machine::new(&program).run_main().unwrap(),
            Value::Integer(42)
        );
    }

    #[test]
    fn inlines_small_leaf_functions_into_the_caller() {
        let compilation = crate::compile(
            "func increment(value: Int) -> Int { value + 1 }
             func main() -> Int { increment(41) }",
        )
        .unwrap();
        let program = compile(&compilation).unwrap();
        verify(&program).unwrap();
        let main = &program.functions[&program.main.unwrap()];

        assert!(
            main.instructions
                .iter()
                .all(|instruction| !matches!(instruction, Instruction::Call { .. }))
        );
        assert_eq!(main.instructions.len(), main.instruction_spans.len());
        assert_eq!(
            Machine::new(&program).run_main().unwrap(),
            Value::Integer(42)
        );

        let isolation = crate::compile(
            "func replace(value: Int) -> Int {
                value = 42
                value
             }
             func main() -> Int {
                let original = 1
                let changed = replace(original)
                original + changed
             }",
        )
        .unwrap();
        assert_eq!(run(&isolation).unwrap(), Value::Integer(43));
    }

    #[test]
    fn specializes_non_escaping_closure_calls_without_losing_captures() {
        let compilation = crate::compile(
            "func main() -> Int {
                let offset = 2
                ([copy offset] (value: Int) -> { value * offset })(21)
            }",
        )
        .unwrap();
        let program = compile(&compilation).unwrap();
        verify(&program).unwrap();
        let main = &program.functions[&program.main.unwrap()];

        assert!(
            main.instructions
                .iter()
                .any(|instruction| matches!(instruction, Instruction::CallClosure { .. }))
        );
        assert!(main.instructions.iter().all(|instruction| !matches!(
            instruction,
            Instruction::MakeClosure { .. } | Instruction::CallValue { .. }
        )));
        assert_eq!(
            Machine::new(&program).run_main().unwrap(),
            Value::Integer(42)
        );

        let named = crate::compile(
            "func main() -> Int {
                let offset = 2
                let multiply = [copy offset] (value: Int) -> { value * offset }
                multiply(21)
            }",
        )
        .unwrap();
        let named = compile(&named).unwrap();
        assert!(
            named.functions[&named.main.unwrap()]
                .instructions
                .iter()
                .any(|instruction| matches!(instruction, Instruction::CallClosure { .. }))
        );
        assert_eq!(Machine::new(&named).run_main().unwrap(), Value::Integer(42));

        let borrowed = crate::compile(
            "func main() -> Int {
                let count = 41
                ([ref count] () -> { count = count + 1 })()
                count
            }",
        )
        .unwrap();
        let borrowed = compile(&borrowed).unwrap();
        assert!(
            borrowed.functions[&borrowed.main.unwrap()]
                .instructions
                .iter()
                .any(|instruction| matches!(instruction, Instruction::CallClosure { .. }))
        );
        assert_eq!(
            Machine::new(&borrowed).run_main().unwrap(),
            Value::Integer(42)
        );
    }

    #[test]
    fn executes_control_flow_lists_records_and_variants() {
        let branching = crate::compile(
            "func choose(value: Int) -> Int {\n\
                 branch { value > 10 -> [value, 42][1] _ -> 0 }\n\
             }\n\
             func main() -> Int { choose(20) }",
        )
        .unwrap();
        assert_eq!(run(&branching).unwrap(), Value::Integer(42));

        let record = crate::compile(
            "type Pair = { left: Int, right: Int }\n\
             func main() -> Int {\n\
                 let pair = Pair { left: 20, right: 22 }\n\
                 pair.left + pair.right\n\
             }",
        )
        .unwrap();
        assert_eq!(run(&record).unwrap(), Value::Integer(42));

        let variant =
            crate::compile("enum Answer = Value(Int)\nfunc main() { Answer.Value(42) }").unwrap();
        assert!(matches!(
            run(&variant).unwrap(),
            Value::Variant { type_name, alternative, payload, .. }
                if type_name.as_ref() == "Answer" && alternative.as_ref() == "Value" && payload == [Value::Integer(42)]
        ));
    }

    #[test]
    fn executes_subject_branches_with_atomic_pattern_bindings() {
        let compilation = crate::compile(
            "enum Choice = Left(Int) | Right(Int)\n\
             func unwrap(value: Choice) -> Int {\n\
                 branch value {\n\
                     Choice.Left(number) -> number\n\
                     Choice.Right(42) -> 42\n\
                     Choice.Right(_) -> 0\n\
                 }\n\
             }\n\
             func main() -> Int { unwrap(Choice.Left(42)) }",
        )
        .unwrap();
        assert_eq!(run(&compilation).unwrap(), Value::Integer(42));
    }

    #[test]
    fn executes_closure_captures_and_dynamic_calls() {
        let compilation = crate::compile(
            "func multiplier(factor: Int) {\n\
                 func apply(value: Int) -> Int { factor * value }\n\
             }\n\
             func main() -> Int {\n\
                 let twice = multiplier(2)\n\
                 twice(21)\n\
             }",
        )
        .unwrap();
        assert_eq!(run(&compilation).unwrap(), Value::Integer(42));

        let partial = crate::compile(
            "func add(left: Int, right: Int) -> Int { left + right }\n\
             func main() -> Int { add(20, _)(22) }",
        )
        .unwrap();
        assert_eq!(run(&partial).unwrap(), Value::Integer(42));
    }

    #[test]
    fn executes_copy_move_and_reference_capture_modes() {
        let mutable = crate::compile(
            "func main() -> Int {\n\
                 let count = 40\n\
                 let increment = [ref count] () -> { count = count + 1 }\n\
                 increment()\n\
                 increment()\n\
                 count\n\
             }",
        )
        .unwrap();
        assert_eq!(run(&mutable).unwrap(), Value::Integer(42));

        let moved = crate::compile(
            "func main() -> Int {\n\
                 let text = \"forty-two\"\n\
                 let length = [move text] () -> text.length\n\
                 length()\n\
             }",
        )
        .unwrap();
        assert_eq!(run(&moved).unwrap(), Value::Integer(9));

        let copied = crate::compile(
            "func main() -> Int {\n\
                 let value = 42\n\
                 let reader = [copy value] () -> value\n\
                 reader()\n\
             }",
        )
        .unwrap();
        assert_eq!(run(&copied).unwrap(), Value::Integer(42));
    }

    #[test]
    fn executes_self_recursive_closures_and_deep_vm_frames() {
        let nested = crate::compile(
            "func main() -> Int {\n\
                 func count(value: Int) -> Int {\n\
                     branch { value == 0 -> 42 _ -> count(value - 1) }\n\
                 }\n\
                 count(100)\n\
             }",
        )
        .unwrap();
        assert_eq!(run(&nested).unwrap(), Value::Integer(42));

        let deep = crate::compile(
            "func count(value: Int) -> Int {\n\
                 branch { value == 0 -> 42 _ -> count(value - 1) }\n\
             }\n\
             func main() -> Int { count(25000) }",
        )
        .unwrap();
        assert_eq!(run(&deep).unwrap(), Value::Integer(42));
    }
}
