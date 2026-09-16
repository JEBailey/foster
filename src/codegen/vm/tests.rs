use super::emission::{Emission, emit_copies};
use crate::codegen::ir::{self, Block, Type, Value};
use crate::codegen::metadata::{Constant, ProgramMetadata};
use crate::codegen::types::ExecutableType;
use crate::hir::FunctionId;
use crate::vm::{self, Register};
use std::collections::HashMap;

use super::*;
use crate::vm::{Machine, Program};
use la_arena::{Idx, RawIdx};

#[test]
fn program_sealing_restores_functions_after_an_error() {
    let id = Idx::from_raw(RawIdx::from_u32(0));
    let mut program = Program::default();
    let span = 0..0;
    program.functions.insert(
        id,
        vm::BytecodeFunction {
            name: "invalid".into(),
            intrinsic_stub: false,
            parameters: 0,
            parameter_types: vec![],
            parameter_modes: vec![],
            mutable_parameters: vec![],
            returns_reference: false,
            captures: 0,
            capture_types: vec![],
            result_type: ExecutableType::Unit,
            registers: 0,
            instructions: vec![vm::Instruction::Jump { target: 1 }],
            instruction_spans: vec![span],
        },
    );
    let original = program.clone();

    assert!(lower_program_through_shared_ir(&mut program).is_err());
    assert_eq!(program, original);
}

#[test]
fn program_consumers_reject_the_same_invalid_construction() {
    let compilation = crate::compile("func main() -> Int { 42 }").unwrap();
    let program = crate::vm::compile(&compilation).unwrap();
    for defect in ["entry", "spans", "types", "symbols"] {
        let mut invalid = program.clone();
        let main = invalid.metadata.main.unwrap();
        match defect {
            "entry" => invalid.metadata.main_arguments = true,
            "spans" => invalid
                .functions
                .get_mut(&main)
                .unwrap()
                .instruction_spans
                .clear(),
            "types" => invalid.functions.get_mut(&main).unwrap().result_type = ExecutableType::Bool,
            "symbols" => invalid.metadata.symbols.version = u16::MAX,
            _ => unreachable!(),
        }
        let snapshot = invalid.clone();
        let retained_error = seal_program(invalid.clone()).unwrap_err();
        let lowered_error = lower_program_through_shared_ir(&mut invalid).unwrap_err();
        assert_eq!(retained_error, lowered_error, "{defect}");
        assert_eq!(invalid, snapshot, "{defect}");
        let optimizer_error = vm::optimize(&mut invalid).unwrap_err();
        assert!(
            optimizer_error
                .message
                .contains(&retained_error.to_string())
        );
        assert_eq!(invalid, snapshot, "optimizer changed input with {defect}");
    }
}

#[test]
fn vm_lowering_rolls_back_constants_after_successful_sealing() {
    let id = Idx::from_raw(RawIdx::from_u32(0));
    let mut program = Program::default();
    // Repeated cleanup creates an empty SSA seed. Lowering materializes that seed as
    // Unit, which is absent from this full constant pool, so its append must fail.
    program.metadata.constants = vec![Constant::Integer(42); usize::from(u16::MAX) + 1];
    program.metadata.main = Some(id);
    program.functions.insert(
        id,
        vm::BytecodeFunction {
            name: "full_constant_pool".into(),
            intrinsic_stub: false,
            parameters: 0,
            parameter_types: vec![],
            parameter_modes: vec![],
            mutable_parameters: vec![],
            returns_reference: false,
            captures: 0,
            capture_types: vec![],
            result_type: ExecutableType::Integer,
            registers: 2,
            instructions: vec![
                vm::Instruction::LoadConstant {
                    destination: Register(0),
                    constant: 0,
                },
                vm::Instruction::Drop {
                    register: Register(0),
                },
                vm::Instruction::Drop {
                    register: Register(0),
                },
                vm::Instruction::LoadConstant {
                    destination: Register(1),
                    constant: 0,
                },
                vm::Instruction::Return {
                    source: Register(1),
                },
            ],
            instruction_spans: vec![0..1, 1..2, 2..3, 3..4, 4..5],
        },
    );
    seal_program(program.clone()).unwrap();
    let before = vm::encode_program(&program).unwrap();
    assert_eq!(
        lower_program_through_shared_ir(&mut program)
            .unwrap_err()
            .to_string(),
        "too many VM constants"
    );
    // Check the entire executable, including bodies, source spans, and metadata.
    assert_eq!(vm::encode_program(&program).unwrap(), before);
    let error = vm::optimize(&mut program).unwrap_err();
    assert_eq!(
        error.message,
        "shared VM lowering failed: too many VM constants"
    );
    assert_eq!(vm::encode_program(&program).unwrap(), before);
}

