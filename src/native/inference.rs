//! Infer native register types from verified construction instructions.
use super::{
    BinaryOp, BytecodeFunction, Constant, FosterError, HashMap, Instruction, LayoutId, LayoutKind,
    LayoutRegistry, NativeIrEnvironment, NativeType, Pattern, PhysicalKind, PhysicalRegistry,
    Program, Register, SpecializationKey, UnaryOp, VerificationType, VerifiedRemoteCall,
    native_error, native_type_from_value_layout, resolve_specialization,
    verification_type_for_native,
};

pub(super) fn infer_register_types(
    function: &BytecodeFunction,
    parameter_types: &[NativeType],
    instance: &SpecializationKey,
    environment: NativeIrEnvironment<'_>,
    remote_calls: &HashMap<u16, VerifiedRemoteCall>,
) -> Result<Vec<Option<NativeType>>, FosterError> {
    let mut result = vec![None; usize::from(function.registers)];
    let mut definitions = HashMap::new();
    let mut merged = std::collections::HashSet::new();
    for (index, ty) in parameter_types.iter().enumerate() {
        result[index] = Some(*ty);
        definitions.insert(index, *ty);
    }
    for instruction in &function.instructions {
        match instruction {
            Instruction::LoadConstant {
                destination,
                constant,
            } => {
                result[usize::from(destination.0)] = Some(
                    match environment.program.constants[usize::from(*constant)] {
                        Constant::Unit => NativeType::Unit,
                        Constant::Bool(_) => NativeType::Bool,
                        Constant::Integer(_) => NativeType::Int,
                        Constant::Float(_) => NativeType::Float,
                        Constant::CodePoint(_) => NativeType::CodePoint,
                        Constant::String(_) => NativeType::String,
                        Constant::Symbol(_) => NativeType::String,
                    },
                );
            }
            Instruction::Move {
                destination,
                source,
            } => {
                result[usize::from(destination.0)] = result[usize::from(source.0)];
            }
            Instruction::Unary {
                destination,
                operator,
                operand,
            } => {
                result[usize::from(destination.0)] = Some(match operator {
                    UnaryOp::Negate => register_type(&result, *operand, function)?,
                    UnaryOp::Not => NativeType::Bool,
                    UnaryOp::BitNot => NativeType::Byte,
                });
            }
            Instruction::Binary {
                destination,
                operator,
                left,
                ..
            } => {
                result[usize::from(destination.0)] = Some(match operator {
                    BinaryOp::Equal
                    | BinaryOp::NotEqual
                    | BinaryOp::Less
                    | BinaryOp::LessEqual
                    | BinaryOp::Greater
                    | BinaryOp::GreaterEqual => NativeType::Bool,
                    BinaryOp::BitAnd
                    | BinaryOp::BitOr
                    | BinaryOp::BitXor
                    | BinaryOp::ShiftLeft
                    | BinaryOp::ShiftRight => NativeType::Byte,
                    BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide => {
                        match dereference_native_type(
                            register_type(&result, *left, function)?,
                            environment,
                        )? {
                            NativeType::CodePoint | NativeType::Byte => NativeType::Int,
                            ty => ty,
                        }
                    }
                });
            }
            Instruction::Call {
                destination,
                function: callee,
                specialization,
                ..
            }
            | Instruction::CallMethod {
                destination,
                function: callee,
                specialization,
                ..
            }
            | Instruction::CallClosure {
                destination,
                function: callee,
                specialization,
                ..
            } => {
                let callee = environment.instances[&SpecializationKey {
                    function: *callee,
                    substitutions: resolve_specialization(specialization, &instance.substitutions),
                }];
                result[usize::from(destination.0)] =
                    Some(environment.function_types[&callee].result);
            }
            Instruction::MakeClosure {
                destination,
                function: target,
                specialization,
                ..
            } => {
                let specialization =
                    resolve_specialization(specialization, &instance.substitutions);
                result[usize::from(destination.0)] = Some(NativeType::Object(
                    environment
                        .layouts
                        .closure_instance(*target, &specialization)
                        .ok_or_else(|| {
                            native_error(format!(
                                "closure in `{}` has no native layout",
                                function.name
                            ))
                        })?,
                ));
            }
            Instruction::CallValue {
                destination,
                callee,
                ..
            } => {
                let NativeType::Object(layout) = register_type(&result, *callee, function)? else {
                    return Err(native_error(format!(
                        "dynamic call in `{}` has an erased callable representation",
                        function.name
                    )));
                };
                let result_type = match &environment.layouts.get(layout).kind {
                    LayoutKind::Closure {
                        function: target,
                        specialization,
                        ..
                    } => {
                        let target = environment.instances[&SpecializationKey {
                            function: *target,
                            substitutions: specialization.clone(),
                        }];
                        environment.function_types[&target].result
                    }
                    LayoutKind::Builtin {
                        ty: crate::vm::VerificationType::Function { result, .. },
                    } => native_verification_type(
                        environment.program,
                        environment.layouts,
                        result,
                        None,
                    )?,
                    _ => {
                        return Err(native_error(format!(
                            "dynamic call in `{}` does not reference a callable layout",
                            function.name
                        )));
                    }
                };
                result[usize::from(destination.0)] = Some(result_type);
            }
            Instruction::MakeRecord {
                destination,
                record,
                type_arguments,
                ..
            } => {
                let arguments = type_arguments
                    .iter()
                    .map(|ty| ty.specialize(&instance.substitutions))
                    .collect::<Vec<_>>();
                result[usize::from(destination.0)] = Some(NativeType::Object(
                    environment
                        .layouts
                        .record_instance(*record, &arguments)
                        .ok_or_else(|| {
                            native_error(format!(
                                "record in `{}` has no native layout",
                                function.name
                            ))
                        })?,
                ));
            }
            Instruction::MakeVariant {
                destination,
                variant,
                type_arguments,
                ..
            } => {
                let parent = environment.program.variants[variant].parent;
                let arguments = type_arguments
                    .iter()
                    .map(|ty| ty.specialize(&instance.substitutions))
                    .collect::<Vec<_>>();
                result[usize::from(destination.0)] = Some(NativeType::Object(
                    environment
                        .layouts
                        .variant_instance(parent, &arguments)
                        .ok_or_else(|| {
                            native_error(format!(
                                "variant in `{}` has no native layout",
                                function.name
                            ))
                        })?,
                ));
            }
            Instruction::MakeList {
                destination,
                element_type,
                ..
            } => {
                let concrete = crate::vm::VerificationType::List(Box::new(
                    element_type.specialize(&instance.substitutions),
                ));
                let layout = environment.layouts.builtin(&concrete).ok_or_else(|| {
                    native_error(format!(
                        "list in `{}` has no concrete native layout for `{concrete:?}`",
                        function.name
                    ))
                })?;
                result[usize::from(destination.0)] = Some(NativeType::Object(layout));
            }
            Instruction::LoadField {
                destination,
                object,
                field,
                by_reference,
            } => {
                let object = dereference_native_type(
                    register_type(&result, *object, function)?,
                    environment,
                )?;
                if *by_reference {
                    let NativeType::Object(layout) = object else {
                        return Err(native_error("projected field requires a record"));
                    };
                    let LayoutKind::Record { fields, .. } = &environment.layouts.get(layout).kind
                    else {
                        return Err(native_error("projected field requires a record layout"));
                    };
                    let slot = fields
                        .iter()
                        .find(|slot| slot.name == *field)
                        .ok_or_else(|| native_error("projected field has no logical slot"))?;
                    let pointer = environment
                        .layouts
                        .pointer(&slot.ty, crate::codegen::layout::Ownership::Borrowed)
                        .ok_or_else(|| {
                            native_error("projected field has no borrowed pointer layout")
                        })?;
                    result[usize::from(destination.0)] = Some(NativeType::Object(pointer));
                    continue;
                }
                result[usize::from(destination.0)] = Some(
                    field_type(
                        environment.program,
                        environment.layouts,
                        environment.physical_layouts,
                        object,
                        field,
                    )
                    .map_err(|error| {
                        native_error(format!(
                            "{} while lowering field `{field}` in `{}`",
                            error.message, function.name
                        ))
                    })?,
                );
            }
            Instruction::Index {
                destination,
                object,
                ..
            } => {
                let object = register_type(&result, *object, function)?;
                result[usize::from(destination.0)] = Some(match object {
                    NativeType::String => NativeType::CodePoint,
                    NativeType::Object(layout) => {
                        match &environment.physical_layouts.get(layout).kind {
                            PhysicalKind::Buffer { element, .. } => {
                                match &environment.layouts.get(layout).kind {
                                    LayoutKind::Builtin {
                                        ty: VerificationType::List(item),
                                    } => native_verification_type(
                                        environment.program,
                                        environment.layouts,
                                        item,
                                        element.pointee,
                                    )?,
                                    _ => native_type_from_value_layout(*element),
                                }
                            }
                            PhysicalKind::Bytes { .. } => NativeType::Byte,
                            _ => {
                                return Err(native_error(format!(
                                    "native indexing requires bytes or a buffer in `{}`",
                                    function.name
                                )));
                            }
                        }
                    }
                    _ => {
                        return Err(native_error(format!(
                            "native indexing does not support `{object:?}` in `{}`",
                            function.name
                        )));
                    }
                });
            }
            Instruction::MakeReference {
                destination,
                pointee_type,
                ..
            }
            | Instruction::MakeWholeReference {
                destination,
                pointee_type,
                ..
            }
            | Instruction::MakeFieldReference {
                destination,
                pointee_type,
                ..
            } => {
                let pointee = pointee_type.specialize(&instance.substitutions);
                let layout = environment
                    .layouts
                    .pointer(&pointee, crate::codegen::layout::Ownership::Borrowed)
                    .ok_or_else(|| native_error("reference has no concrete native layout"))?;
                result[usize::from(destination.0)] = Some(NativeType::Object(layout));
            }
            Instruction::MoveOut {
                by_reference,
                destination,
                source,
            } => {
                let source_type = register_type(&result, *source, function)?;
                result[usize::from(destination.0)] = Some(if *by_reference {
                    dereference_native_type(source_type, environment)?
                } else {
                    source_type
                });
            }
            Instruction::Push { destination, .. } => {
                result[usize::from(destination.0)] = Some(NativeType::Unit);
            }
            Instruction::Append {
                destination,
                object,
                ..
            } => {
                result[usize::from(destination.0)] =
                    Some(register_type(&result, *object, function)?);
            }
            Instruction::Contains { destination, .. } => {
                result[usize::from(destination.0)] = Some(NativeType::Bool);
            }
            Instruction::Builtin {
                destination,
                builtin,
                ..
            } => {
                result[usize::from(destination.0)] =
                    Some(native_intrinsic_result_type(*builtin, environment)?);
            }
            Instruction::SpawnRemote { destination, value }
            | Instruction::SpawnRemoteBorrow {
                destination,
                source: value,
            } => {
                let value = register_type(&result, *value, function)?;
                let remote = VerificationType::Remote(Box::new(verification_type_for_native(
                    value,
                    environment.program,
                    environment.layouts,
                )));
                let layout = environment.layouts.builtin(&remote).ok_or_else(|| {
                    native_error(format!(
                        "remote value in `{}` has no concrete native layout",
                        function.name
                    ))
                })?;
                result[usize::from(destination.0)] = Some(NativeType::Object(layout));
            }
            Instruction::RemoteCall { destination, .. } => {
                let call = remote_calls
                    .get(&destination.0)
                    .ok_or_else(|| native_error("remote call has no verified specialization"))?;
                let future = VerificationType::Future(Box::new(
                    environment.program.remote_outcome_type(call.result.clone()),
                ));
                let layout = environment.layouts.builtin(&future).ok_or_else(|| {
                    native_error(format!(
                        "future in `{}` has no concrete native layout",
                        function.name
                    ))
                })?;
                result[usize::from(destination.0)] = Some(NativeType::Object(layout));
            }
            Instruction::Await {
                destination,
                future,
            } => {
                let NativeType::Object(layout) = register_type(&result, *future, function)? else {
                    return Err(native_error(format!(
                        "await in `{}` has a non-object future",
                        function.name
                    )));
                };
                let LayoutKind::Builtin {
                    ty: VerificationType::Future(value),
                } = &environment.layouts.get(layout).kind
                else {
                    return Err(native_error(format!(
                        "await in `{}` does not receive Future<T>",
                        function.name
                    )));
                };
                result[usize::from(destination.0)] = Some(native_verification_type(
                    environment.program,
                    environment.layouts,
                    value,
                    None,
                )?);
            }
            Instruction::CallContractMethod {
                destination,
                receiver,
                slot,
                result_type,
                ..
            } => {
                let receiver = register_type(&result, *receiver, function)?;
                if *slot == crate::types::CAN_COPY_SLOT || *slot == crate::types::COPY_SLOT {
                    result[usize::from(destination.0)] =
                        Some(if *slot == crate::types::CAN_COPY_SLOT {
                            NativeType::Bool
                        } else {
                            receiver
                        });
                    continue;
                }
                let concrete = result_type.specialize(&instance.substitutions);
                result[usize::from(destination.0)] = Some(native_verification_type(
                    environment.program,
                    environment.layouts,
                    &concrete,
                    None,
                )?);
            }
            Instruction::MatchPattern {
                destination,
                subject,
                pattern,
                bindings,
            } => {
                result[usize::from(destination.0)] = Some(NativeType::Bool);
                let subject = register_type(&result, *subject, function)?;
                let mut types = Vec::new();
                native_pattern_binding_types(
                    environment.program,
                    environment.layouts,
                    environment.physical_layouts,
                    pattern,
                    subject,
                    &mut types,
                )?;
                if types.len() != bindings.len() {
                    return Err(native_error(format!(
                        "native pattern in `{}` has inconsistent binding metadata",
                        function.name
                    )));
                }
                for (binding, ty) in bindings.iter().zip(types) {
                    result[usize::from(binding.0)] = Some(ty);
                }
            }
            _ => {}
        }
        let destination = match instruction {
            Instruction::MakeClosure { destination, .. }
            | Instruction::Move { destination, .. }
            | Instruction::Call { destination, .. }
            | Instruction::CallMethod { destination, .. }
            | Instruction::CallClosure { destination, .. }
            | Instruction::CallValue { destination, .. }
            | Instruction::LoadField { destination, .. } => Some(usize::from(destination.0)),
            _ => None,
        };
        if let Some(destination) = destination
            && let Some(ty) = result[destination]
            && definitions
                .insert(destination, ty)
                .is_some_and(|previous| previous != ty)
        {
            merged.insert(destination);
        }
    }
    let mut aliases = HashMap::<usize, Vec<usize>>::new();
    for instruction in &function.instructions {
        if let Instruction::Move {
            destination,
            source,
        } = instruction
        {
            aliases
                .entry(usize::from(source.0))
                .or_default()
                .push(usize::from(destination.0));
        }
    }
    let mut pending = merged.iter().copied().collect::<Vec<_>>();
    while let Some(source) = pending.pop() {
        for &destination in aliases.get(&source).into_iter().flatten() {
            if merged.insert(destination) {
                pending.push(destination);
            }
        }
    }
    // A storage home may receive different closures on different control-flow paths.
    // Keep its signature stable; concrete environments are local to MakeClosure.
    for home in merged {
        let Some(ty) = result[home].as_mut() else {
            continue;
        };
        if let NativeType::Object(layout) = *ty
            && let LayoutKind::Closure {
                function,
                specialization,
                ..
            } = &environment.layouts.get(layout).kind
        {
            let function = &environment.program.functions[function];
            let callable = VerificationType::Function {
                parameters: function.parameter_types.clone(),
                parameter_modes: function.parameter_modes.clone(),
                result: Box::new(function.result_type.clone()),
            }
            .specialize(specialization);
            *ty = native_verification_type(
                environment.program,
                environment.layouts,
                &callable,
                None,
            )?;
        }
    }
    Ok(result)
}

