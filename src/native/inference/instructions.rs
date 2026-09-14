//! Result-layout dependencies and native rules for portable SSA instructions.
use super::{
    dereference_native_type, field_type, native_intrinsic_result_type, native_verification_type,
};
use crate::codegen::types::ExecutableType;
use crate::native::{
    BinaryOp, Constant, FosterError, HashMap, LayoutKind, LayoutRegistry, NativeIrEnvironment,
    NativeType, Pattern, PhysicalKind, PhysicalRegistry, SpecializationKey, UnaryOp,
    VerifiedRemoteCall, executable_type_for_native, ir, native_error,
    native_type_from_value_layout, resolve_specialization,
};
use ir::PortableInstruction as Instruction;
/// Only dependencies that determine the result layout. Shared logical flow
/// checks argument availability; direct calls already have a known result ABI.
pub(super) fn representation_operand(instruction: &Instruction) -> Option<ir::Value> {
    match instruction {
        Instruction::Move { source, .. }
        | Instruction::CopyOnWrite { source, .. }
        | Instruction::MoveOut { source, .. }
        | Instruction::SpawnRemoteBorrow { source, .. } => Some(*source),
        Instruction::Unary {
            operand,
            operator: UnaryOp::Negate,
            ..
        } => Some(*operand),
        Instruction::Binary {
            left,
            operator: BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide,
            ..
        } => Some(*left),
        Instruction::CallValue { callee, .. } => Some(*callee),
        Instruction::LoadField { object, .. }
        | Instruction::Index { object, .. }
        | Instruction::Append { object, .. } => Some(*object),
        Instruction::SpawnRemote { value, .. } => Some(*value),
        Instruction::Await { future, .. } => Some(*future),
        Instruction::CallContractMethod { receiver, .. } => Some(*receiver),
        Instruction::MatchPattern { subject, .. } => Some(*subject),
        Instruction::Drop { .. }
        | Instruction::StoreField { .. }
        | Instruction::StoreIndex { .. }
        | Instruction::Assert { .. }
        | Instruction::LoadConstant { .. }
        | Instruction::Unary { .. }
        | Instruction::Binary { .. }
        | Instruction::MakeList { .. }
        | Instruction::MakeRecord { .. }
        | Instruction::MakeVariant { .. }
        | Instruction::MakeReference { .. }
        | Instruction::MakeWholeReference { .. }
        | Instruction::MakeFieldReference { .. }
        | Instruction::Push { .. }
        | Instruction::Contains { .. }
        | Instruction::Builtin { .. }
        | Instruction::RemoteCall { .. }
        | Instruction::Call { .. }
        | Instruction::CallMethod { .. }
        | Instruction::MakeClosure { .. }
        | Instruction::CallClosure { .. } => None,
    }
}

