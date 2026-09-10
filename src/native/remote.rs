//! Remote execution and result conversion.
use super::{
    AlternativeLayout, ClifValue, FosterError, FunctionBuilder, FunctionId, InstBuilder, IntCC,
    LayoutId, LayoutKind, MemFlagsData, Module, NativeBackend, NativeLowering, NativeType,
    ObjectModule, ObjectRuntime, ParameterMode, PhysicalKind, StackSlotData, StackSlotKind, abi,
    allocate_native_handle, ir, load_physical_value, native_error, native_handle_layout,
    native_release_address, native_to_remote_word, native_verification_type, remote_word_to_native,
    runtime_call, store_physical_value, types,
};

pub(super) fn lower_native_spawn_remote(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    destination: ir::Value,
    source: ir::Value,
    borrowed: bool,
    context: NativeLowering<'_, '_>,
) -> Result<ClifValue, FosterError> {
    let NativeType::Object(layout) = context.function.value_type(destination) else {
        return Err(native_error("native Remote<T> has a non-object layout"));
    };
    let source_type = context.function.value_type(source);
    if borrowed && !matches!(source_type, NativeType::Object(_)) {
        return Err(native_error(
            "borrowed native remote state must be an object",
        ));
    }
    let state = native_to_remote_word(builder, module, context.values[&source], source_type);
    if borrowed
        && let NativeType::Object(source_layout) = source_type
        && context.backend.objects.layouts.is_managed(source_layout)
    {
        context
            .backend
            .objects
            .retain(builder, context.values[&source], source_layout);
    }
    let release = native_release_address(builder, module, source_type, context.backend);
    let borrowed = builder.ins().iconst(types::I8, i64::from(borrowed));
    let handle = runtime_call(
        builder,
        module,
        abi::REMOTE_SPAWN,
        &ir::Signature {
            parameters: vec![NativeType::Int, NativeType::Opaque, NativeType::Bool],
            result: NativeType::Opaque,
        },
        &[state, release, borrowed],
    )?;
    allocate_native_handle(
        builder,
        module,
        layout,
        handle,
        source_type,
        context.backend,
    )
}

pub(super) fn lower_native_remote_call(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    destination: ir::Value,
    remote: ir::Value,
    target: FunctionId,
    arguments: &[(ParameterMode, ir::Value)],
    context: NativeLowering<'_, '_>,
) -> Result<ClifValue, FosterError> {
    let target_signature = &context.backend.ir.function_types[&target];
    if target_signature.parameters.len() != arguments.len() + 1 {
        return Err(native_error(
            "native remote call has the wrong argument arity",
        ));
    }
    let frame_size = arguments
        .len()
        .checked_mul(8)
        .and_then(|size| u32::try_from(size.max(8)).ok())
        .ok_or_else(|| native_error("native remote argument frame is too large"))?;
    let frame = builder.create_sized_stack_slot(StackSlotData::new(
        StackSlotKind::ExplicitSlot,
        frame_size,
        3,
    ));
    for (index, (_, argument)) in arguments.iter().enumerate() {
        let value = native_to_remote_word(
            builder,
            module,
            context.values[argument],
            context.function.value_type(*argument),
        );
        builder.ins().stack_store(
            module.target_config().pointer_type(),
            value,
            frame,
            i32::try_from(index * 8)
                .map_err(|_| native_error("native remote argument frame exceeds i32 offsets"))?,
        );
    }
    let NativeType::Object(remote_layout) = context.function.value_type(remote) else {
        return Err(native_error("native remote call has a non-object receiver"));
    };
    let (handle_offset, _) = native_handle_layout(remote_layout, context.backend.objects)?;
    let handle = builder.ins().load(
        module.target_config().pointer_type(),
        MemFlagsData::trusted(),
        context.values[&remote],
        handle_offset as i32,
    );
    let thunk = context
        .backend
        .remote_thunks
        .get(&target)
        .copied()
        .ok_or_else(|| native_error("native remote method has no callback thunk"))?;
    let thunk = module.declare_func_in_func(thunk, builder.func);
    let thunk = builder
        .ins()
        .func_addr(module.target_config().pointer_type(), thunk);
    let argument_data = builder
        .ins()
        .stack_addr(module.target_config().pointer_type(), frame, 0);
    let argument_count = builder.ins().iconst(
        types::I64,
        i64::try_from(arguments.len())
            .map_err(|_| native_error("native remote argument count exceeds Int"))?,
    );
    let blocking = arguments.iter().any(|(mode, argument)| {
        *mode == ParameterMode::Borrow
            && matches!(
                context.function.value_type(*argument),
                NativeType::Object(_) | NativeType::String
            )
    });
    let blocking = builder.ins().iconst(types::I8, i64::from(blocking));
    let result_release =
        native_release_address(builder, module, target_signature.result, context.backend);
    let future = runtime_call(
        builder,
        module,
        abi::REMOTE_CALL,
        &ir::Signature {
            parameters: vec![
                NativeType::Opaque,
                NativeType::Opaque,
                NativeType::Opaque,
                NativeType::Int,
                NativeType::Bool,
                NativeType::Opaque,
            ],
            result: NativeType::Opaque,
        },
        &[
            handle,
            thunk,
            argument_data,
            argument_count,
            blocking,
            result_release,
        ],
    )?;
    let NativeType::Object(future_layout) = context.function.value_type(destination) else {
        return Err(native_error("native Future<T> has a non-object layout"));
    };
    allocate_native_handle(
        builder,
        module,
        future_layout,
        future,
        target_signature.result,
        context.backend,
    )
}