#[test]
fn sealed_program_rejects_invalid_metadata_and_keeps_signatures_consistent() {
    let compilation = crate::compile("func main() -> Int { 42 }").unwrap();
    let program = crate::vm::compile(&compilation).unwrap();
    let sealed = seal_program(program.clone()).unwrap();
    for (id, function) in sealed.functions() {
        assert_eq!(sealed.signatures()[id], function.signature);
    }
    assert_eq!(sealed.metadata(), &program.metadata);
    let restored = lower_shared_program(sealed).unwrap();
    assert_eq!(
        Machine::new(&restored).run_main().unwrap(),
        Machine::new(&program).run_main().unwrap()
    );
    let mut invalid = program;
    invalid
        .functions
        .get_mut(&invalid.metadata.main.unwrap())
        .unwrap()
        .instruction_spans
        .clear();
    assert!(
        seal_program(invalid)
            .unwrap_err()
            .to_string()
            .contains("span")
    );
}

#[test]
fn instruction_spans_follow_reordering_through_bytecode_lowering() {
    let mut instructions = vec![
        ir::Instruction::Constant {
            destination: Value(0),
            value: ir::Constant::Integer(20),
        }
        .with_span(10..12),
        ir::Instruction::Constant {
            destination: Value(1),
            value: ir::Constant::Integer(22),
        }
        .with_span(20..22),
    ];
    instructions.swap(0, 1);
    let function = ir::Function {
        name: "reordered".into(),
        signature: ir::Signature {
            parameters: vec![],
            result: Type::Int,
        },
        parameters: vec![],
        captures: vec![],
        entry_seeds: vec![],
        entry: Block(0),
        entry_arguments: vec![],
        values: vec![Type::Int; 2].into_iter().collect(),
        blocks: vec![ir::BlockData {
            parameters: vec![],
            instructions,
            terminator: ir::Terminator::Return(Value(0)),
            terminator_span: 30..32,
        }],
    };
    let mut constants = vec![];
    let lowered = lower_function(
        &function,
        &HashMap::new(),
        &mut constants,
        FunctionMetadata::default(),
    )
    .unwrap();
    for (instruction, span) in lowered.instructions.iter().zip(&lowered.instruction_spans) {
        if let vm::Instruction::LoadConstant { constant, .. } = instruction {
            match constants[usize::from(*constant)] {
                Constant::Integer(20) => assert_eq!(*span, 10..12),
                Constant::Integer(22) => assert_eq!(*span, 20..22),
                _ => panic!("unexpected constant"),
            }
        }
    }
}

