//! Native byte and element buffer allocation, copying, and growth.
use super::{
    ClifValue, FosterError, FunctionBuilder, InstBuilder, IntCC, LayoutId, MemFlagsData, Module,
    NativeType, ObjectModule, ObjectRuntime, ValueLayout, abi, fail_if, ir, native_buffer_layout,
    native_bytes_layout, native_error, physical_cranelift_type, runtime_call, store_physical_value,
    types,
};

fn allocate_byte_data(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    length: ClifValue,
) -> Result<ClifValue, FosterError> {
    let word = module.target_config().pointer_type();
    let empty = builder.ins().icmp_imm_s(IntCC::Equal, length, 0);
    let one = builder.ins().iconst(word, 1);
    let size = builder.ins().select(empty, one, length);
    let align = builder.ins().iconst(types::I64, 1);
    runtime_call(
        builder,
        module,
        abi::ALLOC,
        &ir::Signature {
            parameters: vec![NativeType::Int, NativeType::Int],
            result: NativeType::Opaque,
        },
        &[size, align],
    )
}

pub(super) fn allocate_native_bytes(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    objects: ObjectRuntime<'_>,
    layout: LayoutId,
    length: ClifValue,
) -> Result<(ClifValue, ClifValue), FosterError> {
    let (data_offset, length_offset) = native_bytes_layout(layout, objects)?;
    let object = objects.allocate(builder, module, layout)?;
    let data = allocate_byte_data(builder, module, length)?;
    store_physical_value(builder, object, data_offset, data);
    store_physical_value(builder, object, length_offset, length);
    Ok((object, data))
}

pub(super) fn allocate_native_byte_buffer(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    objects: ObjectRuntime<'_>,
    layout: LayoutId,
    length: ClifValue,
) -> Result<(ClifValue, ClifValue), FosterError> {
    let (data_offset, length_offset, capacity_offset, element) =
        native_buffer_layout(layout, objects)?;
    if element.size != 1 {
        return Err(native_error(
            "byte buffer allocation requires one-byte elements",
        ));
    }
    let object = objects.allocate(builder, module, layout)?;
    let data = allocate_byte_data(builder, module, length)?;
    store_physical_value(builder, object, data_offset, data);
    store_physical_value(builder, object, length_offset, length);
    store_physical_value(builder, object, capacity_offset, length);
    Ok((object, data))
}

pub(super) fn copy_native_bytes(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    destination: ClifValue,
    source: ClifValue,
    length: ClifValue,
) -> Result<(), FosterError> {
    runtime_call(
        builder,
        module,
        abi::COPY_BYTES,
        &ir::Signature {
            parameters: vec![NativeType::Opaque, NativeType::Opaque, NativeType::Int],
            result: NativeType::Unit,
        },
        &[destination, source, length],
    )?;
    Ok(())
}

pub(super) fn native_bytes_tail(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    source: ClifValue,
    layout: LayoutId,
    objects: ObjectRuntime<'_>,
) -> Result<ClifValue, FosterError> {
    let (data_offset, length_offset) = native_bytes_layout(layout, objects)?;
    let word = module.target_config().pointer_type();
    let source_data = builder
        .ins()
        .load(word, MemFlagsData::trusted(), source, data_offset as i32);
    let length = builder
        .ins()
        .load(word, MemFlagsData::trusted(), source, length_offset as i32);
    let empty = builder.ins().icmp_imm_s(IntCC::Equal, length, 0);
    let zero = builder.ins().iconst(word, 0);
    let decremented = builder.ins().iadd_imm_s(length, -1);
    let tail_length = builder.ins().select(empty, zero, decremented);
    let (target, target_data) =
        allocate_native_bytes(builder, module, objects, layout, tail_length)?;
    let first_tail_byte = builder.ins().iadd_imm_s(source_data, 1);
    copy_native_bytes(builder, module, target_data, first_tail_byte, tail_length)?;
    Ok(target)
}

