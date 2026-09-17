//! Native layout projections, intrinsic ABIs, and representation joins.
use crate::codegen::types::ExecutableType;
use crate::native::{
    FosterError, LayoutId, LayoutKind, LayoutRegistry, NativeIrEnvironment, NativeType,
    PhysicalKind, PhysicalRegistry, native_error,
};
pub(super) fn join_representation(
    left: NativeType,
    right: NativeType,
    environment: NativeIrEnvironment<'_>,
) -> Result<NativeType, FosterError> {
    if left == right {
        return Ok(left);
    }
    let left_value = dereference_native_type(left, environment)?;
    let right_value = dereference_native_type(right, environment)?;
    if left_value == right_value {
        return Ok(left_value);
    }
    let erased = NativeType::Object(environment.layouts.opaque());
    if left == erased || right == erased {
        return Ok(erased);
    }
    let callable = |ty| -> Result<Option<NativeType>, FosterError> {
        let NativeType::Object(layout) = ty else {
            return Ok(None);
        };
        let logical = match &environment.layouts.get(layout).kind {
            LayoutKind::Closure {
                function,
                specialization,
                ..
            } => {
                let function = &environment.program.functions[function];
                ExecutableType::Function {
                    parameters: crate::types::Parameter::from_parts(
                        function.parameter_types.clone(),
                        function.parameter_modes.clone(),
                    ),
                    result: Box::new(function.result_type.clone()),
                }
                .specialize(specialization)
            }
            LayoutKind::Builtin {
                ty: ty @ ExecutableType::Function { .. },
            } => ty.clone(),
            _ => return Ok(None),
        };
        Ok(Some(native_verification_type(
            &environment.program.metadata,
            environment.layouts,
            &logical,
            None,
        )?))
    };
    if let (Some(left), Some(right)) = (callable(left)?, callable(right)?)
        && left == right
    {
        return Ok(left);
    }
    Err(native_error(format!(
        "incompatible native representations at SSA join: {left:?} and {right:?}"
    )))
}

pub(in crate::native) fn dereference_native_type(
    ty: NativeType,
    environment: NativeIrEnvironment<'_>,
) -> Result<NativeType, FosterError> {
    let NativeType::Object(layout) = ty else {
        return Ok(ty);
    };
    let LayoutKind::Pointer { pointee, .. } = &environment.layouts.get(layout).kind else {
        return Ok(ty);
    };
    native_verification_type(
        &environment.program.metadata,
        environment.layouts,
        pointee,
        None,
    )
}

pub(in crate::native) fn field_type(
    program: &crate::codegen::metadata::ProgramMetadata,
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
            .builtin(&crate::codegen::types::ExecutableType::Bytes)
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
                ty: crate::codegen::types::ExecutableType::List(element),
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
                ty: crate::codegen::types::ExecutableType::Bytes,
            } => match field {
                "empty?" => Ok(NativeType::Bool),
                "length" => Ok(NativeType::Int),
                "head" => Ok(NativeType::Byte),
                "rest" => Ok(NativeType::Object(layout)),
                _ => Err(native_error(format!("native Bytes has no field `{field}`"))),
            },
            LayoutKind::Builtin {
                ty: crate::codegen::types::ExecutableType::ByteBuffer,
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

pub(in crate::native) fn native_verification_type(
    program: &crate::codegen::metadata::ProgramMetadata,
    layouts: &LayoutRegistry,
    ty: &crate::codegen::types::ExecutableType,
    physical_pointee: Option<LayoutId>,
) -> Result<NativeType, FosterError> {
    use crate::codegen::types::ExecutableType;
    match ty {
        ExecutableType::Unit => Ok(NativeType::Unit),
        ExecutableType::Bool => Ok(NativeType::Bool),
        ExecutableType::Integer => Ok(NativeType::Int),
        ExecutableType::Float => Ok(NativeType::Float),
        ExecutableType::CodePoint => Ok(NativeType::CodePoint),
        ExecutableType::Byte => Ok(NativeType::Byte),
        ExecutableType::Record { record, .. } if Some(*record) == program.string_record => {
            Ok(NativeType::String)
        }
        ExecutableType::Record { record, .. } if Some(*record) == program.symbol_record => {
            Ok(NativeType::String)
        }
        ExecutableType::Record { record, arguments } => layouts
            .record_instance(*record, arguments)
            .or(physical_pointee)
            .map(NativeType::Object)
            .ok_or_else(|| native_error("record field has no native layout")),
        ExecutableType::Variant { variant, arguments } => layouts
            .variant_instance(*variant, arguments)
            .or(physical_pointee)
            .map(NativeType::Object)
            .ok_or_else(|| native_error("variant field has no native layout")),
        ExecutableType::List(_)
        | ExecutableType::Bytes
        | ExecutableType::ByteBuffer
        | ExecutableType::Remote(_)
        | ExecutableType::Future(_)
        | ExecutableType::Function { .. } => layouts
            .builtin(ty)
            .or(physical_pointee)
            .map(NativeType::Object)
            .ok_or_else(|| native_error("builtin value has no native layout")),
        ExecutableType::Reference(pointee) => layouts
            .pointer(pointee, crate::codegen::layout::Ownership::Borrowed)
            .or(physical_pointee)
            .map(NativeType::Object)
            .ok_or_else(|| native_error("reference has no native layout")),
        ExecutableType::Unknown
        | ExecutableType::Alternatives(_)
        | ExecutableType::Intersection(_)
        | ExecutableType::AliasArguments { .. } => Ok(NativeType::Object(layouts.opaque())),
        ExecutableType::Generic(name) => Err(native_error(format!(
            "unresolved generic `{name}` has no native representation"
        ))),
    }
}

fn native_intrinsic_type(
    ty: crate::intrinsics::IntrinsicType,
    layouts: &LayoutRegistry,
) -> Result<NativeType, FosterError> {
    use crate::codegen::types::ExecutableType;
    use crate::intrinsics::IntrinsicType;
    match ty {
        IntrinsicType::Unit => Ok(NativeType::Unit),
        IntrinsicType::Bool => Ok(NativeType::Bool),
        IntrinsicType::Integer => Ok(NativeType::Int),
        IntrinsicType::Float => Ok(NativeType::Float),
        IntrinsicType::CodePoint => Ok(NativeType::CodePoint),
        IntrinsicType::Byte => Ok(NativeType::Byte),
        IntrinsicType::String => Ok(NativeType::String),
        IntrinsicType::Bytes => layouts
            .builtin(&ExecutableType::Bytes)
            .map(NativeType::Object)
            .ok_or_else(|| native_error("Bytes intrinsic type has no native layout")),
        IntrinsicType::ByteBuffer => layouts
            .builtin(&ExecutableType::ByteBuffer)
            .map(NativeType::Object)
            .ok_or_else(|| native_error("ByteBuffer intrinsic type has no native layout")),
        IntrinsicType::ListByte => layouts
            .builtin(&ExecutableType::List(Box::new(ExecutableType::Byte)))
            .map(NativeType::Object)
            .ok_or_else(|| native_error("List<Byte> intrinsic type has no native layout")),
        IntrinsicType::Any => Err(native_error(
            "erased intrinsic type does not define a native ABI",
        )),
    }
}

pub(in crate::native) fn native_intrinsic_result_type(
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
    native_verification_type(&environment.program.metadata, environment.layouts, ty, None)
}
