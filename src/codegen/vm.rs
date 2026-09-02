//! Sealing from VM construction form into shared SSA and de-SSA lowering to portable bytecode.

use std::collections::HashMap;
use std::fmt;
use std::ops::Range;

use crate::codegen::ir::{self, Block, Type, Value};
use crate::hir::FunctionId;
use crate::vm::{self, Register, VerificationType};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LowerError(String);

impl fmt::Display for LowerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for LowerError {}

/// Make shared SSA the mandatory backend boundary for every executable VM function.
pub fn lower_program_through_shared_ir(program: &mut vm::Program) -> Result<(), LowerError> {
    // Own the table during the transaction instead of cloning every instruction payload. Any
    // sealing/lowering failure restores the original functions before returning.
    let originals = std::mem::take(&mut program.functions);
    let result_types = originals
        .iter()
        .map(|(id, function)| (*id, shared_type(&function.result_type)))
        .collect::<HashMap<_, _>>();
    let mut sealed = HashMap::with_capacity(originals.len());
    for (id, function) in &originals {
        if function.intrinsic_stub {
            continue;
        }
        match seal_function_with_types(&program.constants, &result_types, function) {
            Ok(function) => {
                sealed.insert(*id, function);
            }
            Err(error) => {
                program.functions = originals;
                return Err(error);
            }
        }
    }
    let signatures = sealed
        .iter()
        .map(|(id, function)| (*id, function.signature.clone()))
        .collect::<HashMap<_, _>>();
    let mut lowered = HashMap::with_capacity(originals.len());
    for (id, function) in sealed {
        let original = &originals[&id];
        let metadata = FunctionMetadata::from_bytecode(original);
        match lower_function(&function, &signatures, &mut program.constants, metadata) {
            Ok(function) => {
                lowered.insert(id, function);
            }
            Err(error) => {
                program.functions = originals;
                return Err(error);
            }
        }
    }
    for (id, function) in originals {
        if function.intrinsic_stub {
            lowered.insert(id, function);
        }
    }
    program.functions = lowered;
    Ok(())
}

/// Non-executable metadata which is intentionally not part of SSA.
#[derive(Debug, Clone, Default)]
pub struct FunctionMetadata {
    pub intrinsic_stub: bool,
    pub parameter_modes: Vec<crate::ast::ParameterMode>,
    pub mutable_parameters: Vec<bool>,
    pub returns_reference: bool,
    pub parameter_types: Vec<VerificationType>,
    pub capture_types: Vec<VerificationType>,
    pub result_type: Option<VerificationType>,
}

impl FunctionMetadata {
    fn from_bytecode(function: &vm::BytecodeFunction) -> Self {
        Self {
            intrinsic_stub: function.intrinsic_stub,
            parameter_modes: function.parameter_modes.clone(),
            mutable_parameters: function.mutable_parameters.clone(),
            returns_reference: function.returns_reference,
            parameter_types: function.parameter_types.clone(),
            capture_types: function.capture_types.clone(),
            result_type: Some(function.result_type.clone()),
        }
    }
}

/// Seal the HIR compiler's unstructured virtual-register construction into block-argument SSA
/// while retaining observable VM storage homes.
pub fn seal_function(
    program: &vm::Program,
    function: &vm::BytecodeFunction,
) -> Result<ir::Function, LowerError> {
    let result_types = program
        .functions
        .iter()
        .map(|(id, function)| (*id, shared_type(&function.result_type)))
        .collect::<HashMap<_, _>>();
    seal_function_with_types(&program.constants, &result_types, function)
}