pub(super) fn infer_instruction(
    instruction: &Instruction,
    function: &ir::Function,
    instance: &SpecializationKey,
    environment: NativeIrEnvironment<'_>,
    remote_calls: &HashMap<ir::Value, VerifiedRemoteCall>,
    result: &mut [Option<NativeType>],
) -> Result<(), FosterError> {
    match instruction {
        Instruction::LoadConstant {
            destination,
            constant,
        } => {
            result[destination.index()] = Some(
                match environment.program.metadata.constants[usize::from(*constant)] {
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
        Instruction::CopyOnWrite {
            destination,
            source,
        }
        | Instruction::Move {
            destination,
            source,
        } => {
            result[destination.index()] = result[source.index()];
        }
        Instruction::Unary {
            destination,
            operator,
            operand,
        } => {
            result[destination.index()] = Some(match operator {
                UnaryOp::Negate => value_type(&result, *operand, function)?,
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
            result[destination.index()] = Some(match operator {
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
                        value_type(&result, *left, function)?,
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
            result[destination.index()] = Some(environment.function_types[&callee].result);
        }
        Instruction::MakeClosure {
            destination,
            function: target,
            specialization,
            ..
        } => {
            let specialization = resolve_specialization(specialization, &instance.substitutions);
            result[destination.index()] = Some(NativeType::Object(
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
            let NativeType::Object(layout) = value_type(&result, *callee, function)? else {
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
                    ty: crate::codegen::types::ExecutableType::Function { result, .. },
                } => native_verification_type(
                    &environment.program.metadata,
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
            result[destination.index()] = Some(result_type);
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
            result[destination.index()] = Some(NativeType::Object(
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
            let parent = environment.program.metadata.variants[variant].parent;
            let arguments = type_arguments
                .iter()
                .map(|ty| ty.specialize(&instance.substitutions))
                .collect::<Vec<_>>();
            result[destination.index()] = Some(NativeType::Object(
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
            let concrete = crate::codegen::types::ExecutableType::List(Box::new(
                element_type.specialize(&instance.substitutions),
            ));
            let layout = environment.layouts.builtin(&concrete).ok_or_else(|| {
                native_error(format!(
                    "list in `{}` has no concrete native layout for `{concrete:?}`",
                    function.name
                ))
            })?;
            result[destination.index()] = Some(NativeType::Object(layout));
        }
        Instruction::LoadField {
            destination,
            object,
            field,
            by_reference,
        } => {
            let object =
                dereference_native_type(value_type(&result, *object, function)?, environment)?;
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
                result[destination.index()] = Some(NativeType::Object(pointer));
                return Ok(());
            }
            result[destination.index()] = Some(
                field_type(
                    &environment.program.metadata,
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
            let object = value_type(&result, *object, function)?;
            result[destination.index()] = Some(match object {
                NativeType::String => NativeType::CodePoint,
                NativeType::Object(layout) => {
                    match &environment.physical_layouts.get(layout).kind {
                        PhysicalKind::Buffer { element, .. } => {
                            match &environment.layouts.get(layout).kind {
                                LayoutKind::Builtin {
                                    ty: ExecutableType::List(item),
                                } => native_verification_type(
                                    &environment.program.metadata,
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
            result[destination.index()] = Some(NativeType::Object(layout));
        }
        Instruction::MoveOut {
            by_reference,
            destination,
            source,
        } => {
            let source_type = value_type(&result, *source, function)?;
            result[destination.index()] = Some(if *by_reference {
                dereference_native_type(source_type, environment)?
            } else {
                source_type
            });
        }
        Instruction::Push { destination, .. } => {
            result[destination.index()] = Some(NativeType::Unit);
        }
        Instruction::Append {
            destination,
            object,
            ..
        } => {
            result[destination.index()] = Some(value_type(&result, *object, function)?);
        }
        Instruction::Contains { destination, .. } => {
            result[destination.index()] = Some(NativeType::Bool);
        }
        Instruction::Builtin {
            destination,
            builtin,
            ..
        } => {
            result[destination.index()] =
                Some(native_intrinsic_result_type(*builtin, environment)?);
        }
        Instruction::SpawnRemote { destination, value }
        | Instruction::SpawnRemoteBorrow {
            destination,
            source: value,
        } => {
            let value = value_type(&result, *value, function)?;
            let remote = ExecutableType::Remote(Box::new(executable_type_for_native(
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
            result[destination.index()] = Some(NativeType::Object(layout));
        }
        Instruction::RemoteCall { destination, .. } => {
            let call = remote_calls
                .get(destination)
                .ok_or_else(|| native_error("remote call has no verified specialization"))?;
            let future = ExecutableType::Future(Box::new(
                environment
                    .program
                    .metadata
                    .remote_outcome_type(call.result.clone()),
            ));
            let layout = environment.layouts.builtin(&future).ok_or_else(|| {
                native_error(format!(
                    "future in `{}` has no concrete native layout",
                    function.name
                ))
            })?;
            result[destination.index()] = Some(NativeType::Object(layout));
        }
        Instruction::Await {
            destination,
            future,
        } => {
            let NativeType::Object(layout) = value_type(&result, *future, function)? else {
                return Err(native_error(format!(
                    "await in `{}` has a non-object future",
                    function.name
                )));
            };
            let LayoutKind::Builtin {
                ty: ExecutableType::Future(value),
            } = &environment.layouts.get(layout).kind
            else {
                return Err(native_error(format!(
                    "await in `{}` does not receive Future<T>",
                    function.name
                )));
            };
            result[destination.index()] = Some(native_verification_type(
                &environment.program.metadata,
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
            let receiver = value_type(&result, *receiver, function)?;
            if *slot == crate::types::CAN_COPY_SLOT || *slot == crate::types::COPY_SLOT {
                result[destination.index()] = Some(if *slot == crate::types::CAN_COPY_SLOT {
                    NativeType::Bool
                } else {
                    receiver
                });
                return Ok(());
            }
            let concrete = result_type.specialize(&instance.substitutions);
            result[destination.index()] = Some(native_verification_type(
                &environment.program.metadata,
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
            result[destination.index()] = Some(NativeType::Bool);
            let subject = value_type(&result, *subject, function)?;
            let mut types = Vec::new();
            native_pattern_binding_types(
                &environment.program.metadata,
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
                result[binding.index()] = Some(ty);
            }
        }
        Instruction::Drop { .. }
        | Instruction::StoreField { .. }
        | Instruction::StoreIndex { .. }
        | Instruction::Assert { .. } => {}
    }

    Ok(())
}

fn native_pattern_binding_types(
    program: &crate::codegen::metadata::ProgramMetadata,
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

fn value_type(
    types: &[Option<NativeType>],
    value: ir::Value,
    function: &ir::Function,
) -> Result<NativeType, FosterError> {
    types[value.index()].ok_or_else(|| {
        native_error(format!(
            "cannot determine the representation of SSA value %{} in `{}`",
            value.index(),
            function.name
        ))
    })
}
