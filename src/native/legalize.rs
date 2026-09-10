//! Legalize shared IR into native IR, including ownership and call conversions.
use super::{
    BTreeSet, BinaryOp, BytecodeFunction, Constant, FailureCleanup, FosterError, HashMap, HashSet,
    LayoutKind, LayoutRegistry, NativeIrEnvironment, NativeType, ParameterMode, PhysicalKind,
    Range, SpecializationKey, VerificationType, VerifiedRemoteCall, abi, contract_candidates,
    dereference_native_type, infer_register_types, ir, native_error, native_field_helper,
    native_intrinsic_result_type, native_verification_type, reference_load_helper,
    reference_store_helper, resolve_specialization, runtime_signature, verified_remote_calls,
};

pub(super) fn lower_shared_to_native_ir(
    shared: &ir::Function,
    metadata: &BytecodeFunction,
    source_states: &[Option<Vec<Option<VerificationType>>>],
    function_signature: &ir::Signature,
    instance: &SpecializationKey,
    environment: NativeIrEnvironment<'_>,
) -> Result<(ir::Function, FailureCleanup), FosterError> {
    let remote_calls = verified_remote_calls(metadata, source_states, instance, environment)?;
    let external_values = shared.captures.iter().chain(&shared.parameters);
    let external_types = metadata
        .capture_types
        .iter()
        .chain(&metadata.parameter_types);
    let reference_homes = external_values
        .zip(external_types)
        .filter_map(|(value, ty)| {
            matches!(ty, crate::vm::VerificationType::Reference(_))
                .then(|| shared.storage_hints[value.0 as usize].map(|home| (home, *value)))
                .flatten()
        })
        .collect::<HashMap<_, _>>();
    let inferred = infer_register_types(
        metadata,
        &function_signature.parameters,
        instance,
        environment,
        &remote_calls,
    )?;
    let mut value_types = shared
        .storage_hints
        .iter()
        .enumerate()
        .map(|(index, register)| {
            register
                .and_then(|register| inferred[usize::from(register)])
                .or_else(|| {
                    shared
                        .value_types
                        .get(index)
                        .copied()
                        .map(native_shared_type)
                })
                .unwrap_or(NativeType::Unit)
        })
        .collect::<Vec<_>>();
    for (home, value) in &reference_homes {
        let Some(crate::vm::VerificationType::Reference(pointee)) = metadata
            .capture_types
            .iter()
            .chain(&metadata.parameter_types)
            .nth(usize::from(*home))
        else {
            continue;
        };
        let pointee = pointee.specialize(&instance.substitutions);
        let pointee_type =
            native_verification_type(environment.program, environment.layouts, &pointee, None)?;
        // The ABI input is a typed address, while every SSA value carrying
        // that storage home after the prologue is the loaded pointee value.
        for (index, storage_home) in shared.storage_hints.iter().enumerate() {
            if storage_home == &Some(*home) {
                value_types[index] = pointee_type;
            }
        }
        let layout = environment
            .layouts
            .pointer(&pointee, crate::codegen::layout::Ownership::Borrowed)
            .ok_or_else(|| native_error("captured reference has no native layout"))?;
        value_types[value.0 as usize] = NativeType::Object(layout);
    }
    // Empty values carried after a Drop have no storage home. Recover their
    // specialized layout from the receiving block instead of keeping Opaque.
    let seeds = shared.entry_seeds.iter().copied().collect::<HashSet<_>>();
    for block in &shared.blocks {
        let edges = match &block.terminator {
            ir::Terminator::Jump { target, arguments } => vec![(*target, arguments)],
            ir::Terminator::Branch {
                then_target,
                then_arguments,
                else_target,
                else_arguments,
                ..
            } => vec![
                (*then_target, then_arguments),
                (*else_target, else_arguments),
            ],
            ir::Terminator::Return(_) => Vec::new(),
        };
        for (target, arguments) in edges {
            for (argument, parameter) in arguments
                .iter()
                .zip(&shared.blocks[target.0 as usize].parameters)
            {
                if seeds.contains(argument) && shared.storage_hints[argument.0 as usize].is_none() {
                    value_types[argument.0 as usize] = value_types[parameter.0 as usize];
                }
            }
        }
    }
    let mut storage_hints = shared.storage_hints.clone();
    let mut blocks = Vec::with_capacity(shared.blocks.len());
    let mut cleanup_edges = Vec::new();
    let mut failure_cleanup = FailureCleanup::default();

    for (block_index, block) in shared.blocks.iter().enumerate() {
        let mut state = HashMap::<u16, ir::Value>::new();
        let mut temporaries = BTreeSet::new();
        for value in &block.parameters {
            if let Some(home) = shared.storage_hints[value.0 as usize] {
                state.insert(home, *value);
            }
        }
        let mut instructions = Vec::new();
        let mut spans = Vec::new();
        let poll = ir::Value(value_types.len() as u32);
        value_types.push(NativeType::Bool);
        storage_hints.push(None);
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
        for (instruction, span) in block.instructions.iter().zip(&block.instruction_spans) {
            let lowered = lower_shared_instruction(
                instruction,
                metadata,
                instance,
                environment,
                &mut value_types,
                &mut storage_hints,
                NativeFunctionFacts {
                    reference_homes: &reference_homes,
                    remote_calls: &remote_calls,
                },
            )?;
            for (instruction, consumed) in lowered {
                if let ir::Instruction::Portable(ir::PortableInstruction::Drop { value }) =
                    &instruction
                {
                    if !remove_shared_home(&mut state, &storage_hints, *value) {
                        continue;
                    }
                    temporaries.remove(value);
                    if storage_hints[value.0 as usize]
                        .is_some_and(|home| reference_homes.contains_key(&home))
                    {
                        // A loaded reference parameter never owns its caller's value.
                        continue;
                    }
                }
                for value in consumed {
                    remove_shared_home(&mut state, &storage_hints, value);
                    temporaries.remove(&value);
                }
                // ABI argument copies and conversions have no construction home,
                // but own references until transferred to a call or closure.
                let live = owned_home_values(&state, &reference_homes)
                    .chain(temporaries.iter().copied())
                    .filter(|value| {
                        matches!(
                            value_types[value.0 as usize],
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
                    if let Some(home) = storage_hints[destination.0 as usize] {
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
        if let ir::Terminator::Return(returned) = &terminator {
            let mut returned = *returned;
            if let Some(conversion) = ReturnConversion::between(
                value_types[returned.0 as usize],
                function_signature.result,
                environment,
            )? {
                let converted = allocate_shared_value(
                    &mut value_types,
                    &mut storage_hints,
                    function_signature.result,
                );
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
                        remove_shared_home(&mut state, &storage_hints, returned);
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
                        value_types[value.0 as usize],
                        NativeType::Object(_) | NativeType::String
                    )
                {
                    remove_shared_home(&mut state, &storage_hints, value);
                    let live = owned_home_values(&state, &reference_homes)
                        .chain(temporaries.iter().copied())
                        .filter(|value| {
                            matches!(
                                value_types[value.0 as usize],
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
        let dying = |arguments: &[ir::Value]| {
            owned
                .iter()
                .copied()
                .filter(|value| !arguments.contains(value))
                .filter(|value| {
                    matches!(
                        value_types[value.0 as usize],
                        NativeType::Object(_) | NativeType::String
                    )
                })
                .map(|value| ir::Instruction::Portable(ir::PortableInstruction::Drop { value }))
                .collect::<Vec<_>>()
        };
        match &mut terminator {
            ir::Terminator::Jump { arguments, .. } => {
                for drop in dying(arguments) {
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
                    let drops = dying(arguments);
                    if drops.is_empty() {
                        continue;
                    }
                    let edge = ir::Block((shared.blocks.len() + cleanup_edges.len()) as u32);
                    cleanup_edges.push(ir::BlockData {
                        parameters: Vec::new(),
                        instruction_spans: vec![block.terminator_span.clone(); drops.len()],
                        instructions: drops,
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
            instructions,
            instruction_spans: spans,
            terminator,
            terminator_span: block.terminator_span.clone(),
        });
    }
    blocks.extend(cleanup_edges);

    let mut parameters = shared.captures.clone();
    parameters.extend(&shared.parameters);
    // Pruned entry arguments never reach block-local ownership state. The ABI
    // still transfers these references, including an unused method receiver.
    let unused_parameters = parameters
        .iter()
        .copied()
        .filter(|value| !shared.entry_arguments.contains(value))
        .filter(|value| {
            matches!(
                value_types[value.0 as usize],
                NativeType::String | NativeType::Object(_)
            )
        })
        .collect::<Vec<_>>();
    let mut entry = shared.entry;
    let mut entry_arguments = shared.entry_arguments.clone();
    if !reference_homes.is_empty() || !unused_parameters.is_empty() {
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
        for (input, input_type) in shared.captures.iter().chain(&shared.parameters).zip(
            metadata
                .capture_types
                .iter()
                .chain(&metadata.parameter_types),
        ) {
            let crate::vm::VerificationType::Reference(pointee) = input_type else {
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
            value_types[input.0 as usize] = reference_type;
            let loaded_type = native_verification_type(
                environment.program,
                environment.layouts,
                &concrete_pointee,
                None,
            )?;
            let loaded = allocate_shared_value(&mut value_types, &mut storage_hints, loaded_type);
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
        let arguments = shared
            .entry_arguments
            .iter()
            .map(|value| loaded_captures.get(value).copied().unwrap_or(*value))
            .collect();
        blocks.insert(
            0,
            ir::BlockData {
                parameters: Vec::new(),
                instructions: prologue_instructions,
                instruction_spans: prologue_spans,
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
            capture_types: Vec::new(),
            entry_seeds: shared.entry_seeds.clone(),
            entry,
            entry_arguments,
            value_types,
            storage_hints,
            blocks,
        },
        failure_cleanup,
    ))
}

fn result_error_conversion(
    source: NativeType,
    target: NativeType,
    layouts: &LayoutRegistry,
) -> bool {
    let (NativeType::Object(source), NativeType::Object(target)) = (source, target) else {
        return false;
    };
    if source == target {
        return false;
    }
    let LayoutKind::Variant {
        name: source_name,
        alternatives: source_alternatives,
        ..
    } = &layouts.get(source).kind
    else {
        return false;
    };
    let LayoutKind::Variant {
        name: target_name,
        alternatives: target_alternatives,
        ..
    } = &layouts.get(target).kind
    else {
        return false;
    };
    if source_name != "Result" || target_name != "Result" {
        return false;
    }
    let Some(source_error) = source_alternatives
        .iter()
        .find(|alternative| alternative.name == "Error")
    else {
        return false;
    };
    let Some(target_error) = target_alternatives
        .iter()
        .find(|alternative| alternative.name == "Error")
    else {
        return false;
    };
    source_error.payload == target_error.payload
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

fn native_shared_type(ty: ir::Type) -> NativeType {
    match ty {
        ir::Type::Opaque => NativeType::Opaque,
        ir::Type::Unit => NativeType::Unit,
        ir::Type::Bool => NativeType::Bool,
        ir::Type::Int => NativeType::Int,
        ir::Type::Float => NativeType::Float,
        ir::Type::CodePoint => NativeType::CodePoint,
        ir::Type::Byte => NativeType::Byte,
        ir::Type::String => NativeType::String,
        ir::Type::Object(layout) => NativeType::Object(layout),
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

#[derive(Clone, Copy)]
enum ReturnConversion {
    Reference,
    Callable,
    Box,
    Unbox,
    ResultError,
}

impl ReturnConversion {
    fn between(
        source: NativeType,
        target: NativeType,
        environment: NativeIrEnvironment<'_>,
    ) -> Result<Option<Self>, FosterError> {
        Ok(
            if source != target && dereference_native_type(source, environment)? == target {
                Some(Self::Reference)
            } else if callable_conversion(source, target, environment.layouts) {
                Some(Self::Callable)
            } else if let Some(conversion) = erased_conversion(source, target, environment.layouts)
            {
                Some(match conversion {
                    ErasedConversion::Box => Self::Box,
                    ErasedConversion::Unbox => Self::Unbox,
                })
            } else if result_error_conversion(source, target, environment.layouts) {
                Some(Self::ResultError)
            } else {
                None
            },
        )
    }

    fn instruction(self, destination: ir::Value, source: ir::Value) -> ir::Instruction {
        match self {
            // Read and retain the pointee before releasing its local origin.
            Self::Reference => ir::Instruction::Portable(ir::PortableInstruction::Move {
                destination,
                source,
            }),
            Self::Callable => ir::Instruction::WrapCallable {
                destination,
                source,
            },
            Self::Box => ir::Instruction::BoxValue {
                destination,
                source,
            },
            Self::Unbox => ir::Instruction::UnboxValue {
                destination,
                source,
            },
            Self::ResultError => ir::Instruction::ConvertResultError {
                destination,
                source,
            },
        }
    }
}

fn remove_shared_home(
    state: &mut HashMap<u16, ir::Value>,
    storage_hints: &[Option<u16>],
    value: ir::Value,
) -> bool {
    let Some(home) = storage_hints[value.0 as usize] else {
        return true;
    };
    if state.get(&home) == Some(&value) {
        state.remove(&home);
        true
    } else {
        false
    }
}

fn allocate_shared_value(
    value_types: &mut Vec<NativeType>,
    storage_hints: &mut Vec<Option<u16>>,
    ty: NativeType,
) -> ir::Value {
    let value = ir::Value(value_types.len() as u32);
    value_types.push(ty);
    storage_hints.push(None);
    value
}

struct NativeFunctionFacts<'a> {
    reference_homes: &'a HashMap<u16, ir::Value>,
    remote_calls: &'a HashMap<u16, VerifiedRemoteCall>,
}

fn lower_shared_instruction(
    instruction: &ir::Instruction,
    metadata: &BytecodeFunction,
    instance: &SpecializationKey,
    environment: NativeIrEnvironment<'_>,
    value_types: &mut Vec<NativeType>,
    storage_hints: &mut Vec<Option<u16>>,
    facts: NativeFunctionFacts<'_>,
) -> Result<Vec<(ir::Instruction, Vec<ir::Value>)>, FosterError> {
    // Stored callable fields use the erased callable ABI, even when the source
    // expression is a concrete closure. Normalize before retaining it in storage.
    let mut adapted = instruction.clone();
    let mut wrappers = Vec::new();
    let mut prefix = Vec::new();
    if let ir::Instruction::Portable(portable) = &mut adapted {
        let object_layout = |value: ir::Value| match dereference_native_type(
            value_types[value.0 as usize],
            environment,
        )
        .ok()?
        {
            NativeType::Object(layout) => Some(layout),
            _ => None,
        };
        let mut sources = Vec::new();
        match portable {
            ir::PortableInstruction::MakeRecord {
                destination,
                fields,
                ..
            } => {
                if let Some(layout) = object_layout(*destination)
                    && let PhysicalKind::Record {
                        fields: expected, ..
                    } = &environment.physical_layouts.get(layout).kind
                {
                    for ((_, source), field) in fields.iter_mut().zip(expected) {
                        sources.push((source, field.value.pointee));
                    }
                }
            }
            ir::PortableInstruction::MakeVariant {
                destination,
                variant,
                payload,
                ..
            } => {
                if let Some(layout) = object_layout(*destination)
                    && let LayoutKind::Variant { alternatives, .. } =
                        &environment.layouts.get(layout).kind
                    && let Some(tag) = alternatives
                        .iter()
                        .find(|alternative| alternative.variant == *variant)
                        .map(|alternative| alternative.tag)
                    && let PhysicalKind::Variant { alternatives, .. } =
                        &environment.physical_layouts.get(layout).kind
                    && let Some(alternative) = alternatives
                        .iter()
                        .find(|alternative| alternative.tag == tag)
                {
                    for (source, field) in payload.iter_mut().zip(&alternative.fields) {
                        sources.push((source, field.value.pointee));
                    }
                }
            }
            ir::PortableInstruction::MakeList {
                destination,
                elements,
                ..
            } => {
                if let Some(layout) = object_layout(*destination)
                    && let PhysicalKind::Buffer { element, .. } =
                        &environment.physical_layouts.get(layout).kind
                {
                    for source in elements {
                        sources.push((source, element.pointee));
                    }
                }
            }
            ir::PortableInstruction::StoreField {
                object,
                field,
                source,
            } => {
                if let Some(layout) = object_layout(*object)
                    && let PhysicalKind::Record { fields, .. } =
                        &environment.physical_layouts.get(layout).kind
                    && let Some(field) = fields.iter().find(|candidate| candidate.name == *field)
                {
                    sources.push((source, field.value.pointee));
                }
            }
            ir::PortableInstruction::StoreIndex { object, source, .. }
            | ir::PortableInstruction::Push {
                object,
                value: source,
                ..
            } => {
                if let Some(layout) = object_layout(*object)
                    && let PhysicalKind::Buffer { element, .. } =
                        &environment.physical_layouts.get(layout).kind
                {
                    sources.push((source, element.pointee));
                }
            }
            _ => {}
        }
        for (source, layout) in sources {
            if let Some(layout) = layout {
                let expected = NativeType::Object(layout);
                if callable_conversion(
                    value_types[source.0 as usize],
                    expected,
                    environment.layouts,
                ) {
                    let wrapper = allocate_shared_value(value_types, storage_hints, expected);
                    prefix.push((
                        ir::Instruction::WrapCallable {
                            destination: wrapper,
                            source: *source,
                        },
                        Vec::new(),
                    ));
                    wrappers.push(wrapper);
                    *source = wrapper;
                }
            }
        }
    }
    if !wrappers.is_empty() {
        prefix.extend(lower_shared_instruction(
            &adapted,
            metadata,
            instance,
            environment,
            value_types,
            storage_hints,
            facts,
        )?);
        prefix.extend(wrappers.into_iter().map(|value| {
            (
                ir::Instruction::Portable(ir::PortableInstruction::Drop { value }),
                Vec::new(),
            )
        }));
        return Ok(prefix);
    }
    let ty = |value: ir::Value| value_types[value.0 as usize];
    let one = |instruction| vec![(instruction, Vec::new())];
    let ir::Instruction::Portable(portable) = instruction else {
        return Ok(one(instruction.clone()));
    };
    match portable {
        ir::PortableInstruction::LoadConstant {
            destination,
            constant,
        } => {
            let value = match environment.program.constants[usize::from(*constant)] {
                Constant::Unit => ir::Constant::Unit,
                Constant::Bool(value) => ir::Constant::Bool(value),
                Constant::Integer(value) => ir::Constant::Integer(value),
                Constant::Float(value) => ir::Constant::Float(value),
                Constant::CodePoint(value) => ir::Constant::CodePoint(value),
                Constant::String(_) => {
                    ir::Constant::RuntimeString(environment.runtime_string_indices[constant])
                }
                Constant::Symbol(_) => {
                    ir::Constant::RuntimeString(environment.runtime_string_indices[constant])
                }
            };
            Ok(one(ir::Instruction::Constant {
                destination: *destination,
                value,
            }))
        }
        ir::PortableInstruction::CopyOnWrite {
            destination,
            source,
        } => {
            let Some(reference) = storage_hints[source.0 as usize]
                .and_then(|home| facts.reference_homes.get(&home).copied())
            else {
                return Ok(one(instruction.clone()));
            };
            // The SSA home holds a loaded parameter value. Detach through the
            // original address so replacing storage updates the caller as well.
            let reference_type = ty(reference);
            let value_type = ty(*destination);
            let unique = allocate_shared_value(value_types, storage_hints, reference_type);
            Ok(vec![
                (
                    ir::Instruction::Portable(ir::PortableInstruction::CopyOnWrite {
                        destination: unique,
                        source: reference,
                    }),
                    Vec::new(),
                ),
                (
                    ir::Instruction::RuntimeCall {
                        destination: *destination,
                        helper: reference_load_helper(value_type),
                        signature: ir::Signature {
                            parameters: vec![reference_type],
                            result: value_type,
                        },
                        arguments: vec![unique],
                    },
                    Vec::new(),
                ),
            ])
        }
        ir::PortableInstruction::MakeWholeReference {
            destination,
            object,
            ..
        } => {
            if let Some(reference) = storage_hints[object.0 as usize]
                .and_then(|home| facts.reference_homes.get(&home).copied())
            {
                // A reborrow of a reference parameter/capture must keep the caller's
                // address, not return the address of this frame's loaded snapshot.
                Ok(one(ir::Instruction::Portable(
                    ir::PortableInstruction::Move {
                        destination: *destination,
                        source: reference,
                    },
                )))
            } else {
                Ok(one(instruction.clone()))
            }
        }
        ir::PortableInstruction::Move {
            destination,
            source,
        } => {
            let Some(reference) = storage_hints[destination.0 as usize]
                .and_then(|home| facts.reference_homes.get(&home).copied())
            else {
                return Ok(one(instruction.clone()));
            };
            let stored_type = ty(*source);
            let reference_type = ty(reference);
            let stored = allocate_shared_value(value_types, storage_hints, NativeType::Unit);
            Ok(vec![
                (
                    ir::Instruction::RuntimeCall {
                        destination: stored,
                        helper: reference_store_helper(stored_type),
                        signature: ir::Signature {
                            parameters: vec![reference_type, stored_type],
                            result: NativeType::Unit,
                        },
                        arguments: vec![reference, *source],
                    },
                    Vec::new(),
                ),
                (instruction.clone(), Vec::new()),
            ])
        }
        ir::PortableInstruction::Unary {
            destination,
            operator,
            operand,
        } => Ok(one(ir::Instruction::Unary {
            destination: *destination,
            operator: *operator,
            operand: *operand,
        })),
        ir::PortableInstruction::Binary {
            destination,
            operator,
            left,
            right,
        } => {
            let mut result = Vec::new();
            let mut left = *left;
            let mut right = *right;
            for operand in [&mut left, &mut right] {
                let operand_type = value_types[operand.0 as usize];
                if let NativeType::Object(layout) = operand_type
                    && let LayoutKind::Pointer { pointee, .. } =
                        &environment.layouts.get(layout).kind
                {
                    let loaded_type = native_verification_type(
                        environment.program,
                        environment.layouts,
                        pointee,
                        None,
                    )?;
                    let loaded = allocate_shared_value(value_types, storage_hints, loaded_type);
                    result.push((
                        ir::Instruction::RuntimeCall {
                            destination: loaded,
                            helper: reference_load_helper(loaded_type),
                            signature: ir::Signature {
                                parameters: vec![NativeType::Object(layout)],
                                result: loaded_type,
                            },
                            arguments: vec![*operand],
                        },
                        Vec::new(),
                    ));
                    *operand = loaded;
                }
            }
            if value_types[left.0 as usize] == NativeType::Int
                && matches!(
                    value_types[right.0 as usize],
                    NativeType::Byte | NativeType::CodePoint
                )
            {
                let extended = allocate_shared_value(value_types, storage_hints, NativeType::Int);
                result.push((
                    ir::Instruction::IntegerExtend {
                        destination: extended,
                        operand: right,
                    },
                    Vec::new(),
                ));
                right = extended;
            } else if !matches!(operator, BinaryOp::ShiftLeft | BinaryOp::ShiftRight)
                && value_types[right.0 as usize] == NativeType::Int
                && matches!(
                    value_types[left.0 as usize],
                    NativeType::Byte | NativeType::CodePoint
                )
            {
                let extended = allocate_shared_value(value_types, storage_hints, NativeType::Int);
                result.push((
                    ir::Instruction::IntegerExtend {
                        destination: extended,
                        operand: left,
                    },
                    Vec::new(),
                ));
                left = extended;
            }
            result.push((
                ir::Instruction::Binary {
                    destination: *destination,
                    operator: *operator,
                    left,
                    right,
                },
                Vec::new(),
            ));
            Ok(result)
        }
        ir::PortableInstruction::Call {
            destination,
            function,
            specialization,
            arguments,
        } => {
            let mut result = Vec::new();
            let target = environment.instances[&SpecializationKey {
                function: *function,
                substitutions: resolve_specialization(specialization, &instance.substitutions),
            }];
            let (arguments, consumed) = shared_call_arguments(
                arguments,
                &environment.program.functions[function].parameter_modes,
                &environment.function_types[&target].parameters,
                environment,
                value_types,
                storage_hints,
                &mut result,
            )?;
            Ok({
                result.push((
                    ir::Instruction::Call {
                        destination: *destination,
                        function: target,
                        specialization: Vec::new(),
                        arguments,
                    },
                    consumed,
                ));
                result
            })
        }
        ir::PortableInstruction::CallMethod {
            destination,
            receiver,
            function,
            specialization,
            arguments,
        } => {
            let mut sources = vec![*receiver];
            sources.extend(arguments);
            let mut result = Vec::new();
            let target = environment.instances[&SpecializationKey {
                function: *function,
                substitutions: resolve_specialization(specialization, &instance.substitutions),
            }];
            // Method receivers alias the caller's place in the VM, independently
            // of the ordinary parameter mode inferred for `self`. Transfer a
            // retained reference to the native callee, which releases its input.
            let mut modes = environment.program.functions[function]
                .parameter_modes
                .clone();
            modes[0] = ParameterMode::Borrow;
            let (arguments, consumed) = shared_call_arguments(
                &sources,
                &modes,
                &environment.function_types[&target].parameters,
                environment,
                value_types,
                storage_hints,
                &mut result,
            )?;
            result.push((
                ir::Instruction::Call {
                    destination: *destination,
                    function: target,
                    specialization: Vec::new(),
                    arguments,
                },
                consumed,
            ));
            Ok(result)
        }
        ir::PortableInstruction::CallClosure {
            destination,
            function,
            specialization,
            captures,
            arguments,
        } => {
            let mut result = Vec::new();
            let target = environment.instances[&SpecializationKey {
                function: *function,
                substitutions: resolve_specialization(specialization, &instance.substitutions),
            }];
            let expected_captures =
                &environment.function_types[&target].parameters[..captures.len()];
            let (mut lowered, mut consumed) = shared_capture_arguments(
                captures,
                expected_captures,
                environment.layouts,
                value_types,
                storage_hints,
                &mut result,
                &metadata.name,
            )?;
            let (ordinary, ordinary_consumed) = shared_call_arguments(
                arguments,
                &environment.program.functions[function].parameter_modes,
                &environment.function_types[&target].parameters[captures.len()..],
                environment,
                value_types,
                storage_hints,
                &mut result,
            )?;
            lowered.extend(ordinary);
            consumed.extend(ordinary_consumed);
            result.push((
                ir::Instruction::Call {
                    destination: *destination,
                    function: target,
                    specialization: Vec::new(),
                    arguments: lowered,
                },
                consumed,
            ));
            Ok(result)
        }
        ir::PortableInstruction::MakeClosure {
            destination,
            function,
            specialization,
            captures,
        } => {
            let mut result = Vec::new();
            let specialization = resolve_specialization(specialization, &instance.substitutions);
            let target = environment.instances[&SpecializationKey {
                function: *function,
                substitutions: specialization.clone(),
            }];
            let expected_captures =
                &environment.function_types[&target].parameters[..captures.len()];
            let (captures, consumed) = shared_capture_arguments(
                captures,
                expected_captures,
                environment.layouts,
                value_types,
                storage_hints,
                &mut result,
                &metadata.name,
            )?;
            result.push((
                ir::Instruction::Portable(ir::PortableInstruction::MakeClosure {
                    destination: *destination,
                    function: target,
                    specialization: Vec::new(),
                    captures: captures
                        .into_iter()
                        .map(|value| (crate::hir::CaptureMode::Move, value))
                        .collect(),
                }),
                consumed,
            ));
            Ok(result)
        }
        ir::PortableInstruction::CallValue {
            destination,
            callee,
            arguments,
        } => {
            let NativeType::Object(layout) = ty(*callee) else {
                return Err(native_error(format!(
                    "dynamic call in `{}` crosses an erased callable boundary",
                    metadata.name
                )));
            };
            let mut result = Vec::new();
            let (modes, expected) = match &environment.layouts.get(layout).kind {
                LayoutKind::Closure {
                    function: target,
                    specialization,
                    captures,
                } => {
                    let target_instance = environment.instances[&SpecializationKey {
                        function: *target,
                        substitutions: specialization.clone(),
                    }];
                    (
                        environment.program.functions[target]
                            .parameter_modes
                            .as_slice(),
                        environment.function_types[&target_instance].parameters[captures.len()..]
                            .to_vec(),
                    )
                }
                LayoutKind::Builtin {
                    ty:
                        crate::vm::VerificationType::Function {
                            parameters,
                            parameter_modes,
                            ..
                        },
                } => (
                    parameter_modes.as_slice(),
                    parameters
                        .iter()
                        .map(|parameter| {
                            native_verification_type(
                                environment.program,
                                environment.layouts,
                                parameter,
                                None,
                            )
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                ),
                _ => {
                    return Err(native_error(format!(
                        "dynamic call in `{}` requires a callable layout",
                        metadata.name
                    )));
                }
            };
            let (arguments, consumed) = shared_call_arguments(
                arguments,
                modes,
                &expected,
                environment,
                value_types,
                storage_hints,
                &mut result,
            )?;
            result.push((
                ir::Instruction::Portable(ir::PortableInstruction::CallValue {
                    destination: *destination,
                    callee: *callee,
                    arguments,
                }),
                consumed,
            ));
            Ok(result)
        }
        ir::PortableInstruction::MakeRecord {
            destination,
            record,
            type_arguments,
            fields,
        } => Ok(one(ir::Instruction::Portable(
            ir::PortableInstruction::MakeRecord {
                destination: *destination,
                record: *record,
                type_arguments: type_arguments
                    .iter()
                    .map(|ty| ty.specialize(&instance.substitutions))
                    .collect(),
                fields: fields.clone(),
            },
        ))),
        ir::PortableInstruction::MakeVariant {
            destination,
            variant,
            type_arguments,
            payload,
        } => Ok(one(ir::Instruction::Portable(
            ir::PortableInstruction::MakeVariant {
                destination: *destination,
                variant: *variant,
                type_arguments: type_arguments
                    .iter()
                    .map(|ty| ty.specialize(&instance.substitutions))
                    .collect(),
                payload: payload.clone(),
            },
        ))),
        ir::PortableInstruction::MoveOut {
            source,
            by_reference,
            ..
        } => Ok(vec![(
            instruction.clone(),
            if *by_reference { vec![] } else { vec![*source] },
        )]),
        ir::PortableInstruction::Index {
            destination,
            object,
            index,
        } => {
            let helper = match ty(*object) {
                NativeType::String => Some(abi::STRING_GET),
                NativeType::Object(layout)
                    if matches!(
                        environment.physical_layouts.get(layout).kind,
                        PhysicalKind::Buffer { .. } | PhysicalKind::Bytes { .. }
                    ) =>
                {
                    None
                }
                receiver => {
                    return Err(native_error(format!(
                        "native indexing does not support `{receiver:?}`"
                    )));
                }
            };
            let Some(helper) = helper else {
                return Ok(one(instruction.clone()));
            };
            let arguments = vec![*object, *index];
            Ok(one(ir::Instruction::RuntimeCall {
                destination: *destination,
                helper,
                signature: runtime_signature(*destination, &arguments, value_types),
                arguments,
            }))
        }
        ir::PortableInstruction::LoadField {
            destination,
            object,
            field,
            by_reference,
        } if !matches!(ty(*object), NativeType::Object(_)) => {
            if *by_reference {
                return Err(native_error(format!(
                    "native compilation does not support reference field `{field}`"
                )));
            }
            if ty(*object) == NativeType::String && matches!(field.as_str(), "bytes" | "value") {
                let bytes = environment
                    .layouts
                    .builtin(&crate::vm::VerificationType::Bytes)
                    .ok_or_else(|| native_error("String byte storage has no native layout"))?;
                value_types[destination.0 as usize] = NativeType::Object(bytes);
                return Ok(one(ir::Instruction::StringToBytes {
                    destination: *destination,
                    source: *object,
                }));
            }
            if ty(*object) == NativeType::Byte && field == "int" {
                return Ok(one(ir::Instruction::IntegerExtend {
                    destination: *destination,
                    operand: *object,
                }));
            }
            if ty(*object) == NativeType::CodePoint && field == "whitespace?" {
                return Ok(one(ir::Instruction::RuntimeCall {
                    destination: *destination,
                    helper: abi::CODE_POINT_WHITESPACE,
                    signature: ir::Signature {
                        parameters: vec![NativeType::CodePoint],
                        result: NativeType::Bool,
                    },
                    arguments: vec![*object],
                }));
            }
            if ty(*object) == NativeType::CodePoint && field == "string" {
                return Ok(one(ir::Instruction::RuntimeCall {
                    destination: *destination,
                    helper: abi::CODE_POINT_STRING,
                    signature: ir::Signature {
                        parameters: vec![NativeType::CodePoint],
                        result: NativeType::String,
                    },
                    arguments: vec![*object],
                }));
            }
            let helper = native_field_helper(ty(*object), field)?;
            let arguments = vec![*object];
            Ok(one(ir::Instruction::RuntimeCall {
                destination: *destination,
                helper,
                signature: runtime_signature(*destination, &arguments, value_types),
                arguments,
            }))
        }
        ir::PortableInstruction::CallContractMethod {
            destination,
            receiver,
            slot,
            name,
            arguments,
            result_type,
        } => {
            let result_type = result_type.specialize(&instance.substitutions);
            let argument_types = arguments
                .iter()
                .map(|argument| ty(*argument))
                .collect::<Vec<_>>();
            let candidates = contract_candidates(
                *slot,
                dereference_native_type(ty(*receiver), environment)?,
                &argument_types,
                environment,
            )?;
            let mut lowered = Vec::new();
            let (arguments, consumed) = if let Some(first) = candidates.first() {
                let target = &environment.function_types[&first.function];
                let modes = &environment.program.functions[&first.implementation].parameter_modes;
                if target.parameters.len() != arguments.len() + 1
                    || modes.len() != arguments.len() + 1
                {
                    return Err(native_error(format!(
                        "contract implementation for `{name}` has an inconsistent arity"
                    )));
                }
                shared_call_arguments(
                    arguments,
                    &modes[1..],
                    &target.parameters[1..],
                    environment,
                    value_types,
                    storage_hints,
                    &mut lowered,
                )?
            } else {
                (arguments.clone(), Vec::new())
            };
            lowered.push((
                ir::Instruction::Portable(ir::PortableInstruction::CallContractMethod {
                    destination: *destination,
                    receiver: *receiver,
                    slot: *slot,
                    name: name.clone(),
                    arguments,
                    result_type,
                }),
                consumed,
            ));
            Ok(lowered)
        }
        ir::PortableInstruction::Builtin {
            destination,
            builtin,
            arguments,
        } => {
            value_types[destination.0 as usize] =
                native_intrinsic_result_type(*builtin, environment)?;
            if matches!(
                builtin.descriptor().native,
                crate::intrinsics::NativeIntrinsic::Print { .. }
            ) {
                let mut result = Vec::new();
                let mut lowered = Vec::new();
                for argument in arguments {
                    let source = value_types[argument.0 as usize];
                    let pointee = dereference_native_type(source, environment)?;
                    if source != pointee {
                        let loaded = allocate_shared_value(value_types, storage_hints, pointee);
                        result.push((
                            ir::Instruction::RuntimeCall {
                                destination: loaded,
                                helper: reference_load_helper(pointee),
                                signature: ir::Signature {
                                    parameters: vec![source],
                                    result: pointee,
                                },
                                arguments: vec![*argument],
                            },
                            Vec::new(),
                        ));
                        lowered.push(loaded);
                    } else {
                        lowered.push(*argument);
                    }
                }
                result.push((
                    ir::Instruction::Portable(ir::PortableInstruction::Builtin {
                        destination: *destination,
                        builtin: *builtin,
                        arguments: lowered,
                    }),
                    Vec::new(),
                ));
                Ok(result)
            } else {
                Ok(one(instruction.clone()))
            }
        }
        ir::PortableInstruction::SpawnRemote { value, .. } => {
            Ok(vec![(instruction.clone(), vec![*value])])
        }
        ir::PortableInstruction::SpawnRemoteBorrow { source, .. } => {
            if !matches!(ty(*source), NativeType::Object(_)) {
                return Err(native_error(format!(
                    "native borrowed remote state in `{}` must use an object layout",
                    metadata.name
                ))
                .with_help("wrap scalar state in a Foster record before borrowing it remotely"));
            }
            Ok(one(instruction.clone()))
        }
        ir::PortableInstruction::RemoteCall {
            destination,
            remote,
            arguments,
            ..
        } => {
            let target = storage_hints[destination.0 as usize]
                .and_then(|home| facts.remote_calls.get(&home))
                .ok_or_else(|| native_error("remote SSA call has no verified specialization"))?
                .target;
            let modes = arguments.iter().map(|(mode, _)| *mode).collect::<Vec<_>>();
            let sources = arguments
                .iter()
                .map(|(_, value)| *value)
                .collect::<Vec<_>>();
            let mut result = Vec::new();
            let (lowered, consumed) = shared_call_arguments(
                &sources,
                &modes,
                &environment.function_types[&target].parameters[1..],
                environment,
                value_types,
                storage_hints,
                &mut result,
            )?;
            result.push((
                ir::Instruction::Portable(ir::PortableInstruction::RemoteCall {
                    destination: *destination,
                    remote: *remote,
                    function: target,
                    arguments: modes.into_iter().zip(lowered).collect(),
                }),
                consumed,
            ));
            Ok(result)
        }
        ir::PortableInstruction::Assert { condition, message } => {
            Ok(one(ir::Instruction::Assert {
                condition: *condition,
                message: *message,
            }))
        }
        _ => Ok(one(instruction.clone())),
    }
}

fn shared_call_arguments(
    arguments: &[ir::Value],
    modes: &[ParameterMode],
    expected_types: &[NativeType],
    environment: NativeIrEnvironment<'_>,
    value_types: &mut Vec<NativeType>,
    storage_hints: &mut Vec<Option<u16>>,
    instructions: &mut Vec<(ir::Instruction, Vec<ir::Value>)>,
) -> Result<(Vec<ir::Value>, Vec<ir::Value>), FosterError> {
    if arguments.len() != modes.len() || arguments.len() != expected_types.len() {
        return Err(native_error(
            "shared call ownership metadata has the wrong arity",
        ));
    }
    let layouts = environment.layouts;
    let mut lowered = Vec::with_capacity(arguments.len());
    let mut consumed = Vec::new();
    for ((argument, mode), expected) in arguments.iter().zip(modes).zip(expected_types) {
        let source_type = value_types[argument.0 as usize];
        let pointee_type = dereference_native_type(source_type, environment)?;
        let loaded;
        let argument = if *mode == ParameterMode::Borrow
            && source_type != pointee_type
            && source_type != *expected
        {
            loaded = allocate_shared_value(value_types, storage_hints, pointee_type);
            instructions.push((
                ir::Instruction::RuntimeCall {
                    destination: loaded,
                    helper: reference_load_helper(pointee_type),
                    signature: ir::Signature {
                        parameters: vec![source_type],
                        result: pointee_type,
                    },
                    arguments: vec![*argument],
                },
                Vec::new(),
            ));
            &loaded
        } else {
            argument
        };
        let ty = value_types[argument.0 as usize];
        if callable_conversion(ty, *expected, layouts) {
            let callable = allocate_shared_value(value_types, storage_hints, *expected);
            instructions.push((
                ir::Instruction::WrapCallable {
                    destination: callable,
                    source: *argument,
                },
                Vec::new(),
            ));
            lowered.push(callable);
            if *mode == ParameterMode::Consume {
                instructions.push((
                    ir::Instruction::Portable(ir::PortableInstruction::Drop { value: *argument }),
                    Vec::new(),
                ));
            }
            continue;
        }
        if let Some(boxing) = erased_conversion(ty, *expected, layouts) {
            let converted = allocate_shared_value(value_types, storage_hints, *expected);
            instructions.push((
                match boxing {
                    ErasedConversion::Box => ir::Instruction::BoxValue {
                        destination: converted,
                        source: *argument,
                    },
                    ErasedConversion::Unbox => ir::Instruction::UnboxValue {
                        destination: converted,
                        source: *argument,
                    },
                },
                Vec::new(),
            ));
            lowered.push(converted);
            if *mode == ParameterMode::Consume {
                instructions.push((
                    ir::Instruction::Portable(ir::PortableInstruction::Drop { value: *argument }),
                    Vec::new(),
                ));
            }
            continue;
        }
        if *mode == ParameterMode::Borrow
            && matches!(ty, NativeType::Object(_) | NativeType::String)
        {
            let retained = allocate_shared_value(value_types, storage_hints, ty);
            instructions.push((
                ir::Instruction::Portable(ir::PortableInstruction::Move {
                    destination: retained,
                    source: *argument,
                }),
                Vec::new(),
            ));
            lowered.push(retained);
        } else {
            lowered.push(*argument);
            if *mode == ParameterMode::Consume {
                consumed.push(*argument);
            }
        }
    }
    consumed.extend(
        lowered
            .iter()
            .filter(|value| storage_hints[value.0 as usize].is_none()),
    );
    Ok((lowered, consumed))
}

pub(super) fn callable_conversion(
    actual: NativeType,
    expected: NativeType,
    layouts: &LayoutRegistry,
) -> bool {
    let (NativeType::Object(actual), NativeType::Object(expected)) = (actual, expected) else {
        return false;
    };
    matches!(layouts.get(actual).kind, LayoutKind::Closure { .. })
        && matches!(
            layouts.get(expected).kind,
            LayoutKind::Builtin {
                ty: crate::vm::VerificationType::Function { .. }
            }
        )
}

#[derive(Clone, Copy)]
pub(super) enum ErasedConversion {
    Box,
    Unbox,
}

pub(super) fn erased_conversion(
    actual: NativeType,
    expected: NativeType,
    layouts: &LayoutRegistry,
) -> Option<ErasedConversion> {
    let opaque = |ty| {
        matches!(
            ty,
            NativeType::Object(layout) if matches!(layouts.get(layout).kind, LayoutKind::Opaque)
        )
    };
    match (opaque(actual), opaque(expected)) {
        (false, true) => Some(ErasedConversion::Box),
        (true, false) => Some(ErasedConversion::Unbox),
        _ => None,
    }
}

fn shared_capture_arguments(
    captures: &[(crate::hir::CaptureMode, ir::Value)],
    expected_types: &[NativeType],
    layouts: &LayoutRegistry,
    value_types: &mut Vec<NativeType>,
    storage_hints: &mut Vec<Option<u16>>,
    instructions: &mut Vec<(ir::Instruction, Vec<ir::Value>)>,
    function: &str,
) -> Result<(Vec<ir::Value>, Vec<ir::Value>), FosterError> {
    if captures.len() != expected_types.len() {
        return Err(native_error("closure capture ABI has the wrong arity"));
    }
    let mut lowered = Vec::with_capacity(captures.len());
    let mut consumed = Vec::new();
    for ((mode, value), expected) in captures.iter().zip(expected_types) {
        let ty = value_types[value.0 as usize];
        if callable_conversion(ty, *expected, layouts) {
            let callable = allocate_shared_value(value_types, storage_hints, *expected);
            instructions.push((
                ir::Instruction::WrapCallable {
                    destination: callable,
                    source: *value,
                },
                Vec::new(),
            ));
            lowered.push(callable);
            if *mode == crate::hir::CaptureMode::Move {
                instructions.push((
                    ir::Instruction::Portable(ir::PortableInstruction::Drop { value: *value }),
                    Vec::new(),
                ));
            }
            continue;
        }
        if let Some(boxing) = erased_conversion(ty, *expected, layouts) {
            let converted = allocate_shared_value(value_types, storage_hints, *expected);
            instructions.push((
                match boxing {
                    ErasedConversion::Box => ir::Instruction::BoxValue {
                        destination: converted,
                        source: *value,
                    },
                    ErasedConversion::Unbox => ir::Instruction::UnboxValue {
                        destination: converted,
                        source: *value,
                    },
                },
                Vec::new(),
            ));
            lowered.push(converted);
            if *mode == crate::hir::CaptureMode::Move {
                instructions.push((
                    ir::Instruction::Portable(ir::PortableInstruction::Drop { value: *value }),
                    Vec::new(),
                ));
            }
            continue;
        }
        match mode {
            crate::hir::CaptureMode::Move => {
                lowered.push(*value);
                consumed.push(*value);
            }
            crate::hir::CaptureMode::Copy => {
                if matches!(ty, NativeType::Object(_) | NativeType::String) {
                    let retained = allocate_shared_value(value_types, storage_hints, ty);
                    instructions.push((
                        ir::Instruction::Portable(ir::PortableInstruction::Move {
                            destination: retained,
                            source: *value,
                        }),
                        Vec::new(),
                    ));
                    lowered.push(retained);
                } else {
                    lowered.push(*value);
                }
            }
            crate::hir::CaptureMode::Ref => {
                let NativeType::Object(layout) = expected else {
                    return Err(native_error(format!(
                        "native closure `{function}` has a non-reference capture ABI"
                    )));
                };
                let reference =
                    allocate_shared_value(value_types, storage_hints, NativeType::Object(*layout));
                instructions.push((
                    ir::Instruction::Portable(ir::PortableInstruction::MakeWholeReference {
                        destination: reference,
                        pointee_type: crate::vm::VerificationType::Unknown,
                        object: *value,
                    }),
                    Vec::new(),
                ));
                lowered.push(reference);
            }
            crate::hir::CaptureMode::Pending => {
                return Err(native_error(format!(
                    "native closure `{function}` has an unresolved capture mode"
                )));
            }
        }
    }
    consumed.extend(
        lowered
            .iter()
            .filter(|value| storage_hints[value.0 as usize].is_none()),
    );
    Ok((lowered, consumed))
}