pub(super) fn allocate_native_buffer(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    layout: LayoutId,
    length: usize,
    objects: ObjectRuntime<'_>,
) -> Result<ClifValue, FosterError> {
    let (data_offset, length_offset, capacity_offset, element) =
        native_buffer_layout(layout, objects)?;
    let object = objects.allocate(builder, module, layout)?;
    let capacity = length.max(1);
    let size = builder.ins().iconst(
        types::I64,
        i64::try_from(capacity).unwrap_or(i64::MAX) * i64::from(element.size),
    );
    let align = builder.ins().iconst(types::I64, i64::from(element.align));
    let data = runtime_call(
        builder,
        module,
        abi::ALLOC,
        &ir::Signature {
            parameters: vec![NativeType::Int, NativeType::Int],
            result: NativeType::Opaque,
        },
        &[size, align],
    )?;
    let word = module.target_config().pointer_type();
    store_physical_value(builder, object, data_offset, data);
    let length = builder
        .ins()
        .iconst(word, i64::try_from(length).unwrap_or(i64::MAX));
    store_physical_value(builder, object, length_offset, length);
    let capacity = builder
        .ins()
        .iconst(word, i64::try_from(capacity).unwrap_or(i64::MAX));
    store_physical_value(builder, object, capacity_offset, capacity);
    Ok(object)
}

pub(super) fn native_buffer_element_address(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    object: ClifValue,
    index: ClifValue,
    layout: LayoutId,
    objects: ObjectRuntime<'_>,
) -> Result<(ClifValue, ValueLayout), FosterError> {
    let (data_offset, length_offset, _, element) = native_buffer_layout(layout, objects)?;
    let word = module.target_config().pointer_type();
    let length = builder
        .ins()
        .load(word, MemFlagsData::trusted(), object, length_offset as i32);
    let outside = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, index, length);
    fail_if(
        builder,
        module,
        outside,
        abi::failure::INDEX_OUT_OF_BOUNDS,
        index,
        length,
    )?;
    let data = builder
        .ins()
        .load(word, MemFlagsData::trusted(), object, data_offset as i32);
    let offset = builder.ins().imul_imm_u(index, i64::from(element.size));
    Ok((builder.ins().iadd(data, offset), element))
}

fn copy_native_buffer_elements(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    source: ClifValue,
    target: ClifValue,
    layout: LayoutId,
    retain: bool,
    objects: ObjectRuntime<'_>,
) -> Result<(), FosterError> {
    let (data_offset, length_offset, _, element) = native_buffer_layout(layout, objects)?;
    let word = module.target_config().pointer_type();
    let source_data = builder
        .ins()
        .load(word, MemFlagsData::trusted(), source, data_offset as i32);
    let target_data = builder
        .ins()
        .load(word, MemFlagsData::trusted(), target, data_offset as i32);
    let length = builder
        .ins()
        .load(word, MemFlagsData::trusted(), source, length_offset as i32);
    let loop_block = builder.create_block();
    let copy = builder.create_block();
    let finish = builder.create_block();
    builder.append_block_param(loop_block, word);
    let zero = builder.ins().iconst(word, 0);
    builder.ins().jump(loop_block, &[zero.into()]);
    builder.switch_to_block(loop_block);
    let index = builder.block_params(loop_block)[0];
    let done = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, index, length);
    builder.ins().brif(done, finish, &[], copy, &[]);
    builder.switch_to_block(copy);
    let offset = builder.ins().imul_imm_u(index, i64::from(element.size));
    let source_address = builder.ins().iadd(source_data, offset);
    let value = builder.ins().load(
        physical_cranelift_type(element.kind, word),
        MemFlagsData::trusted(),
        source_address,
        0,
    );
    if retain
        && let Some(pointee) = element.pointee
        && objects.layouts.is_managed(pointee)
    {
        objects.retain(builder, value, pointee);
    }
    let target_address = builder.ins().iadd(target_data, offset);
    store_physical_value(builder, target_address, 0, value);
    let next = builder.ins().iadd_imm_s(index, 1);
    builder.ins().jump(loop_block, &[next.into()]);
    builder.switch_to_block(finish);
    Ok(())
}

pub(super) fn clone_native_buffer(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    source: ClifValue,
    layout: LayoutId,
    objects: ObjectRuntime<'_>,
) -> Result<ClifValue, FosterError> {
    let (_, length_offset, _, _) = native_buffer_layout(layout, objects)?;
    let length = builder.ins().load(
        module.target_config().pointer_type(),
        MemFlagsData::trusted(),
        source,
        length_offset as i32,
    );
    let target = allocate_native_buffer_dynamic(builder, module, layout, length, objects)?;
    copy_native_buffer_elements(builder, module, source, target, layout, true, objects)?;
    Ok(target)
}