pub(super) fn lower_native_await(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    destination: ir::Value,
    future: ir::Value,
    context: NativeLowering<'_, '_>,
) -> Result<ClifValue, FosterError> {
    let NativeType::Object(future_layout) = context.function.value_type(future) else {
        return Err(native_error("native await has a non-object future"));
    };
    let (handle_offset, _) = native_handle_layout(future_layout, context.backend.objects)?;
    let handle = builder.ins().load(
        module.target_config().pointer_type(),
        MemFlagsData::trusted(),
        context.values[&future],
        handle_offset as i32,
    );
    let value = runtime_call(
        builder,
        module,
        abi::FUTURE_AWAIT,
        &ir::Signature {
            parameters: vec![NativeType::Opaque],
            result: NativeType::Int,
        },
        &[handle],
    )?;
    let error = runtime_call(
        builder,
        module,
        abi::FUTURE_ERROR,
        &ir::Signature {
            parameters: vec![NativeType::Opaque],
            result: NativeType::String,
        },
        &[handle],
    )?;
    let NativeType::Object(result_layout) = context.function.value_type(destination) else {
        return Err(native_error("remote outcome requires Result layout"));
    };
    let LayoutKind::Variant { arguments, .. } = &context.backend.ir.layouts.get(result_layout).kind
    else {
        return Err(native_error("remote outcome must be a variant"));
    };
    let success_type = native_verification_type(
        context.backend.ir.program,
        context.backend.ir.layouts,
        &arguments[0],
        None,
    )?;
    let NativeType::Object(error_layout) = native_verification_type(
        context.backend.ir.program,
        context.backend.ir.layouts,
        &arguments[1],
        None,
    )?
    else {
        return Err(native_error("remote error requires variant layout"));
    };
    let failed = builder.create_block();
    let success = builder.create_block();
    let join = builder.create_block();
    builder.append_block_param(join, module.target_config().pointer_type());
    builder.ins().brif(error, failed, &[], success, &[]);
    builder.switch_to_block(success);
    let value = remote_word_to_native(builder, module, value, success_type);
    let outcome =
        native_outcome_variant(builder, module, result_layout, "Ok", value, context.backend)?;
    builder.ins().jump(join, &[outcome.into()]);
    builder.switch_to_block(failed);
    let shutdown = builder.create_block();
    let execution_failed = builder.create_block();
    let error_join = builder.create_block();
    builder.append_block_param(error_join, module.target_config().pointer_type());
    let cancelled = builder.ins().icmp_imm_s(IntCC::Equal, error, 1);
    builder
        .ins()
        .brif(cancelled, shutdown, &[], execution_failed, &[]);
    builder.switch_to_block(shutdown);
    let shutdown_error = native_outcome_variant(
        builder,
        module,
        error_layout,
        "Shutdown",
        error,
        context.backend,
    )?;
    builder.ins().jump(error_join, &[shutdown_error.into()]);
    builder.switch_to_block(execution_failed);
    let error = native_outcome_variant(
        builder,
        module,
        error_layout,
        "Failed",
        error,
        context.backend,
    )?;
    builder.ins().jump(error_join, &[error.into()]);
    builder.switch_to_block(error_join);
    let error = builder.block_params(error_join)[0];
    let outcome = native_outcome_variant(
        builder,
        module,
        result_layout,
        "Error",
        error,
        context.backend,
    )?;
    builder.ins().jump(join, &[outcome.into()]);
    builder.switch_to_block(join);
    Ok(builder.block_params(join)[0])
}