fn seal_function_with_types(
    constants: &[vm::Constant],
    result_types: &HashMap<FunctionId, Type>,
    function: &vm::BytecodeFunction,
) -> Result<ir::Function, LowerError> {
    if function.instructions.is_empty() {
        return Err(LowerError(
            "cannot seal an empty executable function into shared SSA".into(),
        ));
    }
    let hints = register_type_hints(constants, result_types, function);
    let leaders = block_leaders(function)?;
    let mut leader_blocks = vec![None; function.instructions.len()];
    for (index, leader) in leaders.iter().copied().enumerate() {
        leader_blocks[leader] = Some(Block(index as u32));
    }
    let liveness = crate::vm::optimizer::analysis::liveness(function);
    let mut value_types = Vec::new();
    let mut storage_hints = Vec::new();
    let mut externals = Vec::new();
    for (register, ty) in hints
        .iter()
        .copied()
        .enumerate()
        .take(usize::from(function.captures + function.parameters))
    {
        externals.push(allocate_lifted_value(
            &mut value_types,
            &mut storage_hints,
            ty,
            Register(register as u16),
        ));
    }
    let capture_count = usize::from(function.captures);
    let captures = externals[..capture_count].to_vec();
    let parameters = externals[capture_count..].to_vec();
    let mut parameter_registers: Vec<Vec<Register>> = Vec::new();
    let mut block_parameters: Vec<Vec<Value>> = Vec::new();
    for leader in &leaders {
        let mut registers = liveness.live_in[*leader]
            .iter()
            .copied()
            .collect::<Vec<_>>();
        registers.sort_unstable_by_key(|register| register.0);
        let values = registers
            .iter()
            .map(|register| {
                allocate_lifted_value(
                    &mut value_types,
                    &mut storage_hints,
                    hints[usize::from(register.0)],
                    *register,
                )
            })
            .collect::<Vec<_>>();
        parameter_registers.push(registers);
        block_parameters.push(values);
    }
    let mut entry_seeds = Vec::new();
    let entry_arguments = parameter_registers[0]
        .iter()
        .map(|register| {
            externals
                .get(usize::from(register.0))
                .copied()
                .unwrap_or_else(|| {
                    let seed = allocate_lifted_value(
                        &mut value_types,
                        &mut storage_hints,
                        hints[usize::from(register.0)],
                        *register,
                    );
                    entry_seeds.push(seed);
                    seed
                })
        })
        .collect::<Vec<_>>();

    let mut blocks = Vec::new();
    for (block_index, start) in leaders.iter().copied().enumerate() {
        let end = leaders
            .get(block_index + 1)
            .copied()
            .unwrap_or(function.instructions.len());
        let mut state = vec![None; usize::from(function.registers)];
        for (register, value) in parameter_registers[block_index]
            .iter()
            .zip(&block_parameters[block_index])
        {
            state[usize::from(register.0)] = Some(*value);
        }
        let mut instructions = Vec::new();
        let mut instruction_spans = Vec::new();
        let mut terminator = None;
        let mut terminator_span = Range::default();
        for source_index in start..end {
            let operation = &function.instructions[source_index];
            let source_span = function
                .instruction_spans
                .get(source_index)
                .cloned()
                .unwrap_or_default();
            match operation {
                vm::Instruction::Jump { target } => {
                    terminator_span = source_span;
                    let target =
                        leader_blocks[*target].expect("validated jump targets are block leaders");
                    terminator = Some(ir::Terminator::Jump {
                        target,
                        arguments: lifted_edge_arguments(
                            target,
                            &parameter_registers,
                            &state,
                            function,
                        )?,
                    });
                    break;
                }
                vm::Instruction::JumpIfFalse { condition, target } => {
                    terminator_span = source_span;
                    let then_target = leader_blocks[source_index + 1].ok_or_else(|| {
                        LowerError(format!(
                            "conditional jump in `{}` has no fallthrough block",
                            function.name
                        ))
                    })?;
                    let else_target =
                        leader_blocks[*target].expect("validated jump targets are block leaders");
                    terminator = Some(ir::Terminator::Branch {
                        condition: lifted_register(&state, *condition, function)?,
                        then_target,
                        then_arguments: lifted_edge_arguments(
                            then_target,
                            &parameter_registers,
                            &state,
                            function,
                        )?,
                        else_target,
                        else_arguments: lifted_edge_arguments(
                            else_target,
                            &parameter_registers,
                            &state,
                            function,
                        )?,
                    });
                    break;
                }
                vm::Instruction::Return { source } => {
                    terminator_span = source_span;
                    terminator = Some(ir::Terminator::Return(lifted_register(
                        &state, *source, function,
                    )?));
                    break;
                }
                vm::Instruction::Drop { register } => {
                    let value = lifted_register(&state, *register, function)?;
                    instructions.push(ir::Instruction::Portable(ir::PortableInstruction::Drop {
                        value,
                    }));
                    instruction_spans.push(source_span);
                    state[usize::from(register.0)] = None;
                }
                operation => {
                    for register in crate::vm::optimizer::analysis::uses(operation) {
                        lifted_register(&state, register, function)?;
                    }
                    let definitions = crate::vm::optimizer::analysis::definitions(operation);
                    let mut destinations = Vec::with_capacity(definitions.len());
                    for register in definitions {
                        let value = allocate_lifted_value(
                            &mut value_types,
                            &mut storage_hints,
                            hints[usize::from(register.0)],
                            register,
                        );
                        destinations.push((register, value));
                    }
                    instructions.push(ir::Instruction::Portable(portable_instruction(
                        operation,
                        &state,
                        &destinations,
                    )));
                    instruction_spans.push(source_span);
                    for (register, value) in destinations {
                        state[usize::from(register.0)] = Some(value);
                    }
                }
            }
        }
        let terminator = match terminator {
            Some(terminator) => terminator,
            None if block_index + 1 < leaders.len() => {
                let target = Block((block_index + 1) as u32);
                ir::Terminator::Jump {
                    target,
                    arguments: lifted_edge_arguments(
                        target,
                        &parameter_registers,
                        &state,
                        function,
                    )?,
                }
            }
            None => {
                return Err(LowerError(format!(
                    "reachable block in `{}` falls off the end",
                    function.name
                )));
            }
        };
        blocks.push(ir::BlockData {
            parameters: block_parameters[block_index].clone(),
            instructions,
            instruction_spans,
            terminator,
            terminator_span,
        });
    }
    Ok(ir::Function {
        name: function.name.clone(),
        signature: ir::Signature {
            parameters: (capture_count..capture_count + usize::from(function.parameters))
                .map(|register| hints[register])
                .collect(),
            result: shared_type(&function.result_type),
        },
        parameters,
        captures,
        capture_types: (0..capture_count).map(|register| hints[register]).collect(),
        entry_seeds,
        entry: Block(0),
        entry_arguments,
        value_types,
        storage_hints,
        blocks,
    })
}

fn allocate_lifted_value(
    types: &mut Vec<Type>,
    homes: &mut Vec<Option<u16>>,
    ty: Type,
    register: Register,
) -> Value {
    let value = Value(types.len() as u32);
    types.push(ty);
    homes.push(Some(register.0));
    value
}

fn lifted_register(
    state: &[Option<Value>],
    register: Register,
    function: &vm::BytecodeFunction,
) -> Result<Value, LowerError> {
    state[usize::from(register.0)].ok_or_else(|| {
        LowerError(format!(
            "shared SSA lift reads uninitialized r{} in `{}`",
            register.0, function.name
        ))
    })
}