pub(super) fn native_buffer_tail(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    source: ClifValue,
    layout: LayoutId,
    objects: ObjectRuntime<'_>,
) -> Result<ClifValue, FosterError> {
    let (data_offset, length_offset, _, element) = native_buffer_layout(layout, objects)?;
    let word = module.target_config().pointer_type();
    let source_data = builder
        .ins()
        .load(word, MemFlagsData::trusted(), source, data_offset as i32);
    let length = builder
        .ins()
        .load(word, MemFlagsData::trusted(), source, length_offset as i32);
    let empty = builder.ins().icmp_imm_s(IntCC::Equal, length, 0);
    let zero = builder.ins().iconst(word, 0);
    let decremented = builder.ins().iadd_imm_s(length, -1);
    let tail_length = builder.ins().select(empty, zero, decremented);
    let target = allocate_native_buffer_dynamic(builder, module, layout, tail_length, objects)?;
    let target_data = builder
        .ins()
        .load(word, MemFlagsData::trusted(), target, data_offset as i32);
    let loop_block = builder.create_block();
    let copy = builder.create_block();
    let finish = builder.create_block();
    builder.append_block_param(loop_block, word);
    builder.ins().jump(loop_block, &[zero.into()]);
    builder.switch_to_block(loop_block);
    let index = builder.block_params(loop_block)[0];
    let done = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, index, tail_length);
    builder.ins().brif(done, finish, &[], copy, &[]);
    builder.switch_to_block(copy);
    let target_offset = builder.ins().imul_imm_u(index, i64::from(element.size));
    let source_offset = builder
        .ins()
        .iadd_imm_s(target_offset, i64::from(element.size));
    let source_address = builder.ins().iadd(source_data, source_offset);
    let value = builder.ins().load(
        physical_cranelift_type(element.kind, word),
        MemFlagsData::trusted(),
        source_address,
        0,
    );
    if let Some(pointee) = element.pointee
        && objects.layouts.is_managed(pointee)
    {
        objects.retain(builder, value, pointee);
    }
    let target_address = builder.ins().iadd(target_data, target_offset);
    store_physical_value(builder, target_address, 0, value);
    let next = builder.ins().iadd_imm_s(index, 1);
    builder.ins().jump(loop_block, &[next.into()]);
    builder.switch_to_block(finish);
    Ok(target)
}

fn copy_native_buffer_data(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    source_data: ClifValue,
    target_data: ClifValue,
    length: ClifValue,
    element: ValueLayout,
) {
    let word = module.target_config().pointer_type();
    let loop_block = builder.create_block();
    let copy = builder.create_block();
    let finish = builder.create_block();
    builder.append_block_param(loop_block, word);
    let zero = builder.ins().iconst(word, 0);
    builder.ins().jump(loop_block, &[zero.into()]);
    builder.switch_to_block(loop_block);
    let index = builder.block_params(loop_block)[0];
    let done = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, index, length);
    builder.ins().brif(done, finish, &[], copy, &[]);
    builder.switch_to_block(copy);
    let offset = builder.ins().imul_imm_u(index, i64::from(element.size));
    let source = builder.ins().iadd(source_data, offset);
    let value = builder.ins().load(
        physical_cranelift_type(element.kind, word),
        MemFlagsData::trusted(),
        source,
        0,
    );
    let target = builder.ins().iadd(target_data, offset);
    store_physical_value(builder, target, 0, value);
    let next = builder.ins().iadd_imm_s(index, 1);
    builder.ins().jump(loop_block, &[next.into()]);
    builder.switch_to_block(finish);
}

pub(super) fn allocate_native_buffer_dynamic(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    layout: LayoutId,
    length: ClifValue,
    objects: ObjectRuntime<'_>,
) -> Result<ClifValue, FosterError> {
    let (data_offset, length_offset, capacity_offset, element) =
        native_buffer_layout(layout, objects)?;
    let object = objects.allocate(builder, module, layout)?;
    let word = module.target_config().pointer_type();
    let is_empty = builder.ins().icmp_imm_s(IntCC::Equal, length, 0);
    let one = builder.ins().iconst(word, 1);
    let capacity = builder.ins().select(is_empty, one, length);
    let size = builder.ins().imul_imm_u(capacity, i64::from(element.size));
    let align = builder.ins().iconst(types::I64, i64::from(element.align));
    let data = runtime_call(
        builder,
        module,
        abi::ALLOC,
        &ir::Signature {
            parameters: vec![NativeType::Int, NativeType::Int],
            result: NativeType::Opaque,
        },
        &[size, align],
    )?;
    store_physical_value(builder, object, data_offset, data);
    store_physical_value(builder, object, length_offset, length);
    store_physical_value(builder, object, capacity_offset, capacity);
    Ok(object)
}