fn native_pattern_binding_types(
    program: &Program,
    layouts: &LayoutRegistry,
    physical_layouts: &PhysicalRegistry,
    pattern: &Pattern,
    subject: NativeType,
    bindings: &mut Vec<NativeType>,
) -> Result<(), FosterError> {
    match pattern.unspanned() {
        Pattern::Binding(_) => bindings.push(subject),
        Pattern::Variant { variant, fields } => {
            let parent = program.variants[variant].parent;
            let NativeType::Object(layout) = subject else {
                return Err(native_error("pattern subject uses the wrong native layout"));
            };
            let LayoutKind::Variant {
                variant_type,
                alternatives,
                ..
            } = &layouts.get(layout).kind
            else {
                unreachable!()
            };
            if *variant_type != parent {
                return Err(native_error(
                    "pattern subject uses the wrong nominal layout",
                ));
            }
            let alternative = alternatives
                .iter()
                .find(|alternative| alternative.variant == *variant)
                .ok_or_else(|| native_error("pattern alternative has no logical layout"))?;
            let physical = physical_layouts
                .variant_alternative(layout, alternative.tag)
                .ok_or_else(|| native_error("pattern alternative has no physical layout"))?;
            for ((pattern, ty), field) in fields
                .iter()
                .zip(&alternative.payload)
                .zip(&physical.fields)
            {
                let ty = native_verification_type(program, layouts, ty, field.value.pointee)?;
                native_pattern_binding_types(
                    program,
                    layouts,
                    physical_layouts,
                    pattern,
                    ty,
                    bindings,
                )?;
            }
        }
        Pattern::Spanned { .. } => unreachable!(),
        Pattern::Wildcard
        | Pattern::Bool(_)
        | Pattern::Integer(_)
        | Pattern::Float(_)
        | Pattern::String(_)
        | Pattern::CodePoint(_)
        | Pattern::Symbol(_) => {}
    }
    Ok(())
}