fn lifted_edge_arguments(
    target: Block,
    parameter_registers: &[Vec<Register>],
    state: &[Option<Value>],
    function: &vm::BytecodeFunction,
) -> Result<Vec<Value>, LowerError> {
    parameter_registers[target.0 as usize]
        .iter()
        .map(|register| lifted_register(state, *register, function))
        .collect()
}

fn block_leaders(function: &vm::BytecodeFunction) -> Result<Vec<usize>, LowerError> {
    let mut leaders = vec![false; function.instructions.len()];
    leaders[0] = true;
    for (index, instruction) in function.instructions.iter().enumerate() {
        match instruction {
            vm::Instruction::Jump { target } | vm::Instruction::JumpIfFalse { target, .. } => {
                if *target >= function.instructions.len() {
                    return Err(LowerError(format!("invalid jump target {target}")));
                }
                leaders[*target] = true;
                if index + 1 < function.instructions.len() {
                    leaders[index + 1] = true;
                }
            }
            vm::Instruction::Return { .. } if index + 1 < function.instructions.len() => {
                leaders[index + 1] = true;
            }
            _ => {}
        }
    }
    Ok(leaders
        .into_iter()
        .enumerate()
        .filter_map(|(index, leader)| leader.then_some(index))
        .collect())
}

fn shared_type(ty: &VerificationType) -> Type {
    match ty {
        VerificationType::Unit => Type::Unit,
        VerificationType::Bool => Type::Bool,
        VerificationType::Integer => Type::Int,
        VerificationType::Float => Type::Float,
        VerificationType::CodePoint => Type::CodePoint,
        VerificationType::Byte => Type::Byte,
        _ => Type::Opaque,
    }
}

fn register_type_hints(
    constants: &[vm::Constant],
    result_types: &HashMap<FunctionId, Type>,
    function: &vm::BytecodeFunction,
) -> Vec<Type> {
    let mut hints = vec![Type::Opaque; usize::from(function.registers)];
    for (index, ty) in function
        .capture_types
        .iter()
        .chain(&function.parameter_types)
        .enumerate()
    {
        hints[index] = shared_type(ty);
    }
    for instruction in &function.instructions {
        let (destination, ty) = match instruction {
            vm::Instruction::LoadConstant {
                destination,
                constant,
            } => (
                Some(*destination),
                match &constants[usize::from(*constant)] {
                    vm::Constant::Unit => Type::Unit,
                    vm::Constant::Bool(_) => Type::Bool,
                    vm::Constant::Integer(_) => Type::Int,
                    vm::Constant::Float(_) => Type::Float,
                    vm::Constant::CodePoint(_) => Type::CodePoint,
                    vm::Constant::String(_) => Type::String,
                    vm::Constant::Symbol(_) => Type::Opaque,
                },
            ),
            vm::Instruction::Unary {
                destination,
                operator,
                operand,
            } => (
                Some(*destination),
                match operator {
                    crate::ast::UnaryOp::Not => Type::Bool,
                    crate::ast::UnaryOp::BitNot => Type::Byte,
                    crate::ast::UnaryOp::Negate => hints[usize::from(operand.0)],
                },
            ),
            vm::Instruction::Binary {
                destination,
                operator,
                left,
                ..
            } => (
                Some(*destination),
                if matches!(
                    operator,
                    crate::ast::BinaryOp::Equal
                        | crate::ast::BinaryOp::NotEqual
                        | crate::ast::BinaryOp::Less
                        | crate::ast::BinaryOp::LessEqual
                        | crate::ast::BinaryOp::Greater
                        | crate::ast::BinaryOp::GreaterEqual
                ) {
                    Type::Bool
                } else {
                    hints[usize::from(left.0)]
                },
            ),
            vm::Instruction::Move {
                destination,
                source,
            } => (Some(*destination), hints[usize::from(source.0)]),
            vm::Instruction::Contains { destination, .. }
            | vm::Instruction::MatchPattern { destination, .. } => (Some(*destination), Type::Bool),
            vm::Instruction::Call {
                destination,
                function,
                ..
            }
            | vm::Instruction::CallMethod {
                destination,
                function,
                ..
            }
            | vm::Instruction::CallClosure {
                destination,
                function,
                ..
            } => (
                Some(*destination),
                result_types.get(function).copied().unwrap_or(Type::Opaque),
            ),
            _ => (None, Type::Opaque),
        };
        if let Some(destination) = destination {
            hints[usize::from(destination.0)] = ty;
        }
        match instruction {
            vm::Instruction::JumpIfFalse { condition, .. }
            | vm::Instruction::Assert {
                condition,
                message: _,
            } => hints[usize::from(condition.0)] = Type::Bool,
            vm::Instruction::Return { source } => {
                hints[usize::from(source.0)] = shared_type(&function.result_type);
            }
            _ => {}
        }
    }
    hints
}

