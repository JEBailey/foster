//! Logical-to-native type and layout conversion.
use super::{
    BytecodeFunction, Compilation, FosterError, HashMap, Instruction, LayoutRegistry, NativeType,
    Program, Register, SpecializationKey, Type, TypeId, native_error, resolve_specialization,
};

pub(super) fn native_builtin_result_types(
    compilation: &Compilation,
) -> Result<HashMap<crate::intrinsics::Builtin, crate::vm::VerificationType>, FosterError> {
    let mut result = HashMap::new();
    for (function, declaration) in compilation.hir.functions.iter() {
        let Some(builtin) = declaration
            .intrinsic
            .as_deref()
            .and_then(crate::intrinsics::Intrinsic::from_key)
            .and_then(crate::intrinsics::Intrinsic::builtin)
        else {
            continue;
        };
        if builtin.descriptor().native != crate::intrinsics::NativeIntrinsic::Host {
            continue;
        }
        let signature = compilation.types.function_type(function).ok_or_else(|| {
            native_error(format!(
                "native host intrinsic `{builtin:?}` is missing type information"
            ))
        })?;
        let ty = specialized_verification_type(compilation, signature.result, &Vec::new(), 0)?;
        result.insert(builtin, ty);
    }
    Ok(result)
}

pub(super) fn instruction_layout_type(
    program: &Program,
    instruction: &Instruction,
    specialization: &crate::vm::Specialization,
) -> Option<crate::vm::VerificationType> {
    use crate::vm::VerificationType;
    match instruction {
        Instruction::CallContractMethod { result_type, .. } => {
            Some(result_type.specialize(specialization))
        }

        Instruction::MakeRecord {
            record,
            type_arguments,
            ..
        } => Some(VerificationType::Record {
            record: *record,
            arguments: type_arguments
                .iter()
                .map(|ty| ty.specialize(specialization))
                .collect(),
        }),
        Instruction::MakeVariant {
            variant,
            type_arguments,
            ..
        } => Some(VerificationType::Variant {
            variant: program.variants[variant].parent,
            arguments: type_arguments
                .iter()
                .map(|ty| ty.specialize(specialization))
                .collect(),
        }),
        Instruction::MakeList { element_type, .. } => Some(VerificationType::List(Box::new(
            element_type.specialize(specialization),
        ))),
        Instruction::MakeReference { pointee_type, .. }
        | Instruction::MakeWholeReference { pointee_type, .. }
        | Instruction::MakeFieldReference { pointee_type, .. } => Some(
            VerificationType::Reference(Box::new(pointee_type.specialize(specialization))),
        ),
        _ => None,
    }
}

pub(super) fn concrete_closure_result(
    program: &Program,
    layouts: &mut LayoutRegistry,
    instance: &SpecializationKey,
) -> Result<NativeType, FosterError> {
    let body = &program.functions[&instance.function];
    let mut result = None;
    for (index, instruction) in body.instructions.iter().enumerate() {
        let Instruction::Return { source } = instruction else {
            continue;
        };
        let key = closure_definition_before(body, index, *source, &instance.substitutions)
            .ok_or_else(|| {
                native_error(format!(
                    "native function `{}` returns an erased callable value",
                    body.name
                ))
                .with_help(
                    "return one statically known closure, or keep this explicitly dynamic call on the VM",
                )
            })?;
        if result.as_ref().is_some_and(|previous| previous != &key) {
            return Err(native_error(format!(
                "native function `{}` returns multiple concrete closure layouts",
                body.name
            ))
            .with_help("use the VM for a callable value selected dynamically"));
        }
        result = Some(key);
    }
    let key = result.ok_or_else(|| {
        native_error(format!(
            "native function `{}` has no concrete closure result",
            body.name
        ))
    })?;
    Ok(NativeType::Object(
        layouts.instantiate_closure(key.function, &key.substitutions)?,
    ))
}