/// Transfer a freshly owned payload into a remote outcome variant.
fn native_outcome_variant(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    layout: LayoutId,
    name: &str,
    payload: ClifValue,
    backend: &NativeBackend<'_>,
) -> Result<ClifValue, FosterError> {
    let LayoutKind::Variant { alternatives, .. } = &backend.ir.layouts.get(layout).kind else {
        return Err(native_error("remote outcome has no variant layout"));
    };
    let tag = alternatives
        .iter()
        .find(|alternative| {
            backend.ir.program.variants[&alternative.variant]
                .alternative
                .as_ref()
                == name
        })
        .ok_or_else(|| native_error("missing remote outcome alternative"))?
        .tag;
    let PhysicalKind::Variant {
        tag_offset,
        alternatives,
        ..
    } = &backend.objects.layouts.physical.get(layout).kind
    else {
        return Err(native_error(
            "remote outcome has no physical variant layout",
        ));
    };
    let alternative = alternatives
        .iter()
        .find(|alternative| alternative.tag == tag)
        .ok_or_else(|| native_error("missing remote outcome physical alternative"))?;
    let object = backend.objects.allocate(builder, module, layout)?;
    let tag_value = builder.ins().iconst(types::I32, i64::from(tag));
    store_physical_value(builder, object, *tag_offset, tag_value);
    if let Some(field) = alternative.fields.first() {
        store_physical_value(builder, object, field.offset, payload);
    }
    Ok(object)
}

pub(super) fn lower_result_error_conversion(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    source: ClifValue,
    source_type: NativeType,
    target_type: NativeType,
    objects: ObjectRuntime<'_>,
) -> Result<ClifValue, FosterError> {
    let (NativeType::Object(source_layout), NativeType::Object(target_layout)) =
        (source_type, target_type)
    else {
        return Err(native_error(
            "Result error conversion requires object layouts",
        ));
    };
    let result_error = |layout| -> Result<(u32, AlternativeLayout), FosterError> {
        let PhysicalKind::Variant {
            tag_offset,
            alternatives,
            ..
        } = &objects.layouts.physical.get(layout).kind
        else {
            return Err(native_error(
                "Result error conversion requires variant layouts",
            ));
        };
        let alternative = alternatives
            .iter()
            .find(|alternative| alternative.name == "Error")
            .cloned()
            .ok_or_else(|| native_error("Result layout is missing its Error alternative"))?;
        Ok((*tag_offset, alternative))
    };
    let (_, source_error) = result_error(source_layout)?;
    let (target_tag_offset, target_error) = result_error(target_layout)?;
    if source_error.fields.len() != 1
        || target_error.fields.len() != 1
        || source_error.fields[0].value != target_error.fields[0].value
    {
        return Err(native_error(
            "Result error conversion has incompatible error payloads",
        ));
    }
    let field = &source_error.fields[0];
    let value = load_physical_value(builder, module, source, field.offset, field.value);
    if let Some(pointee) = field.value.pointee
        && objects.layouts.is_managed(pointee)
    {
        objects.retain(builder, value, pointee);
    }
    let target = objects.allocate(builder, module, target_layout)?;
    let tag = builder
        .ins()
        .iconst(types::I32, i64::from(target_error.tag));
    builder.ins().store(
        MemFlagsData::trusted(),
        tag,
        target,
        target_tag_offset as i32,
    );
    store_physical_value(builder, target, target_error.fields[0].offset, value);
    Ok(target)
}
