//! Value, remote-operation, and control-edge transfer rules.
use super::calls::verify_arguments;
use super::state::{
    FlowState, PendingPattern, assignment_binding, bound_type, read_type, take_type, write_type,
};
use super::types::merge_types;
use super::types::{
    binary_type, constant_type, fully_covered_variant, indexed_element_type,
    intrinsic_verification_type, invalid_instruction, is_foster_byte_buffer, nominal_record,
    pattern_irrefutable, record_type, require_type, type_error, unary_type,
    verification_field_type,
};
use super::{Body, Instruction, Program, StoragePolicy};
use crate::codegen::{ir::Value, types::ExecutableType};
use crate::error::FosterError;
use crate::intrinsics::IntrinsicArgumentMode;

fn pattern_binding_types(
    program: &Program,
    pattern: &crate::hir::Pattern,
    subject: &ExecutableType,
    bindings: &mut Vec<ExecutableType>,
) -> Result<(), FosterError> {
    use crate::hir::Pattern;
    match pattern.unspanned() {
        Pattern::Binding(_) => bindings.push(subject.clone()),
        Pattern::IsType {
            binding: Some(_), ..
        } => bindings.push(ExecutableType::Unknown),
        Pattern::Record { fields } => {
            let subject = match subject {
                ExecutableType::Reference(value) => value.as_ref(),
                value => value,
            };
            if !matches!(
                subject,
                ExecutableType::Record { .. } | ExecutableType::Unknown
            ) {
                return Err(FosterError::runtime(
                    "record pattern requires a record subject",
                ));
            }
            let mut names = std::collections::HashSet::new();
            for (name, field) in fields {
                if !names.insert(name) {
                    return Err(FosterError::runtime("duplicate field in record pattern"));
                }
                let ty = verification_field_type(program, subject, name).ok_or_else(|| {
                    FosterError::runtime(format!("record pattern selects missing field `{name}`"))
                })?;
                pattern_binding_types(program, field, &ty, bindings)?;
            }
        }
        Pattern::Variant { variant, fields } => {
            let metadata =
                program.metadata.variants.get(variant).ok_or_else(|| {
                    FosterError::runtime("pattern references a missing enum case")
                })?;
            if fields.len() != metadata.payload.len() {
                return Err(FosterError::runtime(
                    "pattern has the wrong number of enum payload fields",
                ));
            }
            let substitutions = match subject {
                ExecutableType::Variant { arguments, .. } => metadata
                    .parameters
                    .iter()
                    .cloned()
                    .zip(arguments.iter().cloned())
                    .collect(),
                _ => std::collections::HashMap::new(),
            };
            for (field, schema) in fields.iter().zip(&metadata.payload) {
                pattern_binding_types(
                    program,
                    field,
                    &schema.substitute(&substitutions),
                    bindings,
                )?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn transfer(
    program: &Program,
    function: &Body,
    index: usize,
    mut state: FlowState,
) -> Result<Vec<(usize, FlowState)>, FosterError> {
    let instruction = &function.instructions[index];
    let next = index + 1;
    // Drop insertion can place cleanup between a pattern test and its conditional branch.
    // Preserve the edge fact until the corresponding condition is consumed.
    let pattern = state.pending_pattern.clone();

    // Operations that can mutate aliased storage invalidate literal evidence.
    if matches!(
        instruction,
        Instruction::StoreField { .. }
            | Instruction::StoreIndex { .. }
            | Instruction::Builtin { .. }
            | Instruction::Await { .. }
            | Instruction::RemoteCall { .. }
            | Instruction::MakeWholeReference { .. }
            | Instruction::MakeFieldReference { .. }
            | Instruction::MakeReference { .. }
            | Instruction::MoveOut { .. }
            | Instruction::CopyOnWrite { .. }
            | Instruction::LoadField {
                by_reference: true,
                ..
            }
    ) {
        state.boolean_constants.clear();
    }
    match instruction {
        Instruction::Unreachable => return Ok(Vec::new()),
        Instruction::CopyOnWrite {
            destination,
            source,
        } => {
            state.bindings[destination.index()] = state.bindings[source.index()].clone();
        }
        Instruction::Edge { target, arguments } => {
            let old = state.clone();
            for (source, destination) in arguments {
                state.bindings[destination.index()] = old.bindings[source.index()].clone();
                state.boolean_constants.remove(destination);
                if let Some(value) = old.boolean_constants.get(source) {
                    state.boolean_constants.insert(*destination, *value);
                }
                state.excluded_variants.remove(destination);
                if let Some(excluded) = old.excluded_variants.get(source) {
                    state
                        .excluded_variants
                        .insert(*destination, excluded.clone());
                }
            }
            if let Some(pattern) = &mut state.pending_pattern {
                let rename = |values: &[Value]| {
                    let mut aliases = Vec::new();
                    for value in values {
                        if !arguments
                            .iter()
                            .any(|(_, destination)| destination == value)
                        {
                            aliases.push(*value);
                        }
                        aliases.extend(arguments.iter().filter_map(|(source, destination)| {
                            (source == value).then_some(*destination)
                        }));
                    }
                    aliases.sort_unstable();
                    aliases.dedup();
                    aliases
                };
                pattern.conditions = rename(&pattern.conditions);
                pattern.bindings = pattern
                    .bindings
                    .iter()
                    .flat_map(|(value, ty)| {
                        rename(&[*value])
                            .into_iter()
                            .map(|value| (value, ty.clone()))
                    })
                    .collect();
                if let Some((subjects, _)) = &mut pattern.refined_subject {
                    *subjects = rename(subjects);
                }
                if let Some((subjects, _)) = &mut pattern.covered_variant {
                    *subjects = rename(subjects);
                }
            }
            return Ok(vec![(*target, state)]);
        }
        Instruction::Drop { register } => {
            // Liveness drops are deliberately idempotent. A consuming call can empty a
            // register before the cleanup instruction on that edge executes.
            if function.storage_policy == StoragePolicy::ConsumingSlots {
                state.bindings[register.index()] = None;
            }
            state.boolean_constants.remove(register);
            state.excluded_variants.remove(register);
        }
        Instruction::LoadConstant {
            destination,
            constant,
        } => {
            let constant = &program.metadata.constants[usize::from(*constant)];
            write_type(
                function,
                index,
                &mut state,
                *destination,
                constant_type(program, constant),
            )?;
            if let crate::codegen::metadata::Constant::Bool(value) = constant
                && state.bindings[destination.index()] == Some(ExecutableType::Bool)
            {
                state.boolean_constants.insert(*destination, *value);
            }
        }
        Instruction::Move {
            destination,
            source,
        } => {
            let ty = if matches!(
                assignment_binding(function, &state, *destination),
                Some(ExecutableType::Reference(_))
            ) {
                read_type(function, index, &state, *source)?
            } else {
                bound_type(function, index, &state, *source)?
            };
            write_type(function, index, &mut state, *destination, ty)?;
        }
        Instruction::Unary {
            destination,
            operator,
            operand,
        } => {
            let operand = read_type(function, index, &state, *operand)?;
            let ty = unary_type(function, index, *operator, operand)?;
            write_type(function, index, &mut state, *destination, ty)?;
        }
        Instruction::Binary {
            destination,
            operator,
            left,
            right,
        } => {
            let left = read_type(function, index, &state, *left)?;
            let right = read_type(function, index, &state, *right)?;
            let ty = binary_type(function, index, *operator, left, right)?;
            write_type(function, index, &mut state, *destination, ty)?;
        }
        Instruction::MakeList {
            destination,
            element_type,
            elements,
        } => {
            let mut element = element_type.clone();
            for register in elements {
                let found = read_type(function, index, &state, *register)?;
                element = merge_types(function, index, &element, &found)?;
            }
            write_type(
                function,
                index,
                &mut state,
                *destination,
                ExecutableType::List(Box::new(element)),
            )?;
        }
        Instruction::Index {
            destination,
            object,
            index: subscript,
        } => {
            let subscript = read_type(function, index, &state, *subscript)?;
            require_type(
                function,
                index,
                &subscript,
                &ExecutableType::Integer,
                "index",
            )?;
            let object_type = read_type(function, index, &state, *object)?;
            let result = match object_type {
                ExecutableType::List(element) => *element,
                ExecutableType::Bytes | ExecutableType::ByteBuffer => ExecutableType::Byte,
                ExecutableType::Unknown => ExecutableType::Unknown,
                ExecutableType::Record { .. } if is_foster_byte_buffer(program, &object_type) => {
                    ExecutableType::Byte
                }
                found => return type_error(function, index, "indexable value", &found),
            };
            write_type(function, index, &mut state, *destination, result)?;
        }
        Instruction::MakeRecord {
            destination,
            record,
            fields,
            type_arguments,
        } => {
            for (_, register) in fields {
                read_type(function, index, &state, *register)?;
            }
            write_type(
                function,
                index,
                &mut state,
                *destination,
                match record_type(program, *record) {
                    ExecutableType::Record { record, .. } => ExecutableType::Record {
                        record,
                        arguments: type_arguments.clone(),
                    },
                    ty => ty,
                },
            )?;
        }
        Instruction::MakeVariant {
            destination,
            variant,
            payload,
            type_arguments,
        } => {
            for register in payload {
                read_type(function, index, &state, *register)?;
            }
            write_type(
                function,
                index,
                &mut state,
                *destination,
                ExecutableType::Variant {
                    variant: program.metadata.variants[variant].parent,
                    arguments: type_arguments.clone(),
                },
            )?;
        }
        Instruction::LoadField {
            destination,
            object,
            by_reference,
            ..
        } => {
            read_type(function, index, &state, *object)?;
            let ty = if *by_reference {
                ExecutableType::Reference(Box::new(ExecutableType::Unknown))
            } else {
                ExecutableType::Unknown
            };
            write_type(function, index, &mut state, *destination, ty)?;
        }
        Instruction::StoreField { object, source, .. } => {
            let object_type = read_type(function, index, &state, *object)?;
            if !matches!(
                object_type,
                ExecutableType::Record { .. }
                    | ExecutableType::List(_)
                    | ExecutableType::Bytes
                    | ExecutableType::ByteBuffer
                    | ExecutableType::Unknown
            ) {
                return type_error(function, index, "record", &object_type);
            }
            read_type(function, index, &state, *source)?;
        }
        Instruction::StoreIndex {
            object,
            index: subscript,
            source,
        } => {
            let subscript = read_type(function, index, &state, *subscript)?;
            require_type(
                function,
                index,
                &subscript,
                &ExecutableType::Integer,
                "index",
            )?;
            let source_type = read_type(function, index, &state, *source)?;
            let object_type = read_type(function, index, &state, *object)?;
            match object_type {
                ExecutableType::List(element) => {
                    require_type(function, index, &source_type, &element, "list element")?
                }
                ExecutableType::ByteBuffer => require_type(
                    function,
                    index,
                    &source_type,
                    &ExecutableType::Byte,
                    "byte-buffer element",
                )?,
                ExecutableType::Record { .. } if is_foster_byte_buffer(program, &object_type) => {
                    require_type(
                        function,
                        index,
                        &source_type,
                        &ExecutableType::Byte,
                        "byte-buffer element",
                    )?
                }
                ExecutableType::Unknown => {}
                found => return type_error(function, index, "mutable indexed value", &found),
            }
        }
        Instruction::MakeReference {
            destination,
            pointee_type,
            object,
            index: subscript,
        } => {
            let subscript = read_type(function, index, &state, *subscript)?;
            require_type(
                function,
                index,
                &subscript,
                &ExecutableType::Integer,
                "index",
            )?;
            let object_type = read_type(function, index, &state, *object)?;
            let inferred = indexed_element_type(program, &object_type).ok_or_else(|| {
                FosterError::runtime(format!(
                    "bytecode function `{}` instruction {index} has referenceable indexed value type {object_type:?}",
                    function.name
                ))
            })?;
            require_type(
                function,
                index,
                &inferred,
                pointee_type,
                "reference pointee",
            )?;
            write_type(
                function,
                index,
                &mut state,
                *destination,
                ExecutableType::Reference(Box::new(pointee_type.clone())),
            )?;
        }
        Instruction::MakeWholeReference {
            destination,
            pointee_type,
            object,
        } => {
            let value = read_type(function, index, &state, *object)?;
            let inferred = match value {
                ExecutableType::Reference(pointee) => *pointee,
                value => value,
            };
            require_type(
                function,
                index,
                &inferred,
                pointee_type,
                "reference pointee",
            )?;
            write_type(
                function,
                index,
                &mut state,
                *destination,
                ExecutableType::Reference(Box::new(pointee_type.clone())),
            )?;
        }
        Instruction::MakeFieldReference {
            destination,
            pointee_type,
            object,
            field,
        } => {
            let object = read_type(function, index, &state, *object)?;
            let inferred = verification_field_type(program, &object, field).ok_or_else(|| {
                FosterError::runtime(format!(
                    "bytecode function `{}` instruction {index} references missing field `{field}`",
                    function.name
                ))
            })?;
            require_type(
                function,
                index,
                &inferred,
                pointee_type,
                "reference pointee",
            )?;
            write_type(
                function,
                index,
                &mut state,
                *destination,
                ExecutableType::Reference(Box::new(pointee_type.clone())),
            )?;
        }
        Instruction::MoveOut {
            by_reference,
            destination,
            source,
        } => {
            let ty = if *by_reference {
                match bound_type(function, index, &state, *source)? {
                    ExecutableType::Reference(pointee) => *pointee,
                    _ => {
                        return invalid_instruction(
                            function,
                            index,
                            "projected move requires a reference",
                        );
                    }
                }
            } else {
                take_type(function, index, &mut state, *source)?
            };
            write_type(function, index, &mut state, *destination, ty)?;
        }
        Instruction::Push {
            destination,
            object,
            value,
        } => {
            let value = read_type(function, index, &state, *value)?;
            let object_type = read_type(function, index, &state, *object)?;
            match object_type {
                ExecutableType::List(element) => {
                    require_type(function, index, &value, &element, "list element")?
                }
                ExecutableType::ByteBuffer => require_type(
                    function,
                    index,
                    &value,
                    &ExecutableType::Byte,
                    "byte-buffer element",
                )?,
                ExecutableType::Record { .. } if is_foster_byte_buffer(program, &object_type) => {
                    require_type(
                        function,
                        index,
                        &value,
                        &ExecutableType::Byte,
                        "byte-buffer element",
                    )?
                }
                ExecutableType::Unknown => {}
                found => return type_error(function, index, "List or ByteBuffer", &found),
            }
            write_type(
                function,
                index,
                &mut state,
                *destination,
                ExecutableType::Unit,
            )?;
        }
        Instruction::Append {
            destination,
            object,
            value,
        } => {
            let value = read_type(function, index, &state, *value)?;
            let result = match read_type(function, index, &state, *object)? {
                ExecutableType::List(element) => {
                    require_type(function, index, &value, &element, "list element")?;
                    ExecutableType::List(element)
                }
                ExecutableType::Unknown => ExecutableType::Unknown,
                found => return type_error(function, index, "List", &found),
            };
            write_type(function, index, &mut state, *destination, result)?;
        }
        Instruction::Contains {
            destination,
            value,
            candidates,
        } => {
            let value = read_type(function, index, &state, *value)?;
            for candidate in candidates {
                let candidate = read_type(function, index, &state, *candidate)?;
                require_type(function, index, &candidate, &value, "candidate")?;
            }
            write_type(
                function,
                index,
                &mut state,
                *destination,
                ExecutableType::Bool,
            )?;
        }
        Instruction::Builtin {
            destination,
            builtin,
            arguments,
        } => {
            let signature = builtin.descriptor().signature;
            if !signature.accepts_arity(arguments.len()) {
                return invalid_instruction(function, index, "has invalid builtin arity");
            }
            for (argument_index, argument) in arguments.iter().enumerate() {
                let parameter = signature
                    .parameter(argument_index)
                    .expect("accepted builtin arity has a parameter");
                let expected = intrinsic_verification_type(program, parameter.ty);
                let found = read_type(function, index, &state, *argument)?;
                require_type(function, index, &found, &expected, "builtin argument")?;
            }
            for (argument_index, argument) in arguments.iter().enumerate() {
                let parameter = signature
                    .parameter(argument_index)
                    .expect("accepted builtin arity has a parameter");
                if parameter.mode == IntrinsicArgumentMode::Consume {
                    take_type(function, index, &mut state, *argument)?;
                }
            }
            write_type(
                function,
                index,
                &mut state,
                *destination,
                intrinsic_verification_type(program, signature.result),
            )?;
        }
        Instruction::SpawnRemote { destination, value } => {
            let value = read_type(function, index, &state, *value)?;
            write_type(
                function,
                index,
                &mut state,
                *destination,
                ExecutableType::Remote(Box::new(value)),
            )?;
        }
        Instruction::SpawnRemoteBorrow {
            destination,
            source,
        } => {
            let value = read_type(function, index, &state, *source)?;
            write_type(
                function,
                index,
                &mut state,
                *destination,
                ExecutableType::Remote(Box::new(value)),
            )?;
        }
        Instruction::RemoteCall {
            destination,
            remote,
            function: target,
            arguments,
        } => {
            let target = &program.functions[target];
            let receiver = match read_type(function, index, &state, *remote)? {
                ExecutableType::Remote(value) => *value,
                ExecutableType::Unknown => ExecutableType::Unknown,
                found => return type_error(function, index, "Remote", &found),
            };
            let mut substitutions = std::collections::BTreeMap::new();
            target.parameters[0]
                .ty
                .infer_specialization(&receiver, &mut substitutions);
            for (schema, (_, argument)) in target
                .parameters
                .iter()
                .map(|p| &p.ty)
                .skip(1)
                .zip(arguments)
            {
                schema.infer_specialization(
                    &read_type(function, index, &state, *argument)?,
                    &mut substitutions,
                );
            }
            let specialization = substitutions.into();
            let parameter_types = target
                .parameters
                .iter()
                .map(|p| &p.ty)
                .map(|ty| ty.specialize(&specialization))
                .collect::<Vec<_>>();
            require_type(
                function,
                index,
                &receiver,
                &parameter_types[0],
                "remote receiver",
            )?;
            verify_arguments(
                function,
                index,
                &mut state,
                arguments.iter().map(|(mode, register)| (*mode, *register)),
                target.parameters.iter().map(|p| p.mode).skip(1),
                parameter_types.iter().skip(1),
            )?;
            write_type(
                function,
                index,
                &mut state,
                *destination,
                ExecutableType::Future(Box::new(
                    program
                        .metadata
                        .remote_outcome_type(target.result_type.specialize(&specialization)),
                )),
            )?;
        }
        Instruction::Await {
            destination,
            future,
        } => {
            let result = match read_type(function, index, &state, *future)? {
                ExecutableType::Future(value) => *value,
                ExecutableType::Unknown => ExecutableType::Unknown,
                found => return type_error(function, index, "Future", &found),
            };
            write_type(function, index, &mut state, *destination, result)?;
        }
        Instruction::MatchPattern {
            destination,
            subject,
            bindings,
            pattern,
        } => {
            if let crate::hir::Pattern::IsType { target, .. } = pattern.unspanned() {
                let supported = match target {
                    ExecutableType::Never
                    | ExecutableType::Unit
                    | ExecutableType::Bool
                    | ExecutableType::Integer
                    | ExecutableType::Float
                    | ExecutableType::Byte
                    | ExecutableType::CodePoint
                    | ExecutableType::Bytes => true,
                    ExecutableType::Record { record, arguments } => {
                        arguments.is_empty() && program.metadata.records.contains_key(record)
                    }
                    ExecutableType::Variant { variant, arguments } => {
                        arguments.is_empty()
                            && program
                                .metadata
                                .variants
                                .values()
                                .any(|case| case.parent == *variant)
                    }
                    _ => false,
                };
                if !supported {
                    return invalid_instruction(
                        function,
                        index,
                        "unsupported runtime type-pattern target",
                    );
                }
            }
            let subject_type = read_type(function, index, &state, *subject)?;
            let covered_variant = fully_covered_variant(pattern).map(|variant| (*subject, variant));
            let exhaustive = covered_variant.is_some_and(|(_, variant)| {
                let parent = program.metadata.variants[&variant].parent;
                program
                    .metadata
                    .variants
                    .iter()
                    .filter(|(_, metadata)| metadata.parent == parent)
                    .all(|(candidate, _)| {
                        *candidate == variant
                            || state
                                .excluded_variants
                                .get(subject)
                                .is_some_and(|excluded| excluded.contains(candidate))
                    })
            });
            if let crate::hir::Pattern::Variant { variant, .. } = pattern.unspanned()
                && let ExecutableType::Variant {
                    variant: parent, ..
                } = subject_type
                && program.metadata.variants[variant].parent != parent
            {
                return invalid_instruction(
                    function,
                    index,
                    "matches a variant case against the wrong enum type",
                );
            }
            write_type(
                function,
                index,
                &mut state,
                *destination,
                ExecutableType::Bool,
            )?;
            let mut binding_types = Vec::new();
            pattern_binding_types(program, pattern, &subject_type, &mut binding_types)?;
            state.pending_pattern = Some(PendingPattern {
                conditions: vec![*destination],
                refined_subject: match pattern.unspanned() {
                    crate::hir::Pattern::IsType {
                        target,
                        binding: None,
                        ..
                    } => Some((vec![*subject], target.clone())),
                    _ => None,
                },
                bindings: bindings.iter().copied().zip(binding_types).collect(),
                irrefutable: pattern_irrefutable(pattern) || exhaustive,
                covered_variant: covered_variant.map(|(subject, variant)| (vec![subject], variant)),
            });
        }
        Instruction::Jump { target } => return Ok(vec![(*target, state)]),
        Instruction::JumpIfFalse { condition, target } => {
            let found = read_type(function, index, &state, *condition)?;
            require_type(function, index, &found, &ExecutableType::Bool, "condition")?;
            if let Some(value) = state.boolean_constants.get(condition) {
                return Ok(vec![(if *value { next } else { *target }, state)]);
            }
            let mut truthy = state.clone();
            state.pending_pattern = None;
            truthy.pending_pattern = None;
            let pattern = pattern.filter(|pattern| pattern.conditions.contains(condition));
            if let Some(pattern) = &pattern {
                if let Some((subjects, _target)) = &pattern.refined_subject {
                    for subject in subjects {
                        // A structural view can have a different concrete descriptor.
                        // Preserve reference storage while exposing its erased view.
                        let view = ExecutableType::Unknown;
                        let view = if matches!(
                            truthy.bindings[subject.index()],
                            Some(ExecutableType::Reference(_))
                        ) {
                            ExecutableType::Reference(Box::new(view))
                        } else {
                            view
                        };
                        truthy.bindings[subject.index()] = Some(view);
                    }
                }
                for (binding, ty) in &pattern.bindings {
                    write_type(function, index, &mut truthy, *binding, ty.clone())?;
                }
                if let Some((subjects, variant)) = &pattern.covered_variant {
                    for subject in subjects {
                        state
                            .excluded_variants
                            .entry(*subject)
                            .or_default()
                            .insert(*variant);
                    }
                }
            }
            return if pattern.is_some_and(|pattern| pattern.irrefutable) {
                Ok(vec![(next, truthy)])
            } else {
                Ok(vec![(*target, state), (next, truthy)])
            };
        }
        Instruction::Assert { condition, message } => {
            let found = read_type(function, index, &state, *condition)?;
            require_type(function, index, &found, &ExecutableType::Bool, "condition")?;
            if let Some(message) = message {
                let expected = program
                    .metadata
                    .string_record
                    .map(nominal_record)
                    .unwrap_or(ExecutableType::Unknown);
                let found = read_type(function, index, &state, *message)?;
                require_type(function, index, &found, &expected, "assertion message")?;
            }
            if state.boolean_constants.get(condition) == Some(&false) {
                return Ok(Vec::new());
            }
        }
        Instruction::Call { .. }
        | Instruction::CallMethod { .. }
        | Instruction::CallContractMethod { .. }
        | Instruction::MakeClosure { .. }
        | Instruction::CallValue { .. }
        | Instruction::CallClosure { .. }
        | Instruction::Return { .. } => {
            return super::calls::transfer(program, function, index, state);
        }
    }
    Ok(vec![(next, state)])
}