fn portable_instruction(
    instruction: &vm::Instruction,
    sources: &[Option<Value>],
    destinations: &[(Register, Value)],
) -> ir::PortableInstruction {
    let source = |register: &Register| {
        sources[usize::from(register.0)].expect("instruction operands were validated above")
    };
    let destination = |register: &Register| {
        destinations
            .iter()
            .find_map(|(candidate, value)| (candidate == register).then_some(*value))
            .expect("instruction destinations were allocated above")
    };
    match instruction {
        vm::Instruction::LoadConstant {
            destination: output,
            constant,
        } => ir::PortableInstruction::LoadConstant {
            destination: destination(output),
            constant: *constant,
        },
        vm::Instruction::Move {
            destination: output,
            source: input,
        } => ir::PortableInstruction::Move {
            destination: destination(output),
            source: source(input),
        },
        vm::Instruction::Unary {
            destination: output,
            operator,
            operand,
        } => ir::PortableInstruction::Unary {
            destination: destination(output),
            operator: *operator,
            operand: source(operand),
        },
        vm::Instruction::Binary {
            destination: output,
            operator,
            left,
            right,
        } => ir::PortableInstruction::Binary {
            destination: destination(output),
            operator: *operator,
            left: source(left),
            right: source(right),
        },
        vm::Instruction::MakeList {
            destination: output,
            elements,
        } => ir::PortableInstruction::MakeList {
            destination: destination(output),
            elements: elements.iter().map(source).collect(),
        },
        vm::Instruction::Index {
            destination: output,
            object,
            index,
        } => ir::PortableInstruction::Index {
            destination: destination(output),
            object: source(object),
            index: source(index),
        },
        vm::Instruction::MakeRecord {
            destination: output,
            record,
            fields,
        } => ir::PortableInstruction::MakeRecord {
            destination: destination(output),
            record: *record,
            fields: fields
                .iter()
                .map(|(name, register)| (name.clone(), source(register)))
                .collect(),
        },
        vm::Instruction::MakeVariant {
            destination: output,
            variant,
            payload,
        } => ir::PortableInstruction::MakeVariant {
            destination: destination(output),
            variant: *variant,
            payload: payload.iter().map(source).collect(),
        },
        vm::Instruction::LoadField {
            destination: output,
            object,
            field,
            by_reference,
        } => ir::PortableInstruction::LoadField {
            destination: destination(output),
            object: source(object),
            field: field.clone(),
            by_reference: *by_reference,
        },
        vm::Instruction::StoreField {
            object,
            field,
            source: input,
        } => ir::PortableInstruction::StoreField {
            object: source(object),
            field: field.clone(),
            source: source(input),
        },
        vm::Instruction::StoreIndex {
            object,
            index,
            source: input,
        } => ir::PortableInstruction::StoreIndex {
            object: source(object),
            index: source(index),
            source: source(input),
        },
        vm::Instruction::MakeReference {
            destination: output,
            object,
            index,
        } => ir::PortableInstruction::MakeReference {
            destination: destination(output),
            object: source(object),
            index: source(index),
        },
        vm::Instruction::MakeWholeReference {
            destination: output,
            object,
        } => ir::PortableInstruction::MakeWholeReference {
            destination: destination(output),
            object: source(object),
        },
        vm::Instruction::MakeFieldReference {
            destination: output,
            object,
            field,
        } => ir::PortableInstruction::MakeFieldReference {
            destination: destination(output),
            object: source(object),
            field: field.clone(),
        },
        vm::Instruction::MoveOut {
            destination: output,
            source: input,
        } => ir::PortableInstruction::MoveOut {
            destination: destination(output),
            source: source(input),
        },
        vm::Instruction::Push {
            destination: output,
            object,
            value,
        } => ir::PortableInstruction::Push {
            destination: destination(output),
            object: source(object),
            value: source(value),
        },
        vm::Instruction::Append {
            destination: output,
            object,
            value,
        } => ir::PortableInstruction::Append {
            destination: destination(output),
            object: source(object),
            value: source(value),
        },
        vm::Instruction::Contains {
            destination: output,
            value,
            candidates,
        } => ir::PortableInstruction::Contains {
            destination: destination(output),
            value: source(value),
            candidates: candidates.iter().map(source).collect(),
        },
        vm::Instruction::Builtin {
            destination: output,
            builtin,
            arguments,
        } => ir::PortableInstruction::Builtin {
            destination: destination(output),
            builtin: *builtin,
            arguments: arguments.iter().map(source).collect(),
        },
        vm::Instruction::SpawnRemote {
            destination: output,
            value,
        } => ir::PortableInstruction::SpawnRemote {
            destination: destination(output),
            value: source(value),
        },
        vm::Instruction::SpawnRemoteBorrow {
            destination: output,
            source: input,
        } => ir::PortableInstruction::SpawnRemoteBorrow {
            destination: destination(output),
            source: source(input),
        },
        vm::Instruction::RemoteCall {
            destination: output,
            remote,
            function,
            arguments,
        } => ir::PortableInstruction::RemoteCall {
            destination: destination(output),
            remote: source(remote),
            function: *function,
            arguments: arguments
                .iter()
                .map(|(mode, register)| (*mode, source(register)))
                .collect(),
        },
        vm::Instruction::Await {
            destination: output,
            future,
        } => ir::PortableInstruction::Await {
            destination: destination(output),
            future: source(future),
        },
        vm::Instruction::MatchPattern {
            destination: output,
            subject,
            pattern,
            bindings,
        } => ir::PortableInstruction::MatchPattern {
            destination: destination(output),
            subject: source(subject),
            pattern: pattern.clone(),
            bindings: bindings.iter().map(destination).collect(),
        },
        vm::Instruction::Assert { condition, message } => ir::PortableInstruction::Assert {
            condition: source(condition),
            message: message.as_ref().map(source),
        },
        vm::Instruction::Call {
            destination: output,
            function,
            arguments,
        } => ir::PortableInstruction::Call {
            destination: destination(output),
            function: *function,
            arguments: arguments.iter().map(source).collect(),
        },
        vm::Instruction::CallMethod {
            destination: output,
            receiver,
            function,
            arguments,
        } => ir::PortableInstruction::CallMethod {
            destination: destination(output),
            receiver: source(receiver),
            function: *function,
            arguments: arguments.iter().map(source).collect(),
        },
        vm::Instruction::CallContractMethod {
            destination: output,
            receiver,
            slot,
            name,
            arguments,
        } => ir::PortableInstruction::CallContractMethod {
            destination: destination(output),
            receiver: source(receiver),
            slot: *slot,
            name: name.clone(),
            arguments: arguments.iter().map(source).collect(),
        },
        vm::Instruction::MakeClosure {
            destination: output,
            function,
            captures,
        } => ir::PortableInstruction::MakeClosure {
            destination: destination(output),
            function: *function,
            captures: captures
                .iter()
                .map(|(mode, register)| (*mode, source(register)))
                .collect(),
        },
        vm::Instruction::CallValue {
            destination: output,
            callee,
            arguments,
        } => ir::PortableInstruction::CallValue {
            destination: destination(output),
            callee: source(callee),
            arguments: arguments.iter().map(source).collect(),
        },
        vm::Instruction::CallClosure {
            destination: output,
            function,
            captures,
            arguments,
        } => ir::PortableInstruction::CallClosure {
            destination: destination(output),
            function: *function,
            captures: captures
                .iter()
                .map(|(mode, register)| (*mode, source(register)))
                .collect(),
            arguments: arguments.iter().map(source).collect(),
        },
        vm::Instruction::Drop { .. }
        | vm::Instruction::Jump { .. }
        | vm::Instruction::JumpIfFalse { .. }
        | vm::Instruction::Return { .. } => unreachable!("handled while sealing control flow"),
    }
}

