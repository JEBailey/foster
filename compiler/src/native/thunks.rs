//! Callable and remote entry adapters and function definitions.
use super::{
    ClifValue, FosterError, FuncId, FunctionBuilder, FunctionBuilderContext, FunctionId, HashMap,
    HashSet, InstBuilder, LayoutId, LayoutKind, Linkage, MemFlagsData, Module, NativeBackend,
    NativeFunction, NativeInstance, NativeLayouts, NativeType, ObjectModule, PhysicalKind,
    SpecializationKey, ir, load_physical_value, lower_native_ir, native_error, ordered_entries,
    propagate_native_failure, signature, types,
};

fn concrete_closure_target(
    layouts: NativeLayouts<'_>,
    instances: &HashMap<SpecializationKey, FunctionId>,
    layout: LayoutId,
) -> Option<(FunctionId, usize)> {
    let LayoutKind::Closure {
        function,
        specialization,
        captures,
    } = &layouts.logical.get(layout).kind
    else {
        return None;
    };
    instances
        .get(&SpecializationKey {
            function: *function,
            substitutions: specialization.clone(),
        })
        .copied()
        .map(|target| (target, captures.len()))
}

pub(super) fn declare_callable_thunks(
    module: &mut ObjectModule,
    layouts: NativeLayouts<'_>,
    instances: &HashMap<SpecializationKey, FunctionId>,
    function_types: &HashMap<FunctionId, ir::Signature>,
) -> Result<HashMap<LayoutId, FuncId>, FosterError> {
    let mut result = HashMap::new();
    for layout in layouts
        .physical
        .layouts()
        .iter()
        .filter(|layout| layout.materialized)
    {
        let Some((target, captures)) = concrete_closure_target(layouts, instances, layout.id)
        else {
            continue;
        };
        let target_signature = &function_types[&target];
        let mut parameters = vec![NativeType::Object(layout.id)];
        parameters.extend_from_slice(&target_signature.parameters[captures..]);
        let thunk_signature = signature(
            module,
            &ir::Signature {
                parameters,
                result: target_signature.result,
            },
        );
        let name = format!("foster_callable_l{}", layout.id.0);
        let id = module
            .declare_function(&name, Linkage::Local, &thunk_signature)
            .map_err(|error| native_error(format!("cannot declare `{name}`: {error}")))?;
        result.insert(layout.id, id);
    }
    Ok(result)
}

pub(super) fn declare_remote_thunks(
    module: &mut ObjectModule,
    instances: &[NativeInstance],
    method_receivers: &HashSet<FunctionId>,
) -> Result<HashMap<FunctionId, FuncId>, FosterError> {
    let thunk_signature = signature(
        module,
        &ir::Signature {
            parameters: vec![NativeType::Int, NativeType::Opaque, NativeType::Bool],
            result: NativeType::Int,
        },
    );
    instances
        .iter()
        .filter(|instance| method_receivers.contains(&instance.key.function))
        .map(|instance| {
            let name = format!(
                "foster_remote_{}",
                instance.ir_function.into_raw().into_u32()
            );
            let thunk = module
                .declare_function(&name, Linkage::Local, &thunk_signature)
                .map_err(|error| native_error(format!("cannot declare `{name}`: {error}")))?;
            Ok((instance.ir_function, thunk))
        })
        .collect()
}

pub(super) fn define_function(
    module: &mut ObjectModule,
    prepared: &NativeFunction,
    native_id: FuncId,
    backend: &NativeBackend<'_>,
) -> Result<(), FosterError> {
    let instance = &prepared.instance;
    let function = &backend.ir.program.functions[&instance.key.function];
    let frontend_config = module.target_config();
    let mut context = module.make_context();
    context.func.signature = signature(module, &backend.ir.function_types[&instance.ir_function]);
    let mut builder_context = FunctionBuilderContext::new();
    {
        let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
        lower_native_ir(&mut builder, module, prepared, backend)?;
        builder.finalize(frontend_config);
    }
    module
        .define_function(native_id, &mut context)
        .map_err(|error| native_error(format!("cannot compile `{}`: {error}", function.name)))?;
    module.clear_context(&mut context);
    Ok(())
}

pub(super) fn define_callable_thunks(
    module: &mut ObjectModule,
    backend: &NativeBackend<'_>,
) -> Result<(), FosterError> {
    for (layout, thunk_id) in ordered_entries(backend.callable_thunks) {
        let (target, capture_count) =
            concrete_closure_target(backend.objects.layouts, backend.ir.instances, layout)
                .ok_or_else(|| native_error("callable thunk has no concrete closure target"))?;
        let target_signature = &backend.ir.function_types[&target];
        let mut parameters = vec![NativeType::Object(layout)];
        parameters.extend_from_slice(&target_signature.parameters[capture_count..]);
        let thunk_signature = ir::Signature {
            parameters,
            result: target_signature.result,
        };
        let mut context = module.make_context();
        context.func.signature = signature(module, &thunk_signature);
        let frontend_config = module.target_config();
        let mut builder_context = FunctionBuilderContext::new();
        {
            let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
            let entry = builder.create_block();
            builder.append_block_params_for_function_params(entry);
            builder.switch_to_block(entry);
            let inputs = builder.block_params(entry).to_vec();
            let environment = inputs[0];
            let physical = backend.objects.layouts.physical.get(layout);
            let PhysicalKind::Closure { captures, .. } = &physical.kind else {
                return Err(native_error("callable thunk environment is not a closure"));
            };
            let mut arguments = Vec::with_capacity(captures.len() + inputs.len() - 1);
            for field in captures {
                let value = load_physical_value(
                    &mut builder,
                    module,
                    environment,
                    field.offset,
                    field.value,
                );
                if let Some(pointee) = field.value.pointee
                    && backend.objects.layouts.is_managed(pointee)
                {
                    backend.objects.retain(&mut builder, value, pointee);
                }
                arguments.push(value);
            }
            arguments.extend_from_slice(&inputs[1..]);
            let target = module.declare_func_in_func(backend.functions[&target], builder.func);
            let call = builder.ins().call(target, &arguments);
            propagate_native_failure(&mut builder, module)?;
            let results = builder.inst_results(call).to_vec();
            builder.ins().return_(&results);
            builder.seal_all_blocks();
            builder.finalize(frontend_config);
        }
        module
            .define_function(thunk_id, &mut context)
            .map_err(|error| {
                native_error(format!(
                    "cannot compile callable thunk l{}: {error}",
                    layout.0
                ))
            })?;
        module.clear_context(&mut context);
    }
    Ok(())
}