pub(super) fn append_native_buffer(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    source: ClifValue,
    value: ClifValue,
    layout: LayoutId,
    objects: ObjectRuntime<'_>,
) -> Result<ClifValue, FosterError> {
    let target = clone_native_buffer(builder, module, source, layout, objects)?;
    // The new buffer is still private to this instruction if capacity checking
    // fails. Its unique reference can be destroyed before cleaning the frame.
    builder
        .cleanup
        .insert(0, (target, objects.destructors[&layout]));
    push_native_buffer(builder, module, target, value, layout, objects)?;
    builder.cleanup.remove(0);
    Ok(target)
}

pub(super) fn push_native_buffer(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    object: ClifValue,
    value: ClifValue,
    layout: LayoutId,
    objects: ObjectRuntime<'_>,
) -> Result<(), FosterError> {
    let (data_offset, length_offset, capacity_offset, element) =
        native_buffer_layout(layout, objects)?;
    let word = module.target_config().pointer_type();
    let old_data = builder
        .ins()
        .load(word, MemFlagsData::trusted(), object, data_offset as i32);
    let length = builder
        .ins()
        .load(word, MemFlagsData::trusted(), object, length_offset as i32);
    let old_capacity = builder.ins().load(
        word,
        MemFlagsData::trusted(),
        object,
        capacity_offset as i32,
    );
    // Capacity growth is storage policy, not a Foster collection algorithm. Keep byte-size
    // arithmetic representable before allocating or writing the next element.
    let maximum = isize::MAX as i64 / i64::from(element.size);
    let full = builder
        .ins()
        .icmp_imm_s(IntCC::SignedGreaterThanOrEqual, length, maximum);
    let limit = builder.ins().iconst(word, maximum);
    fail_if(
        builder,
        module,
        full,
        abi::failure::INTEGER_OVERFLOW,
        length,
        limit,
    )?;
    let new_length = builder.ins().iadd_imm_s(length, 1);
    let grow = builder.create_block();
    let ready = builder.create_block();
    builder.append_block_param(ready, word);
    let needs_growth = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThan, new_length, old_capacity);
    builder
        .ins()
        .brif(needs_growth, grow, &[], ready, &[old_data.into()]);
    builder.switch_to_block(grow);
    let can_double =
        builder
            .ins()
            .icmp_imm_s(IntCC::SignedLessThanOrEqual, old_capacity, maximum / 2);
    let doubled = builder.ins().imul_imm_u(old_capacity, 2);
    let grown = builder.ins().select(can_double, doubled, limit);
    let too_small = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, grown, new_length);
    let new_capacity = builder.ins().select(too_small, new_length, grown);
    let size = builder
        .ins()
        .imul_imm_u(new_capacity, i64::from(element.size));
    let align = builder.ins().iconst(types::I64, i64::from(element.align));
    let new_data = runtime_call(
        builder,
        module,
        abi::ALLOC,
        &ir::Signature {
            parameters: vec![NativeType::Int, NativeType::Int],
            result: NativeType::Opaque,
        },
        &[size, align],
    )?;
    copy_native_buffer_data(builder, module, old_data, new_data, length, element);
    store_physical_value(builder, object, data_offset, new_data);
    store_physical_value(builder, object, capacity_offset, new_capacity);
    let old_size = builder
        .ins()
        .imul_imm_u(old_capacity, i64::from(element.size));
    runtime_call(
        builder,
        module,
        abi::DEALLOC,
        &ir::Signature {
            parameters: vec![NativeType::Opaque, NativeType::Int, NativeType::Int],
            result: NativeType::Unit,
        },
        &[old_data, old_size, align],
    )?;
    builder.ins().jump(ready, &[new_data.into()]);
    builder.switch_to_block(ready);
    let data = builder.block_params(ready)[0];
    let offset = builder.ins().imul_imm_u(length, i64::from(element.size));
    let address = builder.ins().iadd(data, offset);
    if let Some(pointee) = element.pointee
        && objects.layouts.is_managed(pointee)
    {
        objects.retain(builder, value, pointee);
    }
    store_physical_value(builder, address, 0, value);
    store_physical_value(builder, object, length_offset, new_length);
    Ok(())
}
