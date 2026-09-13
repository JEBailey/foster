//! Logical-to-native type and layout conversion.
use super::{
    Compilation, FosterError, HashMap, Instruction, LayoutRegistry, NativeType, Program, Type,
    TypeId, native_error,
};

pub(super) fn native_builtin_result_types(
    compilation: &Compilation,
) -> Result<HashMap<crate::intrinsics::Builtin, crate::codegen::types::ExecutableType>, FosterError>
{
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
        let ty =
            specialized_executable_type(compilation, signature.result, &Default::default(), 0)?;
        result.insert(builtin, ty);
    }
    Ok(result)
}

pub(super) fn instruction_layout_type(
    program: &Program,
    instruction: &Instruction,
    specialization: &crate::codegen::types::Specialization,
) -> Option<crate::codegen::types::ExecutableType> {
    use crate::codegen::types::ExecutableType;
    match instruction {
        Instruction::CallContractMethod { result_type, .. } => {
            Some(result_type.specialize(specialization))
        }

        Instruction::MakeRecord {
            record,
            type_arguments,
            ..
        } => Some(ExecutableType::Record {
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
        } => Some(ExecutableType::Variant {
            variant: program.variants[variant].parent,
            arguments: type_arguments
                .iter()
                .map(|ty| ty.specialize(specialization))
                .collect(),
        }),
        Instruction::MakeList { element_type, .. } => Some(ExecutableType::List(Box::new(
            element_type.specialize(specialization),
        ))),
        Instruction::MakeReference { pointee_type, .. }
        | Instruction::MakeWholeReference { pointee_type, .. }
        | Instruction::MakeFieldReference { pointee_type, .. } => Some(ExecutableType::Reference(
            Box::new(pointee_type.specialize(specialization)),
        )),
        _ => None,
    }
}

pub(super) fn native_type(
    compilation: &Compilation,
    layouts: &mut LayoutRegistry,
    ty: TypeId,
    substitutions: &crate::codegen::types::Specialization,
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
        Type::Int | Type::RawInt => Ok(NativeType::Int),
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
            let concrete = crate::codegen::types::ExecutableType::Bytes;
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
            let element = specialized_executable_type(compilation, arguments[0], substitutions, 1)?;
            let concrete = crate::codegen::types::ExecutableType::List(Box::new(element));
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
                .map(|ty| specialized_executable_type(compilation, *ty, substitutions, 1))
                .collect::<Result<Vec<_>, _>>()?;
            let concrete = crate::codegen::types::ExecutableType::Record {
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
                .map(|ty| specialized_executable_type(compilation, *ty, substitutions, 1))
                .collect::<Result<Vec<_>, _>>()?;
            layouts.instantiate_type(&crate::codegen::types::ExecutableType::Union(members))?;
            Ok(NativeType::Object(layouts.opaque()))
        }
        Type::Variant {
            variant,
            ref arguments,
        } => {
            let arguments = arguments
                .iter()
                .map(|ty| specialized_executable_type(compilation, *ty, substitutions, 1))
                .collect::<Result<Vec<_>, _>>()?;
            let concrete = crate::codegen::types::ExecutableType::Variant {
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
            let concrete = specialized_executable_type(compilation, ty, substitutions, 0)?;
            concrete_native_type(compilation, layouts, &concrete, function)
        }
        ref unsupported => Err(native_error(format!(
            "native compilation of `{function}` does not yet support type `{}` ({unsupported:?})",
            compilation.types.display(ty)
        ))
        .with_help("use `foster build` without `--native` for the complete VM language")),
    }
}

pub(super) fn specialized_executable_type(
    compilation: &Compilation,
    ty: TypeId,
    substitutions: &crate::codegen::types::Specialization,
    depth: usize,
) -> Result<crate::codegen::types::ExecutableType, FosterError> {
    use crate::codegen::type_conversion::{MAX_TYPE_DEPTH, Native, convert};
    convert::<Native>(
        &compilation.hir,
        &compilation.types,
        ty,
        substitutions,
        depth,
    )
    .map_err(|_| {
        native_error(format!(
            "native specialization type nesting exceeds {MAX_TYPE_DEPTH} levels"
        ))
    })
}

pub(super) fn concrete_native_type(
    compilation: &Compilation,
    layouts: &mut LayoutRegistry,
    ty: &crate::codegen::types::ExecutableType,
    function: &str,
) -> Result<NativeType, FosterError> {
    use crate::codegen::types::ExecutableType;
    match ty {
        ExecutableType::Unit => Ok(NativeType::Unit),
        ExecutableType::Bool => Ok(NativeType::Bool),
        ExecutableType::Integer => Ok(NativeType::Int),
        ExecutableType::Float => Ok(NativeType::Float),
        ExecutableType::CodePoint => Ok(NativeType::CodePoint),
        ExecutableType::Byte => Ok(NativeType::Byte),
        ExecutableType::Record { record, .. } if Some(*record) == compilation.types.core.string => {
            Ok(NativeType::String)
        }
        ExecutableType::Record { record, .. }
            if record_uses_dynamic_dispatch(compilation, *record) =>
        {
            Ok(NativeType::Object(layouts.opaque()))
        }
        ExecutableType::Record { record, .. } if Some(*record) == compilation.types.core.symbol => {
            Ok(NativeType::String)
        }
        ExecutableType::Record { record, .. } if Some(*record) == compilation.types.core.bytes => {
            layouts.instantiate_type(&ExecutableType::Bytes)?;
            layouts
                .builtin(&ExecutableType::Bytes)
                .map(NativeType::Object)
                .ok_or_else(|| native_error(format!("Bytes type in `{function}` has no layout")))
        }
        ExecutableType::Record { record, arguments } => {
            layouts.instantiate_type(ty)?;
            layouts
                .record_instance(*record, arguments)
                .map(NativeType::Object)
                .ok_or_else(|| native_error(format!("record type in `{function}` has no layout")))
        }
        ExecutableType::Variant { variant, arguments } => {
            layouts.instantiate_type(ty)?;
            layouts
                .variant_instance(*variant, arguments)
                .map(NativeType::Object)
                .ok_or_else(|| native_error(format!("variant type in `{function}` has no layout")))
        }
        ExecutableType::Reference(pointee) => {
            layouts.instantiate_type(ty)?;
            layouts
                .pointer(pointee, crate::codegen::layout::Ownership::Borrowed)
                .map(NativeType::Object)
                .ok_or_else(|| {
                    native_error(format!("reference type in `{function}` has no layout"))
                })
        }
        ExecutableType::Bytes
        | ExecutableType::ByteBuffer
        | ExecutableType::List(_)
        | ExecutableType::Remote(_)
        | ExecutableType::Future(_)
        | ExecutableType::Function { .. } => {
            layouts.instantiate_type(ty)?;
            layouts
                .builtin(ty)
                .map(NativeType::Object)
                .ok_or_else(|| native_error(format!("runtime type in `{function}` has no layout")))
        }
        ExecutableType::Unknown | ExecutableType::Union(_) => {
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
    crate::codegen::type_conversion::record_uses_dynamic_dispatch(
        &compilation.hir,
        &compilation.types,
        record,
    )
}
