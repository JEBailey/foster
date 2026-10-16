//! Checked contiguous packet construction for std.ffi's private intrinsics.
use super::*;

pub(super) fn lower(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    context: NativeLowering<'_, '_>,
    builtin: crate::intrinsics::Builtin,
    destination: ir::Value,
    arguments: &[ir::Value],
    lowered: &[ClifValue],
) -> Result<ClifValue, FosterError> {
    use crate::intrinsics::Builtin;
    let NativeLowering {
        backend, function, ..
    } = context;
    let objects = backend.objects;
    let word = module.target_config().pointer_type();
    let (owner, owner_type) = native_reference_receiver(
        builder,
        module,
        lowered[0],
        function.value_type(arguments[0]),
        backend,
    )?;
    let NativeType::Object(owner_layout) = owner_type else {
        return Err(native_error("C arguments require ByteBuffer storage"));
    };
    let PhysicalKind::Record { fields, .. } = &objects.layouts.physical.get(owner_layout).kind
    else {
        return Err(native_error("C arguments require ByteBuffer storage"));
    };
    let field = fields
        .iter()
        .find(|f| f.name == "value")
        .ok_or_else(|| native_error("C arguments require ByteBuffer.value"))?;
    let layout = field
        .value
        .pointee
        .ok_or_else(|| native_error("C arguments require byte-list storage"))?;
    let (data_offset, length_offset, capacity_offset, element) =
        native_buffer_layout(layout, objects)?;
    if element.size != 1 || element.pointee.is_some() {
        return Err(native_error("C arguments require one-byte scalar elements"));
    }
    let source = load_physical_value(builder, module, owner, field.offset, field.value);
    if builtin == Builtin::CArgumentsSnapshot {
        let length =
            builder
                .ins()
                .load(word, MemFlagsData::trusted(), source, length_offset as i32);
        let data = builder
            .ins()
            .load(word, MemFlagsData::trusted(), source, data_offset as i32);
        let NativeType::Object(result_layout) = function.value_type(destination) else {
            return Err(native_error("C argument snapshot requires Bytes"));
        };
        let (result, result_data) =
            allocate_native_bytes(builder, module, objects, result_layout, length)?;
        copy_native_bytes(builder, module, result_data, data, length)?;
        return Ok(result);
    }
    // Preserve list value semantics if an internal reader still owns a snapshot.
    let count_address = builder.ins().iadd_imm_s(
        source,
        i64::from(
            objects
                .layouts
                .physical
                .get(layout)
                .header
                .strong_count_offset,
        ),
    );
    let count = builder
        .ins()
        .atomic_load(word, MemFlagsData::trusted(), count_address);
    let unique = builder.ins().icmp_imm_s(IntCC::Equal, count, 1);
    let detach = builder.create_block();
    let attached = builder.create_block();
    builder.append_block_param(attached, word);
    builder
        .ins()
        .brif(unique, attached, &[source.into()], detach, &[]);
    builder.switch_to_block(detach);
    let copied =
        buffers::clone_native_buffer_retaining_capacity(builder, module, source, layout, objects)?;
    objects.release(builder, module, source, layout)?;
    store_physical_value(builder, owner, field.offset, copied);
    builder.ins().jump(attached, &[copied.into()]);
    builder.switch_to_block(attached);
    let object = builder.block_params(attached)[0];
    let length = builder
        .ins()
        .load(word, MemFlagsData::trusted(), object, length_offset as i32);
    let capacity = builder.ins().load(
        word,
        MemFlagsData::trusted(),
        object,
        capacity_offset as i32,
    );
    let old_data = builder
        .ins()
        .load(word, MemFlagsData::trusted(), object, data_offset as i32);
    let reserve = builtin == Builtin::CArgumentsReserve;
    let mut source_data = None;
    let amount = match builtin {
        Builtin::CArgumentsInteger | Builtin::CArgumentsFloat => builder.ins().iconst(word, 8),
        Builtin::CArgumentsReserve => lowered[1],
        Builtin::CArgumentsBytes => {
            let NativeType::Object(bytes_layout) = function.value_type(arguments[1]) else {
                return Err(native_error("C argument extension requires Bytes"));
            };
            let (data, length) = native_bytes_layout(bytes_layout, objects)?;
            source_data = Some(builder.ins().load(
                word,
                MemFlagsData::trusted(),
                lowered[1],
                data as i32,
            ));
            builder
                .ins()
                .load(word, MemFlagsData::trusted(), lowered[1], length as i32)
        }
        _ => unreachable!(),
    };
    let maximum = builder.ins().iconst(word, isize::MAX as i64);
    let available = if reserve {
        maximum
    } else {
        builder.ins().isub(maximum, length)
    };
    let invalid = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThan, amount, available);
    fail_if(
        builder,
        module,
        invalid,
        abi::failure::INTEGER_OVERFLOW,
        amount,
        available,
    )?;
    let needed = if reserve {
        amount
    } else {
        builder.ins().iadd(length, amount)
    };
    let grow = builder.create_block();
    let ready = builder.create_block();
    builder.append_block_param(ready, word);
    let needs_growth = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThan, needed, capacity);
    builder
        .ins()
        .brif(needs_growth, grow, &[], ready, &[old_data.into()]);
    builder.switch_to_block(grow);
    let can_double = builder.ins().icmp_imm_s(
        IntCC::SignedLessThanOrEqual,
        capacity,
        isize::MAX as i64 / 2,
    );
    let doubled = builder.ins().imul_imm_u(capacity, 2);
    let grown = builder.ins().select(can_double, doubled, maximum);
    let too_small = builder.ins().icmp(IntCC::UnsignedLessThan, grown, needed);
    let new_capacity = builder.ins().select(too_small, needed, grown);
    let align = builder.ins().iconst(types::I64, 1);
    let data = runtime_call(
        builder,
        module,
        abi::ALLOC,
        &ir::Signature {
            parameters: vec![NativeType::Int, NativeType::Int],
            result: NativeType::Opaque,
        },
        &[new_capacity, align],
    )?;
    copy_native_bytes(builder, module, data, old_data, length)?;
    // Commit the new allocation before releasing the old backing storage.
    store_physical_value(builder, object, data_offset, data);
    store_physical_value(builder, object, capacity_offset, new_capacity);
    runtime_call(
        builder,
        module,
        abi::DEALLOC,
        &ir::Signature {
            parameters: vec![NativeType::Opaque, NativeType::Int, NativeType::Int],
            result: NativeType::Unit,
        },
        &[old_data, capacity, align],
    )?;
    builder.ins().jump(ready, &[data.into()]);
    builder.switch_to_block(ready);
    let data = builder.block_params(ready)[0];
    if !reserve {
        let target = builder.ins().iadd(data, length);
        if let Some(source) = source_data {
            copy_native_bytes(builder, module, target, source, amount)?;
        } else {
            let flags =
                MemFlagsData::trusted().with_endianness(cranelift_codegen::ir::Endianness::Little);
            builder.ins().store(flags, lowered[1], target, 0);
        }
        store_physical_value(builder, object, length_offset, needed);
    }
    Ok(builder.ins().iconst(types::I8, 0))
}