pub(super) fn native_to_remote_word(
    builder: &mut FunctionBuilder<'_>,
    module: &ObjectModule,
    value: ClifValue,
    ty: NativeType,
) -> ClifValue {
    match ty {
        NativeType::Int => value,
        NativeType::Float => builder
            .ins()
            .bitcast(types::I64, MemFlagsData::new(), value),
        NativeType::Unit | NativeType::Bool | NativeType::Byte => {
            builder.ins().uextend(types::I64, value)
        }
        NativeType::CodePoint => builder.ins().uextend(types::I64, value),
        NativeType::String | NativeType::Object(_) | NativeType::Opaque => {
            if module.target_config().pointer_type() == types::I64 {
                value
            } else {
                builder.ins().uextend(types::I64, value)
            }
        }
    }
}

pub(super) fn remote_word_to_native(
    builder: &mut FunctionBuilder<'_>,
    module: &ObjectModule,
    value: ClifValue,
    ty: NativeType,
) -> ClifValue {
    match ty {
        NativeType::Int => value,
        NativeType::Float => builder
            .ins()
            .bitcast(types::F64, MemFlagsData::new(), value),
        NativeType::Unit | NativeType::Bool | NativeType::Byte => {
            builder.ins().ireduce(types::I8, value)
        }
        NativeType::CodePoint => builder.ins().ireduce(types::I32, value),
        NativeType::String | NativeType::Object(_) | NativeType::Opaque => {
            if module.target_config().pointer_type() == types::I64 {
                value
            } else {
                builder
                    .ins()
                    .ireduce(module.target_config().pointer_type(), value)
            }
        }
    }
}

pub(super) fn define_remote_thunks(
    module: &mut ObjectModule,
    backend: &NativeBackend<'_>,
) -> Result<(), FosterError> {
    for (target, thunk_id) in ordered_entries(backend.remote_thunks) {
        let target_signature = &backend.ir.function_types[&target];
        let Some(&receiver_type) = target_signature.parameters.first() else {
            return Err(native_error("remote method has no receiver parameter"));
        };
        let mut context = module.make_context();
        context.func.signature = signature(
            module,
            &ir::Signature {
                parameters: vec![NativeType::Int, NativeType::Opaque, NativeType::Bool],
                result: NativeType::Int,
            },
        );
        let frontend_config = module.target_config();
        let mut builder_context = FunctionBuilderContext::new();
        {
            let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
            let entry = builder.create_block();
            builder.append_block_params_for_function_params(entry);
            builder.switch_to_block(entry);
            let state_word = builder.block_params(entry)[0];
            let state = remote_word_to_native(&mut builder, module, state_word, receiver_type);
            let argument_data = builder.block_params(entry)[1];
            let mut arguments = Vec::with_capacity(target_signature.parameters.len());
            arguments.push(state);
            for (index, ty) in target_signature
                .parameters
                .iter()
                .copied()
                .skip(1)
                .enumerate()
            {
                let word = builder.ins().load(
                    types::I64,
                    MemFlagsData::trusted(),
                    argument_data,
                    i32::try_from(index * 8)
                        .map_err(|_| native_error("remote argument frame exceeds i32 offsets"))?,
                );
                arguments.push(remote_word_to_native(&mut builder, module, word, ty));
            }
            let execute = builder.block_params(entry)[2];
            let run = builder.create_block();
            let discard = builder.create_block();
            builder.ins().brif(execute, run, &[], discard, &[]);
            builder.switch_to_block(discard);
            for (value, ty) in arguments
                .iter()
                .skip(1)
                .zip(target_signature.parameters.iter().skip(1))
            {
                if let Some(layout) = backend.objects.layouts.managed_layout(*ty) {
                    backend
                        .objects
                        .release(&mut builder, module, *value, layout)?;
                }
            }
            let zero = builder.ins().iconst(types::I64, 0);
            builder.ins().return_(&[zero]);
            builder.switch_to_block(run);
            if let Some(layout) = backend.objects.layouts.managed_layout(receiver_type) {
                backend.objects.retain(&mut builder, state, layout);
            }
            let target = module.declare_func_in_func(backend.functions[&target], builder.func);
            let call = builder.ins().call(target, &arguments);
            propagate_native_failure(&mut builder, module)?;
            let result = builder.inst_results(call)[0];
            let result =
                native_to_remote_word(&mut builder, module, result, target_signature.result);
            builder.ins().return_(&[result]);
            builder.seal_all_blocks();
            builder.finalize(frontend_config);
        }
        module
            .define_function(thunk_id, &mut context)
            .map_err(|error| {
                native_error(format!(
                    "cannot compile remote thunk for function #{}: {error}",
                    target.into_raw().into_u32()
                ))
            })?;
        module.clear_context(&mut context);
    }
    Ok(())
}
