//! Seal shared slot construction into SSA and retain logical write bindings.
#[cfg(test)]
mod evidence;
mod instructions;
mod program;
use self::instructions::portable_instruction;
use crate::codegen::LowerError;
use crate::codegen::ir::{self, Block, Type, Value};
use crate::codegen::metadata::Constant;
use crate::codegen::storage::{self, Slot};
use crate::codegen::types::ExecutableType;
use crate::hir::FunctionId;
pub use program::seal_program;
use std::collections::HashMap;
use std::ops::Range;
/// Seal shared logical slots into block-argument SSA while retaining observable storage homes.
pub fn seal_function(
    program: &storage::Program,
    function: &storage::Function,
) -> Result<ir::Function, LowerError> {
    let result_types = program
        .functions
        .iter()
        .map(|(id, function)| (*id, function.result_type.clone()))
        .collect::<HashMap<_, _>>();
    seal_function_with_types(&program.metadata.constants, &result_types, function)
}

#[derive(Default)]
pub(crate) struct SealingEvidence {
    // Prior SSA binding of a destination: Reference values denote assignment through a place.
    pub(crate) write_bindings: HashMap<Value, Value>,
    #[cfg(test)]
    pub(crate) sites: Vec<Vec<usize>>,
    #[cfg(test)]
    pub(crate) bindings: HashMap<usize, Vec<Option<Value>>>,
}

fn seal_function_with_types(
    constants: &[Constant],
    result_types: &HashMap<FunctionId, ExecutableType>,
    function: &storage::Function,
) -> Result<ir::Function, LowerError> {
    seal_function_with_evidence(constants, result_types, function).map(|(function, _)| function)
}