fn register_type(
    types: &[Option<NativeType>],
    register: Register,
    function: &BytecodeFunction,
) -> Result<NativeType, FosterError> {
    types[usize::from(register.0)].ok_or_else(|| {
        native_error(format!(
            "cannot determine the type of register r{} in `{}`",
            register.0, function.name
        ))
    })
}

pub(super) fn dereference_native_type(
    ty: NativeType,
    environment: NativeIrEnvironment<'_>,
) -> Result<NativeType, FosterError> {
    let NativeType::Object(layout) = ty else {
        return Ok(ty);
    };
    let LayoutKind::Pointer { pointee, .. } = &environment.layouts.get(layout).kind else {
        return Ok(ty);
    };
    native_verification_type(environment.program, environment.layouts, pointee, None)
}

pub(super) fn field_type(
    program: &Program,
    layouts: &LayoutRegistry,
    physical_layouts: &PhysicalRegistry,
    receiver: NativeType,
    field: &str,
) -> Result<NativeType, FosterError> {
    match (receiver, field) {
        (NativeType::String, "empty?") => Ok(NativeType::Bool),
        (NativeType::String, "length") => Ok(NativeType::Int),
        (NativeType::String, "head") => Ok(NativeType::String),
        (NativeType::String, "rest") => Ok(NativeType::String),
        (NativeType::String, "whitespace?") => Ok(NativeType::Bool),
        (NativeType::String, "bytes" | "value") => layouts
            .builtin(&crate::vm::VerificationType::Bytes)
            .map(NativeType::Object)
            .ok_or_else(|| native_error("String byte storage has no native layout")),
        (NativeType::Byte, "int") => Ok(NativeType::Int),
        (NativeType::CodePoint, "whitespace?") => Ok(NativeType::Bool),
        (NativeType::CodePoint, "string") => Ok(NativeType::String),
        (NativeType::Object(layout), field) => match &layouts.get(layout).kind {
            LayoutKind::Record { fields, .. } => {
                let slot = fields
                    .iter()
                    .find(|slot| slot.name == field)
                    .ok_or_else(|| {
                        native_error(format!(
                            "native record l{} has no field `{field}`",
                            layout.0
                        ))
                    })?;
                let physical = physical_layouts
                    .record_field(layout, slot.index)
                    .ok_or_else(|| native_error("logical and physical record fields disagree"))?;
                native_verification_type(program, layouts, &slot.ty, physical.value.pointee)
            }
            LayoutKind::Builtin {
                ty: crate::vm::VerificationType::List(element),
            } => match field {
                "empty?" => Ok(NativeType::Bool),
                "length" => Ok(NativeType::Int),
                "head" => native_verification_type(
                    program,
                    layouts,
                    element,
                    match physical_layouts.get(layout).kind {
                        PhysicalKind::Buffer { element, .. } => element.pointee,
                        _ => None,
                    },
                ),
                "rest" => Ok(NativeType::Object(layout)),
                _ => Err(native_error(format!("native list has no field `{field}`"))),
            },
            LayoutKind::Builtin {
                ty: crate::vm::VerificationType::Bytes,
            } => match field {
                "empty?" => Ok(NativeType::Bool),
                "length" => Ok(NativeType::Int),
                "head" => Ok(NativeType::Byte),
                "rest" => Ok(NativeType::Object(layout)),
                _ => Err(native_error(format!("native Bytes has no field `{field}`"))),
            },
            LayoutKind::Builtin {
                ty: crate::vm::VerificationType::ByteBuffer,
            } => match field {
                "empty?" => Ok(NativeType::Bool),
                "length" | "capacity" => Ok(NativeType::Int),
                _ => Err(native_error(format!(
                    "native ByteBuffer has no field `{field}`"
                ))),
            },
            LayoutKind::Pointer { pointee, .. } => field_type(
                program,
                layouts,
                physical_layouts,
                native_verification_type(program, layouts, pointee, None)?,
                field,
            ),
            _ => Err(native_error(format!(
                "native field access requires a record or list, found l{}",
                layout.0
            ))),
        },
        _ => Err(native_error(format!(
            "native compilation does not support field `{field}` on `{receiver:?}`"
        ))),
    }
}

