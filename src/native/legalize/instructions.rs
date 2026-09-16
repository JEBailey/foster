//! Per-instruction native legalization.
use crate::native::{
    BinaryOp, Constant, FosterError, FunctionDeclaration, HashMap, LayoutKind, NativeIrEnvironment,
    NativeType, ParameterMode, PhysicalKind, SpecializationKey, VerifiedRemoteCall, abi,
    contract_candidates, dereference_native_type, ir, native_error, native_field_helper,
    native_intrinsic_result_type, native_verification_type, reference_load_helper,
    reference_store_helper, resolve_specialization, runtime_signature,
};

use super::conversions::{
    ErasedConversion, allocate_shared_value, callable_conversion, erased_conversion,
    shared_call_arguments, shared_capture_arguments,
};
pub(super) struct NativeFunctionFacts<'a> {
    pub(super) reference_homes: &'a HashMap<u16, ir::Value>,
    pub(super) remote_calls: &'a HashMap<ir::Value, VerifiedRemoteCall>,
}

pub(super) fn lower_shared_instruction(
    instruction: &ir::Instruction,
    metadata: &FunctionDeclaration,
    instance: &SpecializationKey,
    environment: NativeIrEnvironment<'_>,
    values: &mut ir::ValueBuilder,
    facts: NativeFunctionFacts<'_>,
) -> Result<Vec<(ir::Instruction, Vec<ir::Value>)>, FosterError> {
    // Interface and callable storage may receive concrete values. Normalize
    // their representation before retaining them in an erased storage home.
    let mut adapted = instruction.clone();
    let mut wrappers = Vec::new();
    let mut prefix = Vec::new();
    if let ir::Instruction::Portable(portable) = &mut adapted {
        let object_layout =
            |value: ir::Value| match dereference_native_type(values[value.0 as usize], environment)
                .ok()?
            {
                NativeType::Object(layout) => Some(layout),
                _ => None,
            };
        let mut sources = Vec::new();
        match portable {
            ir::PortableInstruction::MatchPattern { pattern, .. } => {
                let inner = match pattern {
                    crate::hir::Pattern::Spanned { pattern, .. } => pattern.as_mut(),
                    pattern => pattern,
                };
                if let crate::hir::Pattern::IsType { source, .. } = inner {
                    *source = source.specialize(&instance.substitutions);
                }
            }
            ir::PortableInstruction::Move {
                destination,
                source,
            } => {
                if let Some(layout) = object_layout(*destination) {
                    sources.push((source, Some(layout)));
                }
            }
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
                let conversion = if callable_conversion(
                    values[source.0 as usize],
                    expected,
                    environment.layouts,
                ) {
                    Some(true)
                } else if matches!(
                    erased_conversion(values[source.0 as usize], expected, environment.layouts),
                    Some(ErasedConversion::Box)
                ) {
                    Some(false)
                } else {
                    None
                };
                if let Some(callable) = conversion {
                    let wrapper = allocate_shared_value(values, expected);
                    prefix.push((
                        if callable {
                            ir::Instruction::WrapCallable {
                                destination: wrapper,
                                source: *source,
                            }
                        } else {
                            ir::Instruction::BoxValue {
                                destination: wrapper,
                                source: *source,
                            }
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
            values,
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
    if let ir::Instruction::Portable(ir::PortableInstruction::MatchPattern { pattern, .. }) =
        &adapted
        && matches!(pattern.unspanned(), crate::hir::Pattern::IsType { .. })
    {
        return Ok(vec![(adapted, Vec::new())]);
    }
    let ty = |value: ir::Value| values[value.0 as usize];
    let one = |instruction| vec![(instruction, Vec::new())];
    let ir::Instruction::Portable(portable) = instruction else {
        return Ok(one(instruction.clone()));
    };
    match portable {
        ir::PortableInstruction::LoadConstant {
            destination,
            constant,
        } => {
            let value = match environment.program.metadata.constants[usize::from(*constant)] {
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
            let Some(reference) = values
                .hint(source.0 as usize)
                .and_then(|home| facts.reference_homes.get(&home).copied())
            else {
                return Ok(one(instruction.clone()));
            };
            // The SSA home holds a loaded parameter value. Detach through the
            // original address so replacing storage updates the caller as well.
            let reference_type = ty(reference);
            let value_type = ty(*destination);
            let unique = allocate_shared_value(values, reference_type);
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
            if let Some(reference) = values
                .hint(object.0 as usize)
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
            let Some(reference) = values
                .hint(destination.0 as usize)
                .and_then(|home| facts.reference_homes.get(&home).copied())
            else {
                return Ok(one(instruction.clone()));
            };
            let stored_type = ty(*source);
            let reference_type = ty(reference);
            let stored = allocate_shared_value(values, NativeType::Unit);
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
                let operand_type = values[operand.0 as usize];
                if let NativeType::Object(layout) = operand_type
                    && let LayoutKind::Pointer { pointee, .. } =
                        &environment.layouts.get(layout).kind
                {
                    let loaded_type = native_verification_type(
                        &environment.program.metadata,
                        environment.layouts,
                        pointee,
                        None,
                    )?;
                    let loaded = allocate_shared_value(values, loaded_type);
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
            if matches!(
                operator,
                BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide
            ) && values[destination.0 as usize] == NativeType::Int
            {
                for operand in [&mut left, &mut right] {
                    if matches!(
                        values[operand.0 as usize],
                        NativeType::Byte | NativeType::CodePoint
                    ) {
                        let extended = allocate_shared_value(values, NativeType::Int);
                        result.push((
                            ir::Instruction::IntegerExtend {
                                destination: extended,
                                operand: *operand,
                            },
                            Vec::new(),
                        ));
                        *operand = extended;
                    }
                }
            }
            if values[left.0 as usize] == NativeType::Int
                && matches!(
                    values[right.0 as usize],
                    NativeType::Byte | NativeType::CodePoint
                )
            {
                let extended = allocate_shared_value(values, NativeType::Int);
                result.push((
                    ir::Instruction::IntegerExtend {
                        destination: extended,
                        operand: right,
                    },
                    Vec::new(),
                ));
                right = extended;
            } else if !matches!(operator, BinaryOp::ShiftLeft | BinaryOp::ShiftRight)
                && values[right.0 as usize] == NativeType::Int
                && matches!(
                    values[left.0 as usize],
                    NativeType::Byte | NativeType::CodePoint
                )
            {
                let extended = allocate_shared_value(values, NativeType::Int);
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
                values,
                &mut result,
            )?;
            Ok({
                result.push((
                    ir::Instruction::Call {
                        destination: *destination,
                        function: target,
                        specialization: Default::default(),
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
                values,
                &mut result,
            )?;
            result.push((
                ir::Instruction::Call {
                    destination: *destination,
                    function: target,
                    specialization: Default::default(),
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
                values,
                &mut result,
                &metadata.name,
            )?;
            let (ordinary, ordinary_consumed) = shared_call_arguments(
                arguments,
                &environment.program.functions[function].parameter_modes,
                &environment.function_types[&target].parameters[captures.len()..],
                environment,
                values,
                &mut result,
            )?;
            lowered.extend(ordinary);
            consumed.extend(ordinary_consumed);
            result.push((
                ir::Instruction::Call {
                    destination: *destination,
                    function: target,
                    specialization: Default::default(),
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
                values,
                &mut result,
                &metadata.name,
            )?;
            let layout = environment
                .layouts
                .closure_instance(*function, &specialization)
                .ok_or_else(|| native_error("closure construction has no concrete layout"))?;
            let wrapped = callable_conversion(
                NativeType::Object(layout),
                values[destination.0 as usize],
                environment.layouts,
            );
            let concrete = if wrapped {
                allocate_shared_value(values, NativeType::Object(layout))
            } else {
                *destination
            };
            result.push((
                ir::Instruction::Portable(ir::PortableInstruction::MakeClosure {
                    destination: concrete,
                    function: target,
                    specialization: Default::default(),
                    captures: captures
                        .into_iter()
                        .map(|value| (crate::hir::CaptureMode::Move, value))
                        .collect(),
                }),
                consumed,
            ));
            if wrapped {
                result.push((
                    ir::Instruction::WrapCallable {
                        destination: *destination,
                        source: concrete,
                    },
                    Vec::new(),
                ));
                result.push((
                    ir::Instruction::Portable(ir::PortableInstruction::Drop { value: concrete }),
                    Vec::new(),
                ));
            }
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
                            .clone(),
                        environment.function_types[&target_instance].parameters[captures.len()..]
                            .to_vec(),
                    )
                }
                LayoutKind::Builtin {
                    ty: crate::codegen::types::ExecutableType::Function { parameters, .. },
                } => (
                    parameters.iter().map(|p| p.mode).collect::<Vec<_>>(),
                    parameters
                        .iter()
                        .map(|parameter| {
                            native_verification_type(
                                &environment.program.metadata,
                                environment.layouts,
                                &parameter.ty,
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
                &modes,
                &expected,
                environment,
                values,
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
                signature: runtime_signature(*destination, &arguments, values),
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
                    .builtin(&crate::codegen::types::ExecutableType::Bytes)
                    .ok_or_else(|| native_error("String byte storage has no native layout"))?;
                values[destination.0 as usize] = NativeType::Object(bytes);
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
                signature: runtime_signature(*destination, &arguments, values),
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
                    values,
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
            values[destination.0 as usize] = native_intrinsic_result_type(*builtin, environment)?;
            if matches!(
                builtin.descriptor().native,
                crate::intrinsics::NativeIntrinsic::Print { .. }
            ) {
                let mut result = Vec::new();
                let mut lowered = Vec::new();
                for argument in arguments {
                    let source = values[argument.0 as usize];
                    let pointee = dereference_native_type(source, environment)?;
                    if source != pointee {
                        let loaded = allocate_shared_value(values, pointee);
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
            let target = facts
                .remote_calls
                .get(destination)
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
                values,
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
