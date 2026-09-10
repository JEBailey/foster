//! Runtime handle layouts, descriptors, and allocation.
use super::{
    ClifValue, FosterError, FunctionBuilder, InstBuilder, LayoutId, LayoutKind, MemFlagsData,
    Module, NativeBackend, NativeType, ObjectModule, ObjectRuntime, PhysicalKind, ValueLayout,
    cranelift_type, native_error, native_verification_type, store_physical_value,
};

pub(super) fn native_reference_receiver(
    builder: &mut FunctionBuilder<'_>,
    module: &ObjectModule,
    value: ClifValue,
    mut ty: NativeType,
    backend: &NativeBackend<'_>,
) -> Result<(ClifValue, NativeType), FosterError> {
    let mut value = value;
    while let NativeType::Object(layout) = ty {
        let LayoutKind::Pointer { pointee, .. } = &backend.ir.layouts.get(layout).kind else {
            break;
        };
        ty = native_verification_type(backend.ir.program, backend.ir.layouts, pointee, None)?;
        value = builder.ins().load(
            cranelift_type(ty, module.target_config().pointer_type()),
            MemFlagsData::trusted(),
            value,
            0,
        );
    }
    Ok((value, ty))
}

pub(super) fn native_buffer_layout(
    layout: LayoutId,
    objects: ObjectRuntime<'_>,
) -> Result<(u32, u32, u32, ValueLayout), FosterError> {
    match objects.layouts.physical.get(layout).kind {
        PhysicalKind::Buffer {
            data_offset,
            length_offset,
            capacity_offset,
            element,
            ..
        } => Ok((data_offset, length_offset, capacity_offset, element)),
        _ => Err(native_error(
            "native list operation requires a buffer layout",
        )),
    }
}

pub(super) fn native_bytes_layout(
    layout: LayoutId,
    objects: ObjectRuntime<'_>,
) -> Result<(u32, u32), FosterError> {
    match objects.layouts.physical.get(layout).kind {
        PhysicalKind::Bytes {
            data_offset,
            length_offset,
        } => Ok((data_offset, length_offset)),
        ref kind => Err(native_error(format!(
            "native Bytes l{} has the wrong physical layout {kind:?}",
            layout.0
        ))),
    }
}

pub(super) fn native_handle_layout(
    layout: LayoutId,
    objects: ObjectRuntime<'_>,
) -> Result<(u32, u32), FosterError> {
    match objects.layouts.physical.get(layout).kind {
        PhysicalKind::Handle {
            handle_offset,
            value_descriptor_offset,
        } => Ok((handle_offset, value_descriptor_offset)),
        _ => Err(native_error("native remote value requires a handle layout")),
    }
}

pub(super) fn native_release_address(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    ty: NativeType,
    backend: &NativeBackend<'_>,
) -> ClifValue {
    if let Some(layout) = backend.objects.layouts.managed_layout(ty) {
        let release = module.declare_func_in_func(backend.release_thunks[&layout], builder.func);
        builder
            .ins()
            .func_addr(module.target_config().pointer_type(), release)
    } else {
        builder
            .ins()
            .iconst(module.target_config().pointer_type(), 0)
    }
}

fn native_descriptor_address(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    ty: NativeType,
    backend: &NativeBackend<'_>,
) -> ClifValue {
    if let NativeType::Object(layout) = ty {
        let descriptor =
            module.declare_data_in_func(backend.objects.descriptors[&layout], builder.func);
        builder
            .ins()
            .symbol_value(module.target_config().pointer_type(), descriptor)
    } else {
        builder
            .ins()
            .iconst(module.target_config().pointer_type(), 0)
    }
}

pub(super) fn allocate_native_handle(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    layout: LayoutId,
    handle: ClifValue,
    value_type: NativeType,
    backend: &NativeBackend<'_>,
) -> Result<ClifValue, FosterError> {
    let object = backend.objects.allocate(builder, module, layout)?;
    let (handle_offset, descriptor_offset) = native_handle_layout(layout, backend.objects)?;
    store_physical_value(builder, object, handle_offset, handle);
    let descriptor = native_descriptor_address(builder, module, value_type, backend);
    store_physical_value(builder, object, descriptor_offset, descriptor);
    Ok(object)
}
