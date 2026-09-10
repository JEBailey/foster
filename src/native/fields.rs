//! Physical record field access.
use super::{
    ClifValue, FosterError, FunctionBuilder, InstBuilder, IntCC, LayoutKind, MemFlagsData, Module,
    NativeType, ObjectModule, ObjectRuntime, abi, fail_if, load_physical_value,
    native_buffer_layout, native_buffer_tail, native_bytes_layout, native_bytes_tail, native_error,
    physical_cranelift_type, types, zero_i64,
};

#[allow(clippy::too_many_arguments)]
pub(super) fn lower_native_field(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    receiver: ClifValue,
    receiver_type: NativeType,
    result_type: NativeType,
    field: &str,
    objects: ObjectRuntime<'_>,
) -> Result<ClifValue, FosterError> {
    let NativeType::Object(layout) = receiver_type else {
        return Err(native_error("native field load requires a Foster object"));
    };
    if matches!(
        objects.layouts.logical.get(layout).kind,
        LayoutKind::Builtin {
            ty: crate::vm::VerificationType::Bytes
        }
    ) {
        let (data_offset, length_offset) = native_bytes_layout(layout, objects)?;
        let word = module.target_config().pointer_type();
        let length = builder.ins().load(
            word,
            MemFlagsData::trusted(),
            receiver,
            length_offset as i32,
        );
        let result = match field {
            "empty?" => builder.ins().icmp_imm_s(IntCC::Equal, length, 0),
            "length" => length,
            "head" => {
                let empty = builder.ins().icmp_imm_s(IntCC::Equal, length, 0);
                let index = zero_i64(builder);
                fail_if(
                    builder,
                    module,
                    empty,
                    abi::failure::INDEX_OUT_OF_BOUNDS,
                    index,
                    length,
                )?;
                let data =
                    builder
                        .ins()
                        .load(word, MemFlagsData::trusted(), receiver, data_offset as i32);
                builder
                    .ins()
                    .load(types::I8, MemFlagsData::trusted(), data, 0)
            }
            "rest" => native_bytes_tail(builder, module, receiver, layout, objects)?,
            _ => return Err(native_error(format!("native Bytes has no field `{field}`"))),
        };
        return Ok(result);
    }
    if matches!(
        objects.layouts.logical.get(layout).kind,
        LayoutKind::Builtin {
            ty: crate::vm::VerificationType::ByteBuffer
        }
    ) {
        let (_, length_offset, capacity_offset, _) = native_buffer_layout(layout, objects)?;
        let offset = match field {
            "length" => length_offset,
            "capacity" => capacity_offset,
            "empty?" => {
                let length = builder.ins().load(
                    module.target_config().pointer_type(),
                    MemFlagsData::trusted(),
                    receiver,
                    length_offset as i32,
                );
                return Ok(builder.ins().icmp_imm_s(IntCC::Equal, length, 0));
            }
            _ => {
                return Err(native_error(format!(
                    "native ByteBuffer has no field `{field}`"
                )));
            }
        };
        return Ok(builder.ins().load(
            module.target_config().pointer_type(),
            MemFlagsData::trusted(),
            receiver,
            offset as i32,
        ));
    }
    if matches!(
        objects.layouts.logical.get(layout).kind,
        LayoutKind::Builtin {
            ty: crate::vm::VerificationType::List(_)
        }
    ) {
        let (data_offset, length_offset, _, element) = native_buffer_layout(layout, objects)?;
        let word = module.target_config().pointer_type();
        let length = builder.ins().load(
            word,
            MemFlagsData::trusted(),
            receiver,
            length_offset as i32,
        );
        let result = match field {
            "empty?" => builder.ins().icmp_imm_s(IntCC::Equal, length, 0),
            "length" => length,
            "head" => {
                let empty = builder.ins().icmp_imm_s(IntCC::Equal, length, 0);
                let index = zero_i64(builder);
                fail_if(
                    builder,
                    module,
                    empty,
                    abi::failure::INDEX_OUT_OF_BOUNDS,
                    index,
                    length,
                )?;
                let data =
                    builder
                        .ins()
                        .load(word, MemFlagsData::trusted(), receiver, data_offset as i32);
                let value = builder.ins().load(
                    physical_cranelift_type(element.kind, word),
                    MemFlagsData::trusted(),
                    data,
                    0,
                );
                if let Some(pointee) = element.pointee
                    && objects.layouts.is_managed(pointee)
                {
                    objects.retain(builder, value, pointee);
                }
                value
            }
            "rest" => native_buffer_tail(builder, module, receiver, layout, objects)?,
            _ => return Err(native_error(format!("native list has no field `{field}`"))),
        };
        return Ok(result);
    }
    let LayoutKind::Record { fields, .. } = &objects.layouts.logical.get(layout).kind else {
        return Err(native_error("native field load requires a record or list"));
    };
    let slot = fields
        .iter()
        .find(|slot| slot.name == field)
        .ok_or_else(|| native_error(format!("record has no field `{field}`")))?;
    let physical = objects
        .layouts
        .physical
        .record_field(layout, slot.index)
        .ok_or_else(|| native_error("record field has no physical slot"))?;
    let result = load_physical_value(builder, module, receiver, physical.offset, physical.value);
    if let Some(pointee) = objects.layouts.managed_layout(result_type) {
        objects.retain(builder, result, pointee);
    }
    Ok(result)
}