pub(super) fn native_verification_type(
    program: &Program,
    layouts: &LayoutRegistry,
    ty: &crate::vm::VerificationType,
    physical_pointee: Option<LayoutId>,
) -> Result<NativeType, FosterError> {
    use crate::vm::VerificationType;
    match ty {
        VerificationType::Unit => Ok(NativeType::Unit),
        VerificationType::Bool => Ok(NativeType::Bool),
        VerificationType::Integer => Ok(NativeType::Int),
        VerificationType::Float => Ok(NativeType::Float),
        VerificationType::CodePoint => Ok(NativeType::CodePoint),
        VerificationType::Byte => Ok(NativeType::Byte),
        VerificationType::Record { record, .. } if Some(*record) == program.string_record => {
            Ok(NativeType::String)
        }
        VerificationType::Record { record, .. } if Some(*record) == program.symbol_record => {
            Ok(NativeType::String)
        }
        VerificationType::Record { record, arguments } => layouts
            .record_instance(*record, arguments)
            .or(physical_pointee)
            .map(NativeType::Object)
            .ok_or_else(|| native_error("record field has no native layout")),
        VerificationType::Variant { variant, arguments } => layouts
            .variant_instance(*variant, arguments)
            .or(physical_pointee)
            .map(NativeType::Object)
            .ok_or_else(|| native_error("variant field has no native layout")),
        VerificationType::List(_)
        | VerificationType::Bytes
        | VerificationType::ByteBuffer
        | VerificationType::Remote(_)
        | VerificationType::Future(_)
        | VerificationType::Function { .. } => layouts
            .builtin(ty)
            .or(physical_pointee)
            .map(NativeType::Object)
            .ok_or_else(|| native_error("builtin value has no native layout")),
        VerificationType::Reference(pointee) => layouts
            .pointer(pointee, crate::codegen::layout::Ownership::Borrowed)
            .or(physical_pointee)
            .map(NativeType::Object)
            .ok_or_else(|| native_error("reference has no native layout")),
        VerificationType::Unknown | VerificationType::Union(_) => {
            Ok(NativeType::Object(layouts.opaque()))
        }
        VerificationType::Generic(name) => Err(native_error(format!(
            "unresolved generic `{name}` has no native representation"
        ))),
    }
}

