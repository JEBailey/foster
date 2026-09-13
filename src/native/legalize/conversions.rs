//! Callable, erased-value, return, argument, and capture conversions.
use crate::native::{
    FosterError, LayoutKind, LayoutRegistry, NativeIrEnvironment, NativeType, ParameterMode,
    dereference_native_type, ir, native_error, reference_load_helper,
};

pub(super) fn result_error_conversion(
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

#[derive(Clone, Copy)]
pub(super) enum ReturnConversion {
    Reference,
    Callable,
    Box,
    Unbox,
    ResultError,
}

impl ReturnConversion {
    pub(super) fn between(
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

    pub(super) fn instruction(self, destination: ir::Value, source: ir::Value) -> ir::Instruction {
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

pub(super) fn allocate_shared_value(values: &mut ir::ValueBuilder, ty: NativeType) -> ir::Value {
    values.allocate(ty, None)
}

pub(super) fn shared_call_arguments(
    arguments: &[ir::Value],
    modes: &[ParameterMode],
    expected_types: &[NativeType],
    environment: NativeIrEnvironment<'_>,
    values: &mut ir::ValueBuilder,
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
        let source_type = values[argument.0 as usize];
        let pointee_type = dereference_native_type(source_type, environment)?;
        let loaded;
        let argument = if *mode == ParameterMode::Borrow
            && source_type != pointee_type
            && source_type != *expected
        {
            loaded = allocate_shared_value(values, pointee_type);
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
        let ty = values[argument.0 as usize];
        if callable_conversion(ty, *expected, layouts) {
            let callable = allocate_shared_value(values, *expected);
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
            let converted = allocate_shared_value(values, *expected);
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
            let retained = allocate_shared_value(values, ty);
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
            .filter(|value| values.hint(value.0 as usize).is_none()),
    );
    Ok((lowered, consumed))
}

pub(in crate::native) fn callable_conversion(
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
                ty: crate::codegen::types::ExecutableType::Function { .. }
            }
        )
}

#[derive(Clone, Copy)]
pub(in crate::native) enum ErasedConversion {
    Box,
    Unbox,
}

pub(in crate::native) fn erased_conversion(
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

pub(super) fn shared_capture_arguments(
    captures: &[(crate::hir::CaptureMode, ir::Value)],
    expected_types: &[NativeType],
    layouts: &LayoutRegistry,
    values: &mut ir::ValueBuilder,
    instructions: &mut Vec<(ir::Instruction, Vec<ir::Value>)>,
    function: &str,
) -> Result<(Vec<ir::Value>, Vec<ir::Value>), FosterError> {
    if captures.len() != expected_types.len() {
        return Err(native_error("closure capture ABI has the wrong arity"));
    }
    let mut lowered = Vec::with_capacity(captures.len());
    let mut consumed = Vec::new();
    for ((mode, value), expected) in captures.iter().zip(expected_types) {
        let ty = values[value.0 as usize];
        if callable_conversion(ty, *expected, layouts) {
            let callable = allocate_shared_value(values, *expected);
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
            let converted = allocate_shared_value(values, *expected);
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
                    let retained = allocate_shared_value(values, ty);
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
                let reference = allocate_shared_value(values, NativeType::Object(*layout));
                instructions.push((
                    ir::Instruction::Portable(ir::PortableInstruction::MakeWholeReference {
                        destination: reference,
                        pointee_type: crate::codegen::types::ExecutableType::Unknown,
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
            .filter(|value| values.hint(value.0 as usize).is_none()),
    );
    Ok((lowered, consumed))
}