#[test]
fn branch_edges_receive_distinct_parallel_copies() {
    let function = ir::Function {
        name: "choose".into(),
        signature: ir::Signature {
            parameters: vec![Type::Bool, Type::Int, Type::Int],
            result: Type::Int,
        },
        parameters: vec![Value(0), Value(1), Value(2)],
        captures: vec![],
        entry_seeds: vec![],
        entry: Block(0),
        entry_arguments: vec![Value(0), Value(1), Value(2)],
        values: vec![
            Type::Bool,
            Type::Int,
            Type::Int,
            Type::Bool,
            Type::Int,
            Type::Int,
            Type::Int,
        ]
        .into_iter()
        .collect(),
        blocks: vec![
            ir::BlockData {
                parameters: vec![Value(3), Value(4), Value(5)],
                instructions: ir::SpannedInstruction::from_parts(vec![], vec![]),
                terminator: ir::Terminator::Branch {
                    condition: Value(3),
                    then_target: Block(1),
                    then_arguments: vec![Value(4)],
                    else_target: Block(1),
                    else_arguments: vec![Value(5)],
                },
                terminator_span: 0..0,
            },
            ir::BlockData {
                parameters: vec![Value(6)],
                instructions: ir::SpannedInstruction::from_parts(vec![], vec![]),
                terminator: ir::Terminator::Return(Value(6)),
                terminator_span: 0..0,
            },
        ],
    };
    let lowered = lower_function(
        &function,
        &HashMap::new(),
        &mut Vec::new(),
        FunctionMetadata::default(),
    )
    .unwrap();
    assert!(
        lowered
            .instructions
            .iter()
            .any(|instruction| matches!(instruction, vm::Instruction::JumpIfFalse { .. }))
    );
    assert_eq!(lowered.parameters, 3);
    assert!(lowered.registers >= 6);
}

#[test]
fn parallel_copy_cycles_use_one_temporary() {
    let registers = vec![Some(Register(0)), Some(Register(1))];
    let mut emissions = Vec::new();
    let mut next = 2;
    emit_copies(
        &mut emissions,
        &mut next,
        &registers,
        &[Value(0), Value(1)],
        &[Value(1), Value(0)],
        0..0,
    )
    .unwrap();
    assert_eq!(next, 3);
    let moves = emissions
        .into_iter()
        .map(|emission| match emission {
            Emission::Instruction(
                vm::Instruction::Move {
                    destination,
                    source,
                },
                _,
            ) => (destination, source),
            _ => panic!("parallel copies must contain only moves"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        moves,
        vec![
            (Register(2), Register(0)),
            (Register(0), Register(1)),
            (Register(1), Register(2)),
        ]
    );
}

#[test]
fn de_ssa_bytecode_verifies_and_executes() {
    let function = ir::Function {
        name: "main".into(),
        signature: ir::Signature {
            parameters: vec![],
            result: Type::Int,
        },
        parameters: vec![],
        captures: vec![],
        entry_seeds: vec![],
        entry: Block(0),
        entry_arguments: vec![],
        values: vec![Type::Bool, Type::Int, Type::Int, Type::Int]
            .into_iter()
            .collect(),
        blocks: vec![
            ir::BlockData {
                parameters: vec![],
                instructions: ir::SpannedInstruction::from_parts(
                    vec![
                        ir::Instruction::Constant {
                            destination: Value(0),
                            value: ir::Constant::Bool(true),
                        },
                        ir::Instruction::Constant {
                            destination: Value(1),
                            value: ir::Constant::Integer(41),
                        },
                        ir::Instruction::Constant {
                            destination: Value(2),
                            value: ir::Constant::Integer(99),
                        },
                    ],
                    vec![0..0, 0..0, 0..0],
                ),
                terminator: ir::Terminator::Branch {
                    condition: Value(0),
                    then_target: Block(1),
                    then_arguments: vec![Value(1)],
                    else_target: Block(1),
                    else_arguments: vec![Value(2)],
                },
                terminator_span: 0..0,
            },
            ir::BlockData {
                parameters: vec![Value(3)],
                instructions: ir::SpannedInstruction::from_parts(vec![], vec![]),
                terminator: ir::Terminator::Return(Value(3)),
                terminator_span: 0..0,
            },
        ],
    };
    let mut constants = Vec::new();
    let lowered = lower_function(
        &function,
        &HashMap::new(),
        &mut constants,
        FunctionMetadata::default(),
    )
    .unwrap();
    let main: FunctionId = Idx::from_raw(RawIdx::from_u32(0));
    let mut program = Program {
        metadata: ProgramMetadata {
            constants,
            main: Some(main),
            ..Default::default()
        },
        ..Program::default()
    };
    program.functions.insert(main, lowered);
    crate::vm::verify(&program).unwrap();
    assert_eq!(
        Machine::new(&program).run_main().unwrap(),
        vm::Value::Integer(41)
    );
}