enum Emission {
    Instruction(vm::Instruction, Range<usize>),
    Jump(Block, Range<usize>),
    JumpIfFalse {
        condition: Register,
        target: Block,
        span: Range<usize>,
    },
}

/// Assign every SSA definition a register and materialize block arguments as parallel edge copies.
///
/// Critical conditional edges are split during linearization.  This keeps block parameters out of
/// the bytecode format without making frontends reason about mutable registers.
pub fn lower_function(
    function: &ir::Function,
    signatures: &HashMap<FunctionId, ir::Signature>,
    constants: &mut Vec<vm::Constant>,
    metadata: FunctionMetadata,
) -> Result<vm::BytecodeFunction, LowerError> {
    function
        .verify(signatures)
        .map_err(|error| LowerError(format!("invalid shared IR: {error}")))?;

    let mut registers = function
        .storage_hints
        .iter()
        .map(|home| home.map(Register))
        .collect::<Vec<_>>();
    let mut next = function
        .storage_hints
        .iter()
        .flatten()
        .copied()
        .max()
        .map_or(0, |register| register.saturating_add(1));
    for parameter in function.captures.iter().chain(&function.parameters) {
        assign(&mut registers, *parameter, &mut next)?;
    }
    for index in 0..function.value_types.len() {
        assign(&mut registers, Value(index as u32), &mut next)?;
    }
    let mut emissions = Vec::new();
    let mut labels = vec![None; function.blocks.len()];

    emit_copies(
        &mut emissions,
        &mut next,
        &registers,
        &function.blocks[function.entry.0 as usize].parameters,
        &function.entry_arguments,
        Range::default(),
    )?;
    if !emissions.is_empty() || function.entry != Block(0) {
        emissions.push(Emission::Jump(function.entry, Range::default()));
    }

    for (block_index, block) in function.blocks.iter().enumerate() {
        labels[block_index] = Some(emissions.len());
        for (instruction, span) in block.instructions.iter().zip(&block.instruction_spans) {
            lower_instruction(
                instruction,
                &registers,
                constants,
                &mut emissions,
                span.clone(),
            )?;
        }
        match &block.terminator {
            ir::Terminator::Jump { target, arguments } => {
                emit_copies(
                    &mut emissions,
                    &mut next,
                    &registers,
                    &function.blocks[target.0 as usize].parameters,
                    arguments,
                    block.terminator_span.clone(),
                )?;
                emissions.push(Emission::Jump(*target, block.terminator_span.clone()));
            }
            ir::Terminator::Branch {
                condition,
                then_target,
                then_arguments,
                else_target,
                else_arguments,
            } => {
                if *then_target == Block((block_index + 1) as u32)
                    && !copies_needed(
                        &registers,
                        &function.blocks[then_target.0 as usize].parameters,
                        then_arguments,
                    )
                    && !copies_needed(
                        &registers,
                        &function.blocks[else_target.0 as usize].parameters,
                        else_arguments,
                    )
                {
                    emissions.push(Emission::JumpIfFalse {
                        condition: reg(&registers, *condition),
                        target: *else_target,
                        span: block.terminator_span.clone(),
                    });
                    continue;
                }
                // The false edge gets a private label after the true-edge copies.
                let false_label = Block(labels.len() as u32);
                labels.push(None);
                emissions.push(Emission::JumpIfFalse {
                    condition: reg(&registers, *condition),
                    target: false_label,
                    span: block.terminator_span.clone(),
                });
                emit_copies(
                    &mut emissions,
                    &mut next,
                    &registers,
                    &function.blocks[then_target.0 as usize].parameters,
                    then_arguments,
                    block.terminator_span.clone(),
                )?;
                emissions.push(Emission::Jump(*then_target, block.terminator_span.clone()));
                labels[false_label.0 as usize] = Some(emissions.len());
                emit_copies(
                    &mut emissions,
                    &mut next,
                    &registers,
                    &function.blocks[else_target.0 as usize].parameters,
                    else_arguments,
                    block.terminator_span.clone(),
                )?;
                emissions.push(Emission::Jump(*else_target, block.terminator_span.clone()));
            }
            ir::Terminator::Return(value) => {
                emissions.push(Emission::Instruction(
                    vm::Instruction::Return {
                        source: reg(&registers, *value),
                    },
                    block.terminator_span.clone(),
                ));
            }
        }
    }

    let lowered = emissions
        .into_iter()
        .map(|emission| match emission {
            Emission::Instruction(instruction, span) => Ok((instruction, span)),
            Emission::Jump(target, span) => Ok((
                vm::Instruction::Jump {
                    target: label(&labels, target)?,
                },
                span,
            )),
            Emission::JumpIfFalse {
                condition,
                target,
                span,
            } => Ok((
                vm::Instruction::JumpIfFalse {
                    condition,
                    target: label(&labels, target)?,
                },
                span,
            )),
        })
        .collect::<Result<Vec<_>, LowerError>>()?;
    let (instructions, spans) = lowered.into_iter().unzip();
    let parameter_types = if metadata.parameter_types.is_empty() {
        function
            .signature
            .parameters
            .iter()
            .copied()
            .map(verification_type)
            .collect::<Vec<_>>()
    } else {
        metadata.parameter_types
    };
    let parameter_count = parameter_types.len();
    Ok(vm::BytecodeFunction {
        name: function.name.clone(),
        intrinsic_stub: metadata.intrinsic_stub,
        parameters: u16::try_from(parameter_count)
            .map_err(|_| LowerError("too many function parameters".into()))?,
        parameter_types,
        parameter_modes: if metadata.parameter_modes.is_empty() {
            vec![crate::ast::ParameterMode::Borrow; parameter_count]
        } else {
            metadata.parameter_modes
        },
        mutable_parameters: if metadata.mutable_parameters.is_empty() {
            vec![false; parameter_count]
        } else {
            metadata.mutable_parameters
        },
        returns_reference: metadata.returns_reference,
        captures: u16::try_from(function.captures.len())
            .map_err(|_| LowerError("too many function captures".into()))?,
        capture_types: if metadata.capture_types.is_empty() {
            function
                .capture_types
                .iter()
                .copied()
                .map(verification_type)
                .collect()
        } else {
            metadata.capture_types
        },
        result_type: metadata
            .result_type
            .unwrap_or_else(|| verification_type(function.signature.result)),
        registers: next,
        instructions,
        instruction_spans: spans,
    })
}