fn closure_definition_before(
    function: &BytecodeFunction,
    before: usize,
    register: Register,
    outer: &crate::vm::Specialization,
) -> Option<SpecializationKey> {
    for (index, instruction) in function.instructions[..before].iter().enumerate().rev() {
        match instruction {
            Instruction::MakeClosure {
                destination,
                function,
                specialization,
                ..
            } if *destination == register => {
                return Some(SpecializationKey {
                    function: *function,
                    substitutions: resolve_specialization(specialization, outer),
                });
            }
            Instruction::Move {
                destination,
                source,
            } if *destination == register => {
                return closure_definition_before(function, index, *source, outer);
            }
            _ => {}
        }
    }
    None
}

pub(super) fn native_type(
    compilation: &Compilation,
    layouts: &mut LayoutRegistry,
    ty: TypeId,
    substitutions: &crate::vm::Specialization,
    function: &str,
) -> Result<NativeType, FosterError> {
    if let Type::Generic(name) = &compilation.types.types[ty] {
        let concrete = substitutions
            .iter()
            .find_map(|(candidate, ty)| (candidate == name).then_some(ty))
            .ok_or_else(|| {
                native_error(format!(
                    "native specialization of `{function}` does not resolve generic `{name}`"
                ))
            })?;
        layouts.instantiate_type(concrete)?;
        return concrete_native_type(compilation, layouts, concrete, function);
    }
    match compilation.types.types[ty] {
        Type::Unit => Ok(NativeType::Unit),
        Type::Bool => Ok(NativeType::Bool),
        Type::Int => Ok(NativeType::Int),
        Type::Float => Ok(NativeType::Float),
        Type::CodePoint => Ok(NativeType::CodePoint),
        Type::Byte => Ok(NativeType::Byte),
        Type::Record {
            record,
            ref arguments,
        } if Some(record) == compilation.types.core.string => Ok(NativeType::String),
        Type::Record { record, .. } if Some(record) == compilation.types.core.symbol => {
            Ok(NativeType::String)
        }
        Type::Record { record, .. } if record_uses_dynamic_dispatch(compilation, record) => {
            Ok(NativeType::Object(layouts.opaque()))
        }
        Type::Record { record, .. } if Some(record) == compilation.types.core.bytes => {
            let concrete = crate::vm::VerificationType::Bytes;
            layouts.instantiate_type(&concrete)?;
            layouts
                .builtin(&concrete)
                .map(NativeType::Object)
                .ok_or_else(|| native_error(format!("Bytes type in `{function}` has no layout")))
        }
        Type::Record {
            record,
            ref arguments,
        } if Some(record) == compilation.types.core.list && arguments.len() == 1 => {
            let element =
                specialized_verification_type(compilation, arguments[0], substitutions, 0)?;
            let concrete = crate::vm::VerificationType::List(Box::new(element));
            layouts.instantiate_type(&concrete)?;
            layouts
                .builtin(&concrete)
                .map(NativeType::Object)
                .ok_or_else(|| native_error(format!("list type in `{function}` has no layout")))
        }
        Type::Record {
            record,
            ref arguments,
        } => {
            let arguments = arguments
                .iter()
                .map(|ty| specialized_verification_type(compilation, *ty, substitutions, 0))
                .collect::<Result<Vec<_>, _>>()?;
            let concrete = crate::vm::VerificationType::Record {
                record,
                arguments: arguments.clone(),
            };
            layouts.instantiate_type(&concrete)?;
            layouts
                .record_instance(record, &arguments)
                .map(NativeType::Object)
                .ok_or_else(|| native_error(format!("record type in `{function}` has no layout")))
        }
        Type::Variant {
            variant,
            ref arguments,
        } if compilation.hir.variant_types[variant].kind == crate::ast::VariantKind::Alias => {
            let members = arguments
                .iter()
                .map(|ty| specialized_verification_type(compilation, *ty, substitutions, 0))
                .collect::<Result<Vec<_>, _>>()?;
            layouts.instantiate_type(&crate::vm::VerificationType::Union(members))?;
            Ok(NativeType::Object(layouts.opaque()))
        }
        Type::Variant {
            variant,
            ref arguments,
        } => {
            let arguments = arguments
                .iter()
                .map(|ty| specialized_verification_type(compilation, *ty, substitutions, 0))
                .collect::<Result<Vec<_>, _>>()?;
            let concrete = crate::vm::VerificationType::Variant {
                variant,
                arguments: arguments.clone(),
            };
            layouts.instantiate_type(&concrete)?;
            layouts
                .variant_instance(variant, &arguments)
                .map(NativeType::Object)
                .ok_or_else(|| native_error(format!("variant type in `{function}` has no layout")))
        }
        Type::RawBytes
        | Type::RawByteBuffer
        | Type::Reference { .. }
        | Type::RawList(_)
        | Type::Sequence(_)
        | Type::Remote(_)
        | Type::Future(_)
        | Type::Function(_)
        | Type::Intersection(_) => {
            let concrete = specialized_verification_type(compilation, ty, substitutions, 0)?;
            concrete_native_type(compilation, layouts, &concrete, function)
        }
        ref unsupported => Err(native_error(format!(
            "native compilation of `{function}` does not yet support type `{}` ({unsupported:?})",
            compilation.types.display(ty)
        ))
        .with_help("use `foster build` without `--native` for the complete VM language")),
    }
}