/// Materialize only primitive words in an aligned, call-scoped scalar frame.
/// Managed record layout never crosses the C ABI boundary.
pub(super) fn lower_scalars(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    context: NativeLowering<'_, '_>,
    destination: ir::Value,
    arguments: &[ir::Value],
    lowered: &[ClifValue],
) -> Result<ClifValue, FosterError> {
    let objects = context.backend.objects;
    let function = context.function;
    let fields = match function.value_type(arguments[3]) {
        NativeType::Unit => Some(Vec::new()),
        NativeType::Object(layout) => match &objects.layouts.physical.get(layout).kind {
            PhysicalKind::Record { fields, .. } => {
                let mut fields = fields.iter().collect::<Vec<_>>();
                fields.sort_by_key(|field| &field.name);
                Some(fields)
            }
            _ => None,
        },
        _ => None,
    };
    let valid = fields.as_ref().is_some_and(|fields| {
        fields.len() <= 256
            && fields.iter().all(|field| {
                field.value.pointee.is_none()
                    && matches!(
                        field.value.semantic,
                        ValueSemantic::Integer | ValueSemantic::Float | ValueSemantic::Bool
                    )
            })
    });
    let word = module.target_config().pointer_type();
    let (data, count) = if valid {
        let fields = fields.unwrap();
        let slot = builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            (fields.len() as u32 * 16).max(16),
            3,
        ));
        for (index, field) in fields.iter().enumerate() {
            let value = load_physical_value(builder, module, lowered[3], field.offset, field.value);
            let (bits, kind) = match field.value.semantic {
                ValueSemantic::Integer => (value, 1),
                ValueSemantic::Float => (
                    builder
                        .ins()
                        .bitcast(types::I64, MemFlagsData::new(), value),
                    2,
                ),
                ValueSemantic::Bool => (builder.ins().uextend(types::I64, value), 3),
                _ => unreachable!(),
            };
            let kind = builder.ins().iconst(types::I64, kind);
            builder
                .ins()
                .stack_store(word, bits, slot, (index * 16) as i32);
            builder
                .ins()
                .stack_store(word, kind, slot, (index * 16 + 8) as i32);
        }
        (builder.ins().stack_addr(word, slot, 0), fields.len() as i64)
    } else {
        (builder.ins().iconst(word, 0), -1)
    };
    let count = builder.ins().iconst(types::I64, count);
    runtime_call(
        builder,
        module,
        abi::C_EXCHANGE_SCALARS,
        &ir::Signature {
            parameters: vec![
                function.value_type(arguments[0]),
                function.value_type(arguments[1]),
                NativeType::Int,
                NativeType::Opaque,
                NativeType::Int,
            ],
            result: function.value_type(destination),
        },
        &[lowered[0], lowered[1], lowered[2], data, count],
    )
}