fn copies_needed(
    registers: &[Option<Register>],
    destinations: &[Value],
    sources: &[Value],
) -> bool {
    destinations
        .iter()
        .zip(sources)
        .any(|(destination, source)| reg(registers, *destination) != reg(registers, *source))
}

fn assign(
    registers: &mut [Option<Register>],
    value: Value,
    next: &mut u16,
) -> Result<(), LowerError> {
    if registers[value.0 as usize].is_none() {
        let register = Register(*next);
        *next = next
            .checked_add(1)
            .ok_or_else(|| LowerError("shared IR needs more than 65535 VM registers".into()))?;
        registers[value.0 as usize] = Some(register);
    }
    Ok(())
}

fn reg(registers: &[Option<Register>], value: Value) -> Register {
    registers[value.0 as usize].expect("all values assigned above")
}

fn label(labels: &[Option<usize>], block: Block) -> Result<usize, LowerError> {
    labels
        .get(block.0 as usize)
        .and_then(|label| *label)
        .ok_or_else(|| LowerError(format!("unresolved VM edge label b{}", block.0)))
}

fn emit_copies(
    emissions: &mut Vec<Emission>,
    next: &mut u16,
    registers: &[Option<Register>],
    destinations: &[Value],
    sources: &[Value],
    span: Range<usize>,
) -> Result<(), LowerError> {
    let mut copies = destinations
        .iter()
        .zip(sources)
        .map(|(destination, source)| (reg(registers, *destination), reg(registers, *source)))
        .filter(|(destination, source)| destination != source)
        .collect::<Vec<_>>();
    while !copies.is_empty() {
        if let Some(index) = copies
            .iter()
            .position(|(destination, _)| !copies.iter().any(|(_, source)| source == destination))
        {
            let (destination, source) = copies.remove(index);
            emissions.push(Emission::Instruction(
                vm::Instruction::Move {
                    destination,
                    source,
                },
                span.clone(),
            ));
            continue;
        }
        let preserved = copies[0].0;
        let temporary = Register(*next);
        *next = next
            .checked_add(1)
            .ok_or_else(|| LowerError("parallel copy needs a register past r65535".into()))?;
        emissions.push(Emission::Instruction(
            vm::Instruction::Move {
                destination: temporary,
                source: preserved,
            },
            span.clone(),
        ));
        for (_, source) in &mut copies {
            if *source == preserved {
                *source = temporary;
            }
        }
    }
    Ok(())
}