pub(super) fn specialized_verification_type(
    compilation: &Compilation,
    ty: TypeId,
    substitutions: &crate::vm::Specialization,
    depth: usize,
) -> Result<crate::vm::VerificationType, FosterError> {
    use crate::vm::VerificationType;
    if depth >= 64 {
        return Err(native_error(
            "native specialization type nesting exceeds 64 levels",
        ));
    }
    let nested = |ty| specialized_verification_type(compilation, ty, substitutions, depth + 1);
    Ok(match &compilation.types.types[ty] {
        Type::Generic(name) => substitutions
            .iter()
            .find_map(|(candidate, ty)| (candidate == name).then(|| ty.clone()))
            .unwrap_or_else(|| VerificationType::Generic(name.clone())),
        Type::Unit => VerificationType::Unit,
        Type::Bool => VerificationType::Bool,
        Type::Int => VerificationType::Integer,
        Type::Float => VerificationType::Float,
        Type::CodePoint => VerificationType::CodePoint,
        Type::Byte => VerificationType::Byte,
        Type::RawBytes => VerificationType::Bytes,
        Type::RawByteBuffer => VerificationType::ByteBuffer,
        Type::Reference { value, .. } => VerificationType::Reference(Box::new(nested(*value)?)),
        Type::RawList(value) => VerificationType::List(Box::new(nested(*value)?)),
        // Sequence is a behavioral view, not a promise of list storage.
        Type::Sequence(_) => VerificationType::Unknown,
        Type::Remote(value) => VerificationType::Remote(Box::new(nested(*value)?)),
        Type::Future(value) => VerificationType::Future(Box::new(nested(*value)?)),
        Type::Function(function) => VerificationType::Function {
            parameters: function
                .parameters
                .iter()
                .map(|ty| nested(*ty))
                .collect::<Result<_, _>>()?,
            parameter_modes: function.parameter_modes.clone(),
            result: Box::new(nested(function.result)?),
        },
        Type::Record { record, .. } if Some(*record) == compilation.types.core.bytes => {
            VerificationType::Bytes
        }
        Type::Record { record, arguments }
            if Some(*record) == compilation.types.core.list && arguments.len() == 1 =>
        {
            VerificationType::List(Box::new(nested(arguments[0])?))
        }
        Type::Record { record, arguments } => VerificationType::Record {
            record: *record,
            arguments: arguments
                .iter()
                .map(|ty| nested(*ty))
                .collect::<Result<_, _>>()?,
        },
        Type::Variant { variant, arguments }
            if compilation.hir.variant_types[*variant].kind == crate::ast::VariantKind::Alias =>
        {
            VerificationType::Union(
                arguments
                    .iter()
                    .map(|ty| nested(*ty))
                    .collect::<Result<_, _>>()?,
            )
        }
        Type::Variant { variant, arguments } => VerificationType::Variant {
            variant: *variant,
            arguments: arguments
                .iter()
                .map(|ty| nested(*ty))
                .collect::<Result<_, _>>()?,
        },
        Type::Intersection(members) => VerificationType::Union(
            members
                .iter()
                .map(|ty| nested(*ty))
                .collect::<Result<_, _>>()?,
        ),
        Type::Module(_) => VerificationType::Unknown,
    })
}