fn native_intrinsic_type(
    ty: crate::intrinsics::IntrinsicType,
    layouts: &LayoutRegistry,
) -> Result<NativeType, FosterError> {
    use crate::intrinsics::IntrinsicType;
    use crate::vm::VerificationType;
    match ty {
        IntrinsicType::Unit => Ok(NativeType::Unit),
        IntrinsicType::Bool => Ok(NativeType::Bool),
        IntrinsicType::Integer => Ok(NativeType::Int),
        IntrinsicType::Float => Ok(NativeType::Float),
        IntrinsicType::CodePoint => Ok(NativeType::CodePoint),
        IntrinsicType::Byte => Ok(NativeType::Byte),
        IntrinsicType::String => Ok(NativeType::String),
        IntrinsicType::Bytes => layouts
            .builtin(&VerificationType::Bytes)
            .map(NativeType::Object)
            .ok_or_else(|| native_error("Bytes intrinsic type has no native layout")),
        IntrinsicType::ByteBuffer => layouts
            .builtin(&VerificationType::ByteBuffer)
            .map(NativeType::Object)
            .ok_or_else(|| native_error("ByteBuffer intrinsic type has no native layout")),
        IntrinsicType::ListByte => layouts
            .builtin(&VerificationType::List(Box::new(VerificationType::Byte)))
            .map(NativeType::Object)
            .ok_or_else(|| native_error("List<Byte> intrinsic type has no native layout")),
        IntrinsicType::Any => Err(native_error(
            "erased intrinsic type does not define a native ABI",
        )),
    }
}

pub(super) fn native_intrinsic_result_type(
    builtin: crate::intrinsics::Builtin,
    environment: NativeIrEnvironment<'_>,
) -> Result<NativeType, FosterError> {
    if builtin.descriptor().signature.result != crate::intrinsics::IntrinsicType::Any {
        return native_intrinsic_type(builtin.descriptor().signature.result, environment.layouts);
    }
    let ty = environment
        .builtin_result_types
        .get(&builtin)
        .ok_or_else(|| {
            native_error(format!(
                "native intrinsic `{builtin:?}` has no concrete result type"
            ))
        })?;
    native_verification_type(environment.program, environment.layouts, ty, None)
}