fn lower_instruction(
    instruction: &ir::Instruction,
    registers: &[Option<Register>],
    constants: &mut Vec<vm::Constant>,
    emissions: &mut Vec<Emission>,
    span: Range<usize>,
) -> Result<(), LowerError> {
    let instruction = match instruction {
        ir::Instruction::Constant { destination, value } => {
            let value = match value {
                ir::Constant::Unit => vm::Constant::Unit,
                ir::Constant::Bool(value) => vm::Constant::Bool(*value),
                ir::Constant::Integer(value) => vm::Constant::Integer(*value),
                ir::Constant::Float(value) => vm::Constant::Float(*value),
                ir::Constant::CodePoint(value) => vm::Constant::CodePoint(*value),
                ir::Constant::RuntimeString(_) => {
                    return Err(LowerError(
                        "runtime-string addresses must be legalized before VM lowering".into(),
                    ));
                }
            };
            let constant = constants
                .iter()
                .position(|existing| existing == &value)
                .unwrap_or_else(|| {
                    constants.push(value);
                    constants.len() - 1
                });
            vm::Instruction::LoadConstant {
                destination: reg(registers, *destination),
                constant: u16::try_from(constant)
                    .map_err(|_| LowerError("too many VM constants".into()))?,
            }
        }
        ir::Instruction::Unary {
            destination,
            operator,
            operand,
        } => vm::Instruction::Unary {
            destination: reg(registers, *destination),
            operator: *operator,
            operand: reg(registers, *operand),
        },
        ir::Instruction::IntegerExtend {
            destination,
            operand,
        } => vm::Instruction::Move {
            destination: reg(registers, *destination),
            source: reg(registers, *operand),
        },
        ir::Instruction::Binary {
            destination,
            operator,
            left,
            right,
        } => vm::Instruction::Binary {
            destination: reg(registers, *destination),
            operator: *operator,
            left: reg(registers, *left),
            right: reg(registers, *right),
        },
        ir::Instruction::Call {
            destination,
            function,
            arguments,
        } => vm::Instruction::Call {
            destination: reg(registers, *destination),
            function: *function,
            arguments: arguments
                .iter()
                .map(|value| reg(registers, *value))
                .collect(),
        },
        ir::Instruction::RuntimeCall { helper, .. } => {
            return Err(LowerError(format!(
                "runtime helper `{helper}` has no portable VM opcode"
            )));
        }
        ir::Instruction::Assert { condition, message } => vm::Instruction::Assert {
            condition: reg(registers, *condition),
            message: message.map(|value| reg(registers, value)),
        },
        ir::Instruction::Portable(instruction) => lower_portable(instruction, registers),
    };
    emissions.push(Emission::Instruction(instruction, span));
    Ok(())
}

fn lower_portable(
    instruction: &ir::PortableInstruction,
    registers: &[Option<Register>],
) -> vm::Instruction {
    let get = |value: &Value| reg(registers, *value);
    match instruction {
        ir::PortableInstruction::Drop { value } => vm::Instruction::Drop {
            register: get(value),
        },
        ir::PortableInstruction::LoadConstant {
            destination,
            constant,
        } => vm::Instruction::LoadConstant {
            destination: get(destination),
            constant: *constant,
        },
        ir::PortableInstruction::Move {
            destination,
            source,
        }
        | ir::PortableInstruction::CopyOnWrite {
            destination,
            source,
        } => vm::Instruction::Move {
            destination: get(destination),
            source: get(source),
        },
        ir::PortableInstruction::Unary {
            destination,
            operator,
            operand,
        } => vm::Instruction::Unary {
            destination: get(destination),
            operator: *operator,
            operand: get(operand),
        },
        ir::PortableInstruction::Binary {
            destination,
            operator,
            left,
            right,
        } => vm::Instruction::Binary {
            destination: get(destination),
            operator: *operator,
            left: get(left),
            right: get(right),
        },
        ir::PortableInstruction::MakeList {
            destination,
            elements,
        } => vm::Instruction::MakeList {
            destination: get(destination),
            elements: elements.iter().map(get).collect(),
        },
        ir::PortableInstruction::Index {
            destination,
            object,
            index,
        } => vm::Instruction::Index {
            destination: get(destination),
            object: get(object),
            index: get(index),
        },
        ir::PortableInstruction::MakeRecord {
            destination,
            record,
            fields,
        } => vm::Instruction::MakeRecord {
            destination: get(destination),
            record: *record,
            fields: fields
                .iter()
                .map(|(name, value)| (name.clone(), get(value)))
                .collect(),
        },
        ir::PortableInstruction::MakeVariant {
            destination,
            variant,
            payload,
        } => vm::Instruction::MakeVariant {
            destination: get(destination),
            variant: *variant,
            payload: payload.iter().map(get).collect(),
        },
        ir::PortableInstruction::LoadField {
            destination,
            object,
            field,
            by_reference,
        } => vm::Instruction::LoadField {
            destination: get(destination),
            object: get(object),
            field: field.clone(),
            by_reference: *by_reference,
        },
        ir::PortableInstruction::StoreField {
            object,
            field,
            source,
        } => vm::Instruction::StoreField {
            object: get(object),
            field: field.clone(),
            source: get(source),
        },
        ir::PortableInstruction::StoreIndex {
            object,
            index,
            source,
        } => vm::Instruction::StoreIndex {
            object: get(object),
            index: get(index),
            source: get(source),
        },
        ir::PortableInstruction::MakeReference {
            destination,
            object,
            index,
        } => vm::Instruction::MakeReference {
            destination: get(destination),
            object: get(object),
            index: get(index),
        },
        ir::PortableInstruction::MakeWholeReference {
            destination,
            object,
        } => vm::Instruction::MakeWholeReference {
            destination: get(destination),
            object: get(object),
        },
        ir::PortableInstruction::MakeFieldReference {
            destination,
            object,
            field,
        } => vm::Instruction::MakeFieldReference {
            destination: get(destination),
            object: get(object),
            field: field.clone(),
        },
        ir::PortableInstruction::MoveOut {
            destination,
            source,
        } => vm::Instruction::MoveOut {
            destination: get(destination),
            source: get(source),
        },
        ir::PortableInstruction::Push {
            destination,
            object,
            value,
        } => vm::Instruction::Push {
            destination: get(destination),
            object: get(object),
            value: get(value),
        },
        ir::PortableInstruction::Append {
            destination,
            object,
            value,
        } => vm::Instruction::Append {
            destination: get(destination),
            object: get(object),
            value: get(value),
        },
        ir::PortableInstruction::Contains {
            destination,
            value,
            candidates,
        } => vm::Instruction::Contains {
            destination: get(destination),
            value: get(value),
            candidates: candidates.iter().map(get).collect(),
        },
        ir::PortableInstruction::Builtin {
            destination,
            builtin,
            arguments,
        } => vm::Instruction::Builtin {
            destination: get(destination),
            builtin: *builtin,
            arguments: arguments.iter().map(get).collect(),
        },
        ir::PortableInstruction::SpawnRemote { destination, value } => {
            vm::Instruction::SpawnRemote {
                destination: get(destination),
                value: get(value),
            }
        }
        ir::PortableInstruction::SpawnRemoteBorrow {
            destination,
            source,
        } => vm::Instruction::SpawnRemoteBorrow {
            destination: get(destination),
            source: get(source),
        },
        ir::PortableInstruction::RemoteCall {
            destination,
            remote,
            function,
            arguments,
        } => vm::Instruction::RemoteCall {
            destination: get(destination),
            remote: get(remote),
            function: *function,
            arguments: arguments
                .iter()
                .map(|(mode, value)| (*mode, get(value)))
                .collect(),
        },
        ir::PortableInstruction::Await {
            destination,
            future,
        } => vm::Instruction::Await {
            destination: get(destination),
            future: get(future),
        },
        ir::PortableInstruction::MatchPattern {
            destination,
            subject,
            pattern,
            bindings,
        } => vm::Instruction::MatchPattern {
            destination: get(destination),
            subject: get(subject),
            pattern: pattern.clone(),
            bindings: bindings.iter().map(get).collect(),
        },
        ir::PortableInstruction::Assert { condition, message } => vm::Instruction::Assert {
            condition: get(condition),
            message: message.as_ref().map(get),
        },
        ir::PortableInstruction::Call {
            destination,
            function,
            arguments,
        } => vm::Instruction::Call {
            destination: get(destination),
            function: *function,
            arguments: arguments.iter().map(get).collect(),
        },
        ir::PortableInstruction::CallMethod {
            destination,
            receiver,
            function,
            arguments,
        } => vm::Instruction::CallMethod {
            destination: get(destination),
            receiver: get(receiver),
            function: *function,
            arguments: arguments.iter().map(get).collect(),
        },
        ir::PortableInstruction::CallContractMethod {
            destination,
            receiver,
            slot,
            name,
            arguments,
        } => vm::Instruction::CallContractMethod {
            destination: get(destination),
            receiver: get(receiver),
            slot: *slot,
            name: name.clone(),
            arguments: arguments.iter().map(get).collect(),
        },
        ir::PortableInstruction::MakeClosure {
            destination,
            function,
            captures,
        } => vm::Instruction::MakeClosure {
            destination: get(destination),
            function: *function,
            captures: captures
                .iter()
                .map(|(mode, value)| (*mode, get(value)))
                .collect(),
        },
        ir::PortableInstruction::CallValue {
            destination,
            callee,
            arguments,
        } => vm::Instruction::CallValue {
            destination: get(destination),
            callee: get(callee),
            arguments: arguments.iter().map(get).collect(),
        },
        ir::PortableInstruction::CallClosure {
            destination,
            function,
            captures,
            arguments,
        } => vm::Instruction::CallClosure {
            destination: get(destination),
            function: *function,
            captures: captures
                .iter()
                .map(|(mode, value)| (*mode, get(value)))
                .collect(),
            arguments: arguments.iter().map(get).collect(),
        },
    }
}

