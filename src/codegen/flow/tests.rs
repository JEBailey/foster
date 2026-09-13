use super::*;
use crate::{codegen::ir, vm};

#[test]
fn reused_storage_keeps_distinct_value_types() {
    let compilation = crate::compile("func main() -> Int { 42 }").unwrap();
    let mut program = vm::compile(&compilation).unwrap();
    let id = program.metadata.main.unwrap();
    program.metadata.constants = vec![
        crate::codegen::metadata::Constant::Bool(true),
        crate::codegen::metadata::Constant::Integer(42),
    ];
    let body = program.functions.get_mut(&id).unwrap();
    body.registers = 1;
    body.instructions = vec![
        vm::Instruction::LoadConstant {
            destination: vm::Register(0),
            constant: 0,
        },
        vm::Instruction::Assert {
            condition: vm::Register(0),
            message: None,
        },
        vm::Instruction::Drop {
            register: vm::Register(0),
        },
        vm::Instruction::LoadConstant {
            destination: vm::Register(0),
            constant: 1,
        },
        vm::Instruction::Return {
            source: vm::Register(0),
        },
    ];
    body.instruction_spans = vec![0..0; body.instructions.len()];
    let shared = crate::codegen::vm::seal_program(program).unwrap();
    let function = &shared.functions()[&id];
    let facts = shared.facts(id);
    let definitions = function
        .blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .filter_map(|i| match &i.instruction {
            ir::Instruction::Portable(ir::PortableInstruction::LoadConstant {
                destination,
                constant,
            }) => Some((*destination, *constant)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(definitions.len(), 2);
    for (value, constant) in definitions {
        assert_eq!(function.values.hint(value.index()), Some(0));
        assert_eq!(
            facts.value_types(value),
            &[if constant == 0 {
                ExecutableType::Bool
            } else {
                ExecutableType::Integer
            }]
        );
    }
}

#[test]
fn branch_loop_alias_and_reference_evidence_agrees_with_construction() {
    // Sealing runs the differential oracle for every function in test builds.
    for source in [
        "func main() -> Int { let x = 21\nlet y = x\nx + y }",
        "func main() -> Int { let x = 0\nwhile x < 3 { x = x + 1 }\nx }",
        "enum Choice = First(Int) | Second\nfunc choose(x: Choice) -> Int { branch x { Choice.First(value) -> value\nChoice.Second -> 0 } }\nfunc main() -> Int { choose(Choice.First(42)) }",
        "func set[state: group Int](value: ref[state] Int, next: Int) -> Int [mut state] { value = next\nvalue + 1 }\nfunc main() -> Int { let values = [10, 20]\nlet selected = ref values[0]\nset(ref selected, 41) }",
    ] {
        let compilation = crate::compile(source).unwrap();
        vm::compile_shared(&compilation).unwrap();
    }
}

#[test]
fn unknown_availability_and_reachability_are_distinct() {
    let facts = FunctionFacts {
        points: vec![vec![
            PointFacts::Reachable(HashMap::from([
                (Value(0), Some(ExecutableType::Unknown)),
                (Value(1), None),
            ])),
            PointFacts::Unreachable,
        ]],
        values: vec![vec![ExecutableType::Unknown], vec![]],
    };
    let site = Site {
        block: Block(0),
        instruction: 0,
    };
    assert!(matches!(
        facts.at(site, Value(0)),
        ValueFact::Known(ExecutableType::Unknown)
    ));
    assert!(matches!(facts.at(site, Value(1)), ValueFact::Unavailable));
    assert!(matches!(facts.at(site, Value(2)), ValueFact::NotAnOperand));
    assert!(matches!(
        facts.at(
            Site {
                instruction: 1,
                ..site
            },
            Value(0)
        ),
        ValueFact::Unreachable
    ));
}

#[test]
fn parallel_edges_preserve_every_pattern_alias_and_binding() {
    use engine::{Body, Instruction as Op, Program, StoragePolicy};
    let schema = FunctionSchema {
        name: "pattern_aliases".into(),
        parameters: vec![crate::types::Parameter {
            ty: ExecutableType::Integer,
            mode: crate::ast::ParameterMode::Borrow,
        }],
        captures: vec![],
        result_type: ExecutableType::Unknown,
        returns_reference: false,
        intrinsic_stub: false,
    };
    let local = crate::hir::LocalId::from_raw(la_arena::RawIdx::from_u32(0));
    let body = Body {
        schema: &schema,
        value_count: 6,
        entry_values: vec![Value(0)],
        write_bindings: &HashMap::new(),
        storage_policy: StoragePolicy::ImmutableValues,
        instructions: vec![
            Op::MatchPattern {
                destination: Value(1),
                subject: Value(0),
                pattern: crate::hir::Pattern::Binding(local),
                bindings: vec![Value(4)],
            },
            Op::Edge {
                target: 2,
                arguments: vec![
                    (Value(1), Value(2)),
                    (Value(1), Value(3)),
                    (Value(4), Value(5)),
                ],
            },
            Op::JumpIfFalse {
                condition: Value(3),
                target: 4,
            },
            Op::Return { source: Value(5) },
            Op::Return { source: Value(0) },
        ],
    };
    let states = engine::analyze_function_flow(
        &Program {
            metadata: &Default::default(),
            functions: &HashMap::new(),
        },
        &body,
    )
    .unwrap();
    assert_eq!(states[2].as_ref().unwrap().bindings[5], None);
    assert_eq!(
        states[3].as_ref().unwrap().bindings[5],
        Some(ExecutableType::Unknown)
    );
    assert!(states[4].is_none(), "irrefutable binding has no false edge");
}

#[test]
fn edge_swaps_read_all_arguments_before_writing_parameters() {
    use engine::{Body, Instruction as Op, Program, StoragePolicy};
    let schema = FunctionSchema {
        name: "swap".into(),
        parameters: vec![ExecutableType::Bool, ExecutableType::Integer]
            .into_iter()
            .map(|ty| crate::types::Parameter {
                ty,
                mode: crate::ast::ParameterMode::Borrow,
            })
            .collect(),
        captures: vec![],
        result_type: ExecutableType::Integer,
        returns_reference: false,
        intrinsic_stub: false,
    };
    let body = Body {
        schema: &schema,
        value_count: 2,
        entry_values: vec![Value(0), Value(1)],
        write_bindings: &HashMap::new(),
        storage_policy: StoragePolicy::ImmutableValues,
        instructions: vec![
            Op::Edge {
                target: 1,
                arguments: vec![(Value(0), Value(1)), (Value(1), Value(0))],
            },
            Op::Return { source: Value(0) },
        ],
    };
    let states = engine::analyze_function_flow(
        &Program {
            metadata: &Default::default(),
            functions: &HashMap::new(),
        },
        &body,
    )
    .unwrap();
    assert_eq!(
        states[1].as_ref().unwrap().bindings,
        vec![Some(ExecutableType::Integer), Some(ExecutableType::Bool)]
    );
}
