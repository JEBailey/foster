//! Cranelift signatures and scalar representations.
use super::{
    AbiParam, ClifSignature, ClifType, ClifValue, FosterError, FunctionBuilder, InstBuilder,
    MemFlagsData, Module, NativeType, ObjectModule, ScalarKind, ValueLayout, abi, ir, native_error,
    types,
};

pub(super) fn signature(module: &mut ObjectModule, source: &ir::Signature) -> ClifSignature {
    let mut signature = module.make_signature();
    let pointer_type = module.target_config().pointer_type();
    signature.params = source
        .parameters
        .iter()
        .map(|ty| AbiParam::new(cranelift_type(*ty, pointer_type)))
        .collect();
    signature
        .returns
        .push(AbiParam::new(cranelift_type(source.result, pointer_type)));
    signature
}

pub(super) fn cranelift_type(ty: NativeType, pointer_type: ClifType) -> ClifType {
    cranelift_representation(ty.representation(), pointer_type)
}

pub(super) fn cranelift_representation(
    representation: ir::Representation,
    pointer_type: ClifType,
) -> ClifType {
    match representation {
        ir::Representation::I8 => types::I8,
        ir::Representation::I32 => types::I32,
        ir::Representation::I64 => types::I64,
        ir::Representation::F64 => types::F64,
        ir::Representation::Pointer => pointer_type,
    }
}

pub(super) fn runtime_signature(
    destination: ir::Value,
    arguments: &[ir::Value],
    value_types: &[NativeType],
) -> ir::Signature {
    ir::Signature {
        parameters: arguments
            .iter()
            .map(|value| value_types[value.0 as usize])
            .collect(),
        result: value_types[destination.0 as usize],
    }
}

pub(super) fn native_field_helper(
    receiver: NativeType,
    field: &str,
) -> Result<&'static str, FosterError> {
    let receiver_kind = match receiver {
        NativeType::String => Some(crate::intrinsics::NativeReceiverKind::String),
        _ => None,
    };
    receiver_kind
        .and_then(|receiver| crate::intrinsics::native_member_runtime(receiver, field))
        .ok_or_else(|| {
            native_error(format!(
                "native compilation does not support field `{field}` on `{receiver:?}`"
            ))
        })
}

pub(super) fn reference_load_helper(ty: NativeType) -> &'static str {
    match ty {
        NativeType::Unit | NativeType::Bool | NativeType::Byte => abi::REF_LOAD_I8,
        NativeType::CodePoint => abi::REF_LOAD_I32,
        NativeType::Int => abi::REF_LOAD_I64,
        NativeType::Float => abi::REF_LOAD_F64,
        NativeType::String | NativeType::Object(_) | NativeType::Opaque => abi::REF_LOAD_PTR,
    }
}

pub(super) fn reference_store_helper(ty: NativeType) -> &'static str {
    match ty {
        NativeType::Unit | NativeType::Bool | NativeType::Byte => abi::REF_STORE_I8,
        NativeType::CodePoint => abi::REF_STORE_I32,
        NativeType::Int => abi::REF_STORE_I64,
        NativeType::Float => abi::REF_STORE_F64,
        NativeType::String | NativeType::Object(_) | NativeType::Opaque => abi::REF_STORE_PTR,
    }
}

pub(super) fn load_physical_value(
    builder: &mut FunctionBuilder<'_>,
    module: &ObjectModule,
    object: ClifValue,
    offset: u32,
    value: ValueLayout,
) -> ClifValue {
    let ty = physical_cranelift_type(value.kind, module.target_config().pointer_type());
    builder
        .ins()
        .load(ty, MemFlagsData::trusted(), object, offset as i32)
}

pub(super) fn store_physical_value(
    builder: &mut FunctionBuilder<'_>,
    object: ClifValue,
    offset: u32,
    value: ClifValue,
) {
    builder
        .ins()
        .store(MemFlagsData::trusted(), value, object, offset as i32);
}

pub(super) fn physical_cranelift_type(kind: ScalarKind, pointer_type: ClifType) -> ClifType {
    match kind {
        ScalarKind::I8 => types::I8,
        ScalarKind::I32 => types::I32,
        ScalarKind::I64 => types::I64,
        ScalarKind::F64 => types::F64,
        ScalarKind::Pointer => pointer_type,
    }
}