fn verification_type(ty: Type) -> VerificationType {
    match ty {
        Type::Unit => VerificationType::Unit,
        Type::Bool => VerificationType::Bool,
        Type::Int => VerificationType::Integer,
        Type::Float => VerificationType::Float,
        Type::CodePoint => VerificationType::CodePoint,
        Type::Byte => VerificationType::Byte,
        Type::Opaque | Type::String | Type::Arguments | Type::StringList | Type::Object(_) => {
            VerificationType::Unknown
        }
    }
}

#[cfg(test)]
mod tests {
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
                result_type: VerificationType::Unit,
                registers: 0,
                instructions: vec![vm::Instruction::Jump { target: 1 }],
                instruction_spans: vec![span],
            },
        );
        let original = program.functions.clone();

        assert!(lower_program_through_shared_ir(&mut program).is_err());
        assert_eq!(program.functions, original);
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
            capture_types: vec![],
            entry_seeds: vec![],
            entry: Block(0),
            entry_arguments: vec![Value(0), Value(1), Value(2)],
            value_types: vec![
                Type::Bool,
                Type::Int,
                Type::Int,
                Type::Bool,
                Type::Int,
                Type::Int,
                Type::Int,
            ],
            storage_hints: vec![None; 7],
            blocks: vec![
                ir::BlockData {
                    parameters: vec![Value(3), Value(4), Value(5)],
                    instructions: vec![],
                    instruction_spans: vec![],
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
                    instructions: vec![],
                    instruction_spans: vec![],
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
            capture_types: vec![],
            entry_seeds: vec![],
            entry: Block(0),
            entry_arguments: vec![],
            value_types: vec![Type::Bool, Type::Int, Type::Int, Type::Int],
            storage_hints: vec![None; 4],
            blocks: vec![
                ir::BlockData {
                    parameters: vec![],
                    instructions: vec![
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
                    instruction_spans: vec![0..0, 0..0, 0..0],
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
                    instructions: vec![],
                    instruction_spans: vec![],
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
            constants,
            main: Some(main),
            ..Program::default()
        };
        program.functions.insert(main, lowered);
        crate::vm::verify(&program).unwrap();
        assert_eq!(
            Machine::new(&program).run_main().unwrap(),
            vm::Value::Integer(41)
        );
    }
}
