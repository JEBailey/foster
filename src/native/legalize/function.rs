//! Function preparation, home tracking, cleanup, and control-flow assembly.
use crate::native::{
    BTreeSet, BytecodeFunction, FailureCleanup, FosterError, HashMap, NativeIrEnvironment,
    NativeType, Range, SpecializationKey, abi, infer_value_types, ir, native_error,
    native_verification_type, reference_load_helper, verified_remote_calls,
};

use super::conversions::{ReturnConversion, allocate_shared_value};
use super::edges::{adapt_arguments, record_cleanup};
use super::instructions::{NativeFunctionFacts, lower_shared_instruction};
pub(in crate::native) fn lower_shared_to_native_ir(
    shared: &ir::Function,
    metadata: &BytecodeFunction,
    source_states: &crate::codegen::flow::FunctionFacts,
    function_signature: &ir::Signature,
    instance: &SpecializationKey,
    environment: NativeIrEnvironment<'_>,
) -> Result<(ir::Function, FailureCleanup), FosterError> {
    let remote_calls = verified_remote_calls(shared, source_states, instance, environment)?;
    let external_values = shared
        .captures
        .iter()
        .map(|capture| &capture.value)
        .chain(&shared.parameters);
    let external_types = metadata
        .capture_types
        .iter()
        .chain(&metadata.parameter_types);
    let reference_homes = external_values
        .zip(external_types)
        .filter_map(|(value, ty)| {
            matches!(ty, crate::codegen::types::ExecutableType::Reference(_))
                .then(|| {
                    shared
                        .values
                        .hint(value.0 as usize)
                        .map(|home| (home, *value))
                })
                .flatten()
        })
        .collect::<HashMap<_, _>>();
    let inferred = infer_value_types(
        shared,
        &function_signature.parameters,
        instance,
        environment,
        &remote_calls,
    )?;
    let mut values = shared.values.clone().into_builder();
    let empty = inferred.empty;
    for (index, ty) in inferred.types.into_iter().enumerate() {
        values[index] = ty;
    }
    let mut entry_seeds = shared.entry_seeds.clone();
    let mut blocks = Vec::with_capacity(shared.blocks.len());
    let mut cleanup_edges = Vec::new();
    let mut failure_cleanup = FailureCleanup::default();

    for (block_index, block) in shared.blocks.iter().enumerate() {
        let mut state = HashMap::<u16, ir::Value>::new();
        let mut temporaries = BTreeSet::new();
        for value in &block.parameters {
            if let Some(home) = shared.values.hint(value.0 as usize) {
                state.insert(home, *value);
            }
        }
        let mut instructions = Vec::new();
        let mut spans = Vec::new();
        let poll = values.allocate(NativeType::Bool, None);
        failure_cleanup.values.insert(
            (block_index, 0),
            owned_home_values(&state, &reference_homes).collect(),
        );
        instructions.push(ir::Instruction::RuntimeCall {
            destination: poll,
            helper: abi::CANCELLATION_POINT,
            signature: ir::Signature {
                parameters: vec![],
                result: NativeType::Bool,
            },
            arguments: vec![],
        });
        spans.push(block.terminator_span.clone());
        for (instruction, span) in block
            .instructions
            .iter()
            .map(|entry| (&entry.instruction, &entry.span))
        {
            let lowered = lower_shared_instruction(
                instruction,
                metadata,
                instance,
                environment,
                &mut values,
                NativeFunctionFacts {
                    reference_homes: &reference_homes,
                    remote_calls: &remote_calls,
                },
            )?;
            for (instruction, consumed) in lowered {
                if let ir::Instruction::Portable(ir::PortableInstruction::Drop { value }) =
                    &instruction
                {
                    if !remove_shared_home(&mut state, &values, *value) {
                        continue;
                    }
                    temporaries.remove(value);
                    if values
                        .hint(value.0 as usize)
                        .is_some_and(|home| reference_homes.contains_key(&home))
                    {
                        // A loaded reference parameter never owns its caller's value.
                        continue;
                    }
                }
                for value in consumed {
                    remove_shared_home(&mut state, &values, value);
                    temporaries.remove(&value);
                }
                // ABI argument copies and conversions have no construction home,
                // but own references until transferred to a call or closure.
                let live = owned_home_values(&state, &reference_homes)
                    .chain(temporaries.iter().copied())
                    .filter(|value| {
                        matches!(
                            values[value.0 as usize],
                            NativeType::String | NativeType::Object(_)
                        )
                    })
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                failure_cleanup
                    .values
                    .insert((block_index, instructions.len()), live);
                for destination in instruction.destinations() {
                    if let Some(home) = values.hint(destination.0 as usize) {
                        state.insert(home, destination);
                    } else if !matches!(
                        &instruction,
                        ir::Instruction::RuntimeCall {
                            helper: abi::REF_LOAD_PTR,
                            ..
                        }
                    ) {
                        temporaries.insert(destination);
                    }
                }
                instructions.push(instruction);
                spans.push(span.clone());
            }
        }
        let mut terminator = block.terminator.clone();
        // Native calls transfer consumed arguments instead of retaining VM
        // aliases. Do not resurrect their old storage at a later block join.
        let mut clear_transferred = |arguments: &mut Vec<ir::Value>| {
            for argument in arguments {
                if matches!(
                    values[argument.0 as usize],
                    NativeType::String | NativeType::Object(_)
                ) && values
                    .hint(argument.0 as usize)
                    .is_some_and(|home| !state.contains_key(&home))
                {
                    let ty = values[argument.0 as usize];
                    let empty = allocate_shared_value(&mut values, ty);
                    entry_seeds.push(empty);
                    *argument = empty;
                }
            }
        };
        match &mut terminator {
            ir::Terminator::Jump { arguments, .. } => clear_transferred(arguments),
            ir::Terminator::Branch {
                then_arguments,
                else_arguments,
                ..
            } => {
                clear_transferred(then_arguments);
                clear_transferred(else_arguments);
            }
            ir::Terminator::Return(_) => {}
        }
        if let ir::Terminator::Return(returned) = &terminator {
            let mut returned = *returned;
            if let Some(conversion) = ReturnConversion::between(
                values[returned.0 as usize],
                function_signature.result,
                environment,
            )? {
                let converted = allocate_shared_value(&mut values, function_signature.result);
                instructions.push(conversion.instruction(converted, returned));
                spans.push(block.terminator_span.clone());
                match conversion {
                    ReturnConversion::Reference => {
                        if matches!(
                            function_signature.result,
                            NativeType::Object(_) | NativeType::String
                        ) {
                            temporaries.insert(converted);
                        }
                    }
                    ReturnConversion::ResultError => {
                        instructions.push(ir::Instruction::Portable(
                            ir::PortableInstruction::Drop { value: returned },
                        ));
                        spans.push(block.terminator_span.clone());
                        remove_shared_home(&mut state, &values, returned);
                    }
                    _ => {}
                }
                returned = converted;
            }
            terminator = ir::Terminator::Return(returned);
            for value in owned_home_values(&state, &reference_homes)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .rev()
            {
                if value != returned
                    && matches!(
                        values[value.0 as usize],
                        NativeType::Object(_) | NativeType::String
                    )
                {
                    remove_shared_home(&mut state, &values, value);
                    let live = owned_home_values(&state, &reference_homes)
                        .chain(temporaries.iter().copied())
                        .filter(|value| {
                            matches!(
                                values[value.0 as usize],
                                NativeType::Object(_) | NativeType::String
                            )
                        })
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .rev()
                        .collect();
                    failure_cleanup
                        .values
                        .insert((block_index, instructions.len()), live);
                    instructions.push(ir::Instruction::Portable(ir::PortableInstruction::Drop {
                        value,
                    }));
                    spans.push(block.terminator_span.clone());
                }
            }
        }
        // Pruned SSA block arguments no longer carry dead storage into the successor.
        // Release that storage on the particular edge where its lifetime ends.
        let owned = owned_home_values(&state, &reference_homes).collect::<BTreeSet<_>>();
        let dying = |arguments: &[ir::Value], values: &ir::ValueTable| {
            owned
                .iter()
                .copied()
                .filter(|value| !arguments.contains(value))
                .filter(|value| {
                    matches!(
                        values[value.0 as usize],
                        NativeType::Object(_) | NativeType::String
                    )
                })
                .map(|value| ir::Instruction::Portable(ir::PortableInstruction::Drop { value }))
                .collect::<Vec<_>>()
        };
        match &mut terminator {
            ir::Terminator::Jump { target, arguments } => {
                let conversions = adapt_arguments(
                    arguments,
                    &shared.blocks[target.0 as usize].parameters,
                    &mut values,
                    &mut entry_seeds,
                    &empty,
                    environment,
                )?;
                let edge_instructions = conversions
                    .into_iter()
                    .chain(dying(arguments, &values))
                    .collect::<Vec<_>>();
                record_cleanup(
                    &edge_instructions,
                    owned.iter().chain(&temporaries).copied(),
                    block_index,
                    instructions.len(),
                    &values,
                    &mut failure_cleanup,
                );
                for drop in edge_instructions {
                    instructions.push(drop);
                    spans.push(block.terminator_span.clone());
                }
            }
            ir::Terminator::Branch {
                then_target,
                then_arguments,
                else_target,
                else_arguments,
                ..
            } => {
                for (target, arguments) in
                    [(then_target, then_arguments), (else_target, else_arguments)]
                {
                    let mut drops = adapt_arguments(
                        arguments,
                        &shared.blocks[target.0 as usize].parameters,
                        &mut values,
                        &mut entry_seeds,
                        &empty,
                        environment,
                    )?;
                    drops.extend(dying(arguments, &values));
                    if drops.is_empty() {
                        continue;
                    }
                    let edge = ir::Block((shared.blocks.len() + cleanup_edges.len()) as u32);
                    record_cleanup(
                        &drops,
                        owned.iter().chain(&temporaries).copied(),
                        edge.0 as usize,
                        0,
                        &values,
                        &mut failure_cleanup,
                    );
                    cleanup_edges.push(ir::BlockData {
                        parameters: Vec::new(),
                        instructions: drops
                            .into_iter()
                            .map(|instruction| instruction.with_span(block.terminator_span.clone()))
                            .collect(),
                        terminator: ir::Terminator::Jump {
                            target: *target,
                            arguments: arguments.clone(),
                        },
                        terminator_span: block.terminator_span.clone(),
                    });
                    *target = edge;
                    arguments.clear();
                }
            }
            ir::Terminator::Return(_) => {}
        }
        blocks.push(ir::BlockData {
            parameters: block.parameters.clone(),
            instructions: ir::SpannedInstruction::from_parts(instructions, spans),
            terminator,
            terminator_span: block.terminator_span.clone(),
        });
    }
    blocks.extend(cleanup_edges);

    let mut parameters = shared
        .captures
        .iter()
        .map(|capture| capture.value)
        .collect::<Vec<_>>();
    parameters.extend(&shared.parameters);
    // Pruned entry arguments never reach block-local ownership state. The ABI
    // still transfers these references, including an unused method receiver.
    let unused_parameters = parameters
        .iter()
        .copied()
        .filter(|value| !shared.entry_arguments.contains(value))
        .filter(|value| {
            matches!(
                values[value.0 as usize],
                NativeType::String | NativeType::Object(_)
            )
        })
        .collect::<Vec<_>>();
    let mut entry = shared.entry;
    let mut entry_arguments = shared.entry_arguments.clone();
    let entry_needs_conversion = entry_arguments
        .iter()
        .zip(&shared.blocks[shared.entry.0 as usize].parameters)
        .any(|(argument, parameter)| values[argument.index()] != values[parameter.index()]);
    if !reference_homes.is_empty() || !unused_parameters.is_empty() || entry_needs_conversion {
        failure_cleanup.values = failure_cleanup
            .values
            .into_iter()
            .map(|((block, instruction), values)| ((block + 1, instruction), values))
            .collect();
        let mut loaded_captures = HashMap::new();
        let mut prologue_instructions = Vec::new();
        let mut prologue_spans = Vec::new();
        for value in unused_parameters.into_iter().rev() {
            prologue_instructions.push(ir::Instruction::Portable(ir::PortableInstruction::Drop {
                value,
            }));
            prologue_spans.push(Range::default());
        }
        for (input, input_type) in shared
            .captures
            .iter()
            .map(|capture| &capture.value)
            .chain(&shared.parameters)
            .zip(
                metadata
                    .capture_types
                    .iter()
                    .chain(&metadata.parameter_types),
            )
        {
            let crate::codegen::types::ExecutableType::Reference(pointee) = input_type else {
                continue;
            };
            // Pruned reference inputs were released above and have no pointee users.
            if !shared.entry_arguments.contains(input) {
                continue;
            }

            let concrete_pointee = pointee.specialize(&instance.substitutions);
            let reference_type = NativeType::Object(
                environment
                    .layouts
                    .pointer(
                        &concrete_pointee,
                        crate::codegen::layout::Ownership::Borrowed,
                    )
                    .ok_or_else(|| native_error("captured reference has no native layout"))?,
            );
            values[input.0 as usize] = reference_type;
            let loaded_type = native_verification_type(
                &environment.program.metadata,
                environment.layouts,
                &concrete_pointee,
                None,
            )?;
            let loaded = allocate_shared_value(&mut values, loaded_type);
            prologue_instructions.push(ir::Instruction::RuntimeCall {
                destination: loaded,
                helper: reference_load_helper(loaded_type),
                signature: ir::Signature {
                    parameters: vec![reference_type],
                    result: loaded_type,
                },
                arguments: vec![*input],
            });
            prologue_spans.push(Range::default());
            loaded_captures.insert(*input, loaded);
        }
        for block in &mut blocks {
            shift_native_blocks(&mut block.terminator, 1);
        }
        let mut arguments = shared
            .entry_arguments
            .iter()
            .map(|value| loaded_captures.get(value).copied().unwrap_or(*value))
            .collect::<Vec<_>>();
        let original_arguments = arguments.clone();
        let conversion_offset = prologue_instructions.len();
        let conversions = adapt_arguments(
            &mut arguments,
            &shared.blocks[shared.entry.0 as usize].parameters,
            &mut values,
            &mut entry_seeds,
            &empty,
            environment,
        )?;
        for instruction in conversions {
            prologue_instructions.push(instruction);
            prologue_spans.push(Range::default());
        }
        for value in original_arguments.iter().copied().collect::<BTreeSet<_>>() {
            if !arguments.contains(&value)
                && matches!(
                    values[value.index()],
                    NativeType::Object(_) | NativeType::String
                )
            {
                prologue_instructions.push(ir::Instruction::Portable(
                    ir::PortableInstruction::Drop { value },
                ));
                prologue_spans.push(Range::default());
            }
        }
        record_cleanup(
            &prologue_instructions[conversion_offset..],
            original_arguments,
            0,
            conversion_offset,
            &values,
            &mut failure_cleanup,
        );
        blocks.insert(
            0,
            ir::BlockData {
                parameters: Vec::new(),
                instructions: ir::SpannedInstruction::from_parts(
                    prologue_instructions,
                    prologue_spans,
                ),
                terminator: ir::Terminator::Jump {
                    target: ir::Block(shared.entry.0 + 1),
                    arguments,
                },
                terminator_span: Range::default(),
            },
        );
        entry = ir::Block(0);
        entry_arguments = Vec::new();
    }
    Ok((
        ir::Function {
            name: shared.name.clone(),
            signature: function_signature.clone(),
            parameters,
            captures: Vec::new(),
            entry_seeds,
            entry,
            entry_arguments,
            values: values.finish(),
            blocks,
        },
        failure_cleanup,
    ))
}

fn shift_native_blocks(terminator: &mut ir::Terminator, offset: u32) {
    match terminator {
        ir::Terminator::Jump { target, .. } => target.0 += offset,
        ir::Terminator::Branch {
            then_target,
            else_target,
            ..
        } => {
            then_target.0 += offset;
            else_target.0 += offset;
        }
        ir::Terminator::Return(_) => {}
    }
}

/// Loaded reference parameters belong to the caller, not this frame.
fn owned_home_values<'a>(
    state: &'a HashMap<u16, ir::Value>,
    reference_homes: &'a HashMap<u16, ir::Value>,
) -> impl Iterator<Item = ir::Value> + 'a {
    state
        .iter()
        .filter_map(|(home, value)| (!reference_homes.contains_key(home)).then_some(*value))
}

fn remove_shared_home(
    state: &mut HashMap<u16, ir::Value>,
    values: &ir::ValueTable,
    value: ir::Value,
) -> bool {
    let Some(home) = values.hint(value.0 as usize) else {
        return true;
    };
    if state.get(&home) == Some(&value) {
        state.remove(&home);
        true
    } else {
        false
    }
}