pub(super) fn concrete_native_type(
    compilation: &Compilation,
    layouts: &mut LayoutRegistry,
    ty: &crate::vm::VerificationType,
    function: &str,
) -> Result<NativeType, FosterError> {
    use crate::vm::VerificationType;
    match ty {
        VerificationType::Unit => Ok(NativeType::Unit),
        VerificationType::Bool => Ok(NativeType::Bool),
        VerificationType::Integer => Ok(NativeType::Int),
        VerificationType::Float => Ok(NativeType::Float),
        VerificationType::CodePoint => Ok(NativeType::CodePoint),
        VerificationType::Byte => Ok(NativeType::Byte),
        VerificationType::Record { record, .. }
            if Some(*record) == compilation.types.core.string =>
        {
            Ok(NativeType::String)
        }
        VerificationType::Record { record, .. }
            if record_uses_dynamic_dispatch(compilation, *record) =>
        {
            Ok(NativeType::Object(layouts.opaque()))
        }
        VerificationType::Record { record, .. }
            if Some(*record) == compilation.types.core.symbol =>
        {
            Ok(NativeType::String)
        }
        VerificationType::Record { record, .. }
            if Some(*record) == compilation.types.core.bytes =>
        {
            layouts.instantiate_type(&VerificationType::Bytes)?;
            layouts
                .builtin(&VerificationType::Bytes)
                .map(NativeType::Object)
                .ok_or_else(|| native_error(format!("Bytes type in `{function}` has no layout")))
        }
        VerificationType::Record { record, arguments } => {
            layouts.instantiate_type(ty)?;
            layouts
                .record_instance(*record, arguments)
                .map(NativeType::Object)
                .ok_or_else(|| native_error(format!("record type in `{function}` has no layout")))
        }
        VerificationType::Variant { variant, arguments } => {
            layouts.instantiate_type(ty)?;
            layouts
                .variant_instance(*variant, arguments)
                .map(NativeType::Object)
                .ok_or_else(|| native_error(format!("variant type in `{function}` has no layout")))
        }
        VerificationType::Reference(pointee) => {
            layouts.instantiate_type(ty)?;
            layouts
                .pointer(pointee, crate::codegen::layout::Ownership::Borrowed)
                .map(NativeType::Object)
                .ok_or_else(|| {
                    native_error(format!("reference type in `{function}` has no layout"))
                })
        }
        VerificationType::Bytes
        | VerificationType::ByteBuffer
        | VerificationType::List(_)
        | VerificationType::Remote(_)
        | VerificationType::Future(_)
        | VerificationType::Function { .. } => {
            layouts.instantiate_type(ty)?;
            layouts
                .builtin(ty)
                .map(NativeType::Object)
                .ok_or_else(|| native_error(format!("runtime type in `{function}` has no layout")))
        }
        VerificationType::Unknown | VerificationType::Union(_) => {
            Ok(NativeType::Object(layouts.opaque()))
        }
        unsupported => Err(native_error(format!(
            "native specialization of `{function}` does not yet support `{unsupported:?}`"
        ))),
    }
}

pub(super) fn record_uses_dynamic_dispatch(
    compilation: &Compilation,
    record: crate::hir::RecordId,
) -> bool {
    let declaration = &compilation.hir.records[record];
    // Private storage is nominal implementation state, so values of this exact type always keep
    // their concrete descriptor. Pure public structural surfaces may be erased when they have no
    // implementation of their own.
    if declaration.fields.iter().any(|field| !field.public) {
        return false;
    }
    let has_contract_surface =
        !declaration.methods.is_empty() || !declaration.compositions.is_empty();
    // Inherited defaults add dispatch entries but do not give a structural
    // contract its own concrete implementation storage.
    let has_implementation = compilation
        .types
        .dispatch
        .iter()
        .any(|((nominal, _), function)| {
            *nominal == crate::types::NominalTypeId::Record(record)
                && !compilation.hir.composition_owners.contains_key(function)
        });
    let provides_defaults = compilation.hir.composition_dispatch.iter().any(|function| {
        let method = &compilation.hir.functions[*function];
        !compilation.hir.composition_owners.contains_key(function)
            && method.module == declaration.module
            && method.owner.as_deref() == Some(declaration.name.as_str())
    });
    (has_contract_surface && !has_implementation) || provides_defaults
}