pub(super) fn seal_function_with_evidence(
    constants: &[Constant],
    result_types: &HashMap<FunctionId, ExecutableType>,
    function: &storage::Function,
) -> Result<(ir::Function, SealingEvidence), LowerError> {
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
    // Weak references need their origin storage through the frame's lifetime.
    // Preserve the drop planner's protection when pruning SSA block arguments.
    let mut origins = std::collections::HashSet::new();
    for instruction in &function.instructions {
        match instruction {
            storage::Instruction::MakeReference { object, .. }
            | storage::Instruction::MakeWholeReference { object, .. }
            | storage::Instruction::MakeFieldReference { object, .. }
            | storage::Instruction::LoadField {
                object,
                by_reference: true,
                ..
            } => {
                origins.insert(*object);
            }
            storage::Instruction::MakeClosure { captures, .. }
            | storage::Instruction::CallClosure { captures, .. } => {
                origins.extend(captures.iter().filter_map(|(mode, source)| {
                    (*mode == crate::hir::CaptureMode::Ref).then_some(*source)
                }));
            }
            _ => {}
        }
    }
    // Writable projection chains must detach shared parents before taking
    // child addresses; detaching only the final record would mutate a shared list.
    let mut writable = std::collections::HashSet::new();
    for instruction in &function.instructions {
        if let storage::Instruction::StoreField { object, .. }
        | storage::Instruction::StoreIndex { object, .. }
        | storage::Instruction::MoveOut {
            source: object,
            by_reference: true,
            ..
        } = instruction
        {
            writable.insert(*object);
        }
    }
    loop {
        let mut changed = false;
        for instruction in function.instructions.iter().rev() {
            if let storage::Instruction::MakeReference {
                destination,
                object,
                ..
            }
            | storage::Instruction::MakeFieldReference {
                destination,
                object,
                ..
            } = instruction
                && writable.contains(destination)
            {
                changed |= writable.insert(*object);
            }
        }
        if !changed {
            break;
        }
    }
    let mut reference_homes = function
        .capture_types
        .iter()
        .chain(&function.parameter_types)
        .enumerate()
        .filter_map(|(index, ty)| {
            matches!(ty, ExecutableType::Reference(_)).then_some(Slot(index as u16))
        })
        .collect::<std::collections::HashSet<_>>();
    for instruction in &function.instructions {
        // Variant payloads may carry references. Keep their previous bindings
        // available for assignment; flow analysis determines which are references.
        if let storage::Instruction::MatchPattern { bindings, .. } = instruction {
            reference_homes.extend(bindings.iter().copied());
        }
        let destination = match instruction {
            storage::Instruction::MakeReference { destination, .. }
            | storage::Instruction::MakeWholeReference { destination, .. }
            | storage::Instruction::MakeFieldReference { destination, .. }
            | storage::Instruction::LoadField { destination, by_reference: true, .. }
            // These reads can produce a reference depending on their operand's logical schema.
            | storage::Instruction::Index { destination, .. }
            | storage::Instruction::MoveOut { destination, .. }
            | storage::Instruction::CallValue { destination, .. }
            | storage::Instruction::CallContractMethod { destination, .. }
            | storage::Instruction::Await { destination, .. }
            | storage::Instruction::Binary { destination, operator: crate::ast::BinaryOp::Add, .. } => Some(*destination),
            storage::Instruction::Call { destination, function, specialization, .. }
            | storage::Instruction::CallMethod { destination, function, specialization, .. }
            | storage::Instruction::CallClosure { destination, function, specialization, .. }
                if result_types.get(function).is_some_and(|ty| matches!(ty.specialize(specialization), ExecutableType::Reference(_))) => Some(*destination),
            _ => None,
        };
        reference_homes.extend(destination);
    }
    loop {
        let before = reference_homes.len();
        for instruction in &function.instructions {
            if let storage::Instruction::Move {
                destination,
                source,
            } = instruction
                && reference_homes.contains(source)
            {
                reference_homes.insert(*destination);
            }
        }
        if before == reference_homes.len() {
            break;
        }
    }
    let liveness = crate::codegen::storage::analysis::liveness_with_write_bindings(
        function,
        &origins,
        &reference_homes,
    );
    let mut values = ir::ValueBuilder::default();
    let mut externals = Vec::new();
    for (register, ty) in hints
        .iter()
        .copied()
        .enumerate()
        .take(usize::from(function.captures + function.parameters))
    {
        externals.push(allocate_lifted_value(
            &mut values,
            ty,
            Slot(register as u16),
        ));
    }
    let capture_count = usize::from(function.captures);
    let captures = externals[..capture_count].to_vec();
    let parameters = externals[capture_count..].to_vec();
    let mut parameter_registers: Vec<Vec<Slot>> = Vec::new();
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
                allocate_lifted_value(&mut values, hints[usize::from(register.0)], *register)
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
                        &mut values,
                        hints[usize::from(register.0)],
                        *register,
                    );
                    entry_seeds.push(seed);
                    seed
                })
        })
        .collect::<Vec<_>>();

    #[allow(unused_mut)]
    let mut sources = SealingEvidence::default();
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
        #[cfg(test)]
        let mut origins = Vec::new();
        #[cfg(test)]
        let mut previous_source = start;
        for source_index in start..end {
            #[cfg(test)]
            {
                origins.resize(instructions.len(), previous_source);
                previous_source = source_index;
                sources.bindings.insert(source_index, state.clone());
            }
            let operation = &function.instructions[source_index];
            let source_span = function
                .instruction_spans
                .get(source_index)
                .cloned()
                .unwrap_or_default();
            if let storage::Instruction::MakeReference {
                destination,
                object,
                ..
            }
            | storage::Instruction::MakeFieldReference {
                destination,
                object,
                ..
            } = operation
                && writable.contains(destination)
            {
                let old = lifted_register(&state, *object, function)?;
                let unique =
                    allocate_lifted_value(&mut values, hints[usize::from(object.0)], *object);
                instructions.push(ir::Instruction::Portable(
                    ir::PortableInstruction::CopyOnWrite {
                        destination: unique,
                        source: old,
                    },
                ));
                instruction_spans.push(source_span.clone());
                state[usize::from(object.0)] = Some(unique);
            }
            match operation {
                storage::Instruction::Jump { target } => {
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
                storage::Instruction::JumpIfFalse { condition, target } => {
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
                storage::Instruction::Return { source } => {
                    terminator_span = source_span;
                    terminator = Some(ir::Terminator::Return(lifted_register(
                        &state, *source, function,
                    )?));
                    break;
                }
                storage::Instruction::Drop { register } => {
                    if let Some(value) = state[usize::from(register.0)] {
                        instructions.push(ir::Instruction::Portable(
                            ir::PortableInstruction::Drop { value },
                        ));
                        instruction_spans.push(source_span.clone());
                    }
                    if liveness.live_out[source_index].contains(register) {
                        // A later ownership boundary can drop the same home on another
                        // path. Carry an empty value along that edge, never its old owner.
                        let empty = allocate_lifted_value(
                            &mut values,
                            hints[usize::from(register.0)],
                            *register,
                        );
                        values.set_storage_hint(empty, None);
                        entry_seeds.push(empty);
                        state[usize::from(register.0)] = Some(empty);
                    } else {
                        state[usize::from(register.0)] = None;
                    }
                }
                storage::Instruction::LoadField {
                    destination,
                    object,
                    field,
                    by_reference: true,
                } => {
                    // A mutable field address must belong to this value's unique record,
                    // not to a record still shared with an independently owned snapshot.
                    let old = lifted_register(&state, *object, function)?;
                    let unique =
                        allocate_lifted_value(&mut values, hints[usize::from(object.0)], *object);
                    instructions.push(ir::Instruction::Portable(
                        ir::PortableInstruction::CopyOnWrite {
                            destination: unique,
                            source: old,
                        },
                    ));
                    instruction_spans.push(source_span.clone());
                    let address = allocate_lifted_value(
                        &mut values,
                        hints[usize::from(destination.0)],
                        *destination,
                    );
                    instructions.push(ir::Instruction::Portable(
                        ir::PortableInstruction::LoadField {
                            destination: address,
                            object: unique,
                            field: field.clone(),
                            by_reference: true,
                        },
                    ));
                    instruction_spans.push(source_span);
                    state[usize::from(object.0)] = Some(unique);
                    state[usize::from(destination.0)] = Some(address);
                }
                storage::Instruction::StoreField {
                    object,
                    field,
                    source,
                } => {
                    let old = lifted_register(&state, *object, function)?;
                    let value = lifted_register(&state, *source, function)?;
                    let unique =
                        allocate_lifted_value(&mut values, hints[usize::from(object.0)], *object);
                    instructions.push(ir::Instruction::Portable(
                        ir::PortableInstruction::CopyOnWrite {
                            destination: unique,
                            source: old,
                        },
                    ));
                    instruction_spans.push(source_span.clone());
                    instructions.push(ir::Instruction::Portable(
                        ir::PortableInstruction::StoreField {
                            object: unique,
                            field: field.clone(),
                            source: value,
                        },
                    ));
                    instruction_spans.push(source_span);
                    state[usize::from(object.0)] = Some(unique);
                }
                storage::Instruction::StoreIndex {
                    object,
                    index,
                    source,
                } => {
                    let old = lifted_register(&state, *object, function)?;
                    let index = lifted_register(&state, *index, function)?;
                    let value = lifted_register(&state, *source, function)?;
                    let unique =
                        allocate_lifted_value(&mut values, hints[usize::from(object.0)], *object);
                    instructions.push(ir::Instruction::Portable(
                        ir::PortableInstruction::CopyOnWrite {
                            destination: unique,
                            source: old,
                        },
                    ));
                    instruction_spans.push(source_span.clone());
                    instructions.push(ir::Instruction::Portable(
                        ir::PortableInstruction::StoreIndex {
                            object: unique,
                            index,
                            source: value,
                        },
                    ));
                    instruction_spans.push(source_span);
                    state[usize::from(object.0)] = Some(unique);
                }
                storage::Instruction::Push {
                    destination,
                    object,
                    value,
                } => {
                    let old = lifted_register(&state, *object, function)?;
                    let pushed = lifted_register(&state, *value, function)?;
                    let unique =
                        allocate_lifted_value(&mut values, hints[usize::from(object.0)], *object);
                    instructions.push(ir::Instruction::Portable(
                        ir::PortableInstruction::CopyOnWrite {
                            destination: unique,
                            source: old,
                        },
                    ));
                    instruction_spans.push(source_span.clone());
                    let result = allocate_lifted_value(&mut values, Type::Unit, *destination);
                    instructions.push(ir::Instruction::Portable(ir::PortableInstruction::Push {
                        destination: result,
                        object: unique,
                        value: pushed,
                    }));
                    instruction_spans.push(source_span);
                    state[usize::from(object.0)] = Some(unique);
                    if let Some(previous) = state[usize::from(destination.0)] {
                        sources.write_bindings.insert(result, previous);
                    }
                    state[usize::from(destination.0)] = Some(result);
                }
                operation => {
                    for register in crate::codegen::storage::analysis::uses(operation) {
                        lifted_register(&state, register, function)?;
                    }
                    let definitions = crate::codegen::storage::analysis::definitions(operation);
                    let mut destinations = Vec::with_capacity(definitions.len());
                    for register in definitions {
                        let value = allocate_lifted_value(
                            &mut values,
                            hints[usize::from(register.0)],
                            register,
                        );
                        if let Some(previous) = state[usize::from(register.0)] {
                            sources.write_bindings.insert(value, previous);
                        }
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
                    if let storage::Instruction::MoveOut {
                        source,
                        by_reference: false,
                        ..
                    } = operation
                    {
                        // MoveOut empties the VM storage home. A later cleanup
                        // edge must carry that empty slot, not the old owner.
                        state[usize::from(source.0)] =
                            if liveness.live_out[source_index].contains(source) {
                                let empty = allocate_lifted_value(
                                    &mut values,
                                    hints[usize::from(source.0)],
                                    *source,
                                );
                                values.set_storage_hint(empty, None);
                                entry_seeds.push(empty);
                                Some(empty)
                            } else {
                                None
                            };
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
        #[cfg(test)]
        {
            origins.resize(instructions.len(), previous_source);
            origins.push(previous_source);
            sources.sites.push(origins);
        }
        blocks.push(ir::BlockData {
            parameters: block_parameters[block_index].clone(),
            instructions: ir::SpannedInstruction::from_parts(instructions, instruction_spans),
            terminator,
            terminator_span,
        });
    }
    Ok((
        ir::Function {
            name: function.name.clone(),
            signature: ir::Signature {
                parameters: (capture_count..capture_count + usize::from(function.parameters))
                    .map(|register| hints[register])
                    .collect(),
                result: shared_type(&function.result_type),
            },
            parameters,
            captures: captures
                .into_iter()
                .enumerate()
                .map(|(register, value)| ir::Capture {
                    value,
                    ty: hints[register],
                })
                .collect(),
            entry_seeds,
            entry: Block(0),
            entry_arguments,
            values: values.finish(),
            blocks,
        },
        sources,
    ))
}

fn allocate_lifted_value(values: &mut ir::ValueBuilder, ty: Type, register: Slot) -> Value {
    values.allocate(ty, Some(register.0))
}

fn lifted_register(
    state: &[Option<Value>],
    register: Slot,
    function: &storage::Function,
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
    parameter_registers: &[Vec<Slot>],
    state: &[Option<Value>],
    function: &storage::Function,
) -> Result<Vec<Value>, LowerError> {
    parameter_registers[target.0 as usize]
        .iter()
        .map(|register| lifted_register(state, *register, function))
        .collect()
}

fn block_leaders(function: &storage::Function) -> Result<Vec<usize>, LowerError> {
    let mut leaders = vec![false; function.instructions.len()];
    leaders[0] = true;
    for (index, instruction) in function.instructions.iter().enumerate() {
        match instruction {
            storage::Instruction::Jump { target }
            | storage::Instruction::JumpIfFalse { target, .. } => {
                if *target >= function.instructions.len() {
                    return Err(LowerError(format!("invalid jump target {target}")));
                }
                leaders[*target] = true;
                if index + 1 < function.instructions.len() {
                    leaders[index + 1] = true;
                }
            }
            storage::Instruction::Return { .. } if index + 1 < function.instructions.len() => {
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

fn shared_type(ty: &ExecutableType) -> Type {
    match ty {
        ExecutableType::Unit => Type::Unit,
        ExecutableType::Bool => Type::Bool,
        ExecutableType::Integer => Type::Int,
        ExecutableType::Float => Type::Float,
        ExecutableType::CodePoint => Type::CodePoint,
        ExecutableType::Byte => Type::Byte,
        _ => Type::Opaque,
    }
}

fn register_type_hints(
    constants: &[Constant],
    result_types: &HashMap<FunctionId, ExecutableType>,
    function: &storage::Function,
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
            storage::Instruction::LoadConstant {
                destination,
                constant,
            } => (
                Some(*destination),
                match &constants[usize::from(*constant)] {
                    Constant::Unit => Type::Unit,
                    Constant::Bool(_) => Type::Bool,
                    Constant::Integer(_) => Type::Int,
                    Constant::Float(_) => Type::Float,
                    Constant::CodePoint(_) => Type::CodePoint,
                    Constant::String(_) => Type::String,
                    Constant::Symbol(_) => Type::Opaque,
                },
            ),
            storage::Instruction::Unary {
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
            storage::Instruction::Binary {
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
            storage::Instruction::Move {
                destination,
                source,
            } => (Some(*destination), hints[usize::from(source.0)]),
            storage::Instruction::Contains { destination, .. }
            | storage::Instruction::MatchPattern { destination, .. } => {
                (Some(*destination), Type::Bool)
            }
            storage::Instruction::Call {
                destination,
                function,
                ..
            }
            | storage::Instruction::CallMethod {
                destination,
                function,
                ..
            }
            | storage::Instruction::CallClosure {
                destination,
                function,
                ..
            } => (
                Some(*destination),
                result_types
                    .get(function)
                    .map(shared_type)
                    .unwrap_or(Type::Opaque),
            ),
            _ => (None, Type::Opaque),
        };
        if let Some(destination) = destination {
            hints[usize::from(destination.0)] = ty;
        }
        match instruction {
            storage::Instruction::JumpIfFalse { condition, .. }
            | storage::Instruction::Assert {
                condition,
                message: _,
            } => hints[usize::from(condition.0)] = Type::Bool,
            storage::Instruction::Return { source } => {
                hints[usize::from(source.0)] = shared_type(&function.result_type);
            }
            _ => {}
        }
    }
    hints
}
