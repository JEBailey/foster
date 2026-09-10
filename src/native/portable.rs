//! Lower portable scalar, collection, and intrinsic operations to Cranelift.
use super::{
    BinaryOp, ClifValue, FosterError, FunctionBuilder, InstBuilder, IntCC, LayoutKind,
    MemFlagsData, Module, NativeHostArguments, NativeLowering, NativeType, ObjectModule,
    PhysicalKind, ScalarKind, SpecializationKey, ValueLayout, ValueSemantic, abi,
    allocate_native_buffer, allocate_native_byte_buffer, allocate_native_bytes,
    append_native_buffer, clone_native_buffer, contract_candidates, copy, copy_native_bytes,
    cranelift_type, dispatch, fail_if, ir, load_physical_value, lower_binary, lower_native_await,
    lower_native_field, lower_native_host_intrinsic, lower_native_remote_call,
    lower_native_spawn_remote, native_buffer_element_address, native_buffer_layout,
    native_bytes_layout, native_error, native_reference_receiver, native_verification_type,
    physical_cranelift_type, propagate_native_failure, push_native_buffer, runtime_call,
    runtime_signature, signature, store_physical_value, types, write_native_newline,
    write_native_separator, write_native_value, zero_i64,
};

pub(super) fn lower_portable_native(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    instruction: &ir::PortableInstruction,
    context: NativeLowering<'_, '_>,
) -> Result<Option<ClifValue>, FosterError> {
    let NativeLowering {
        function,
        values,
        homes,
        mutable_parameter_homes,
        backend,
    } = context;
    let objects = backend.objects;
    let get = |value: &ir::Value| values[value];
    match instruction {
        ir::PortableInstruction::Drop { value } => {
            if let Some(layout) = objects.layouts.managed_layout(function.value_type(*value)) {
                objects.release(builder, module, get(value), layout)?;
                propagate_native_failure(builder, module)?;
            }
            Ok(None)
        }
        ir::PortableInstruction::Move {
            destination,
            source,
        } => {
            let value = if function.value_type(*source) != function.value_type(*destination) {
                native_reference_receiver(
                    builder,
                    module,
                    get(source),
                    function.value_type(*source),
                    backend,
                )?
                .0
            } else {
                get(source)
            };
            if let Some(layout) = objects
                .layouts
                .managed_layout(function.value_type(*destination))
            {
                objects.retain(builder, value, layout);
            }
            Ok(Some(value))
        }
        ir::PortableInstruction::CopyOnWrite {
            destination,
            source,
        } => {
            let source_value = *source;
            let original = get(source);
            let source_type = function.value_type(*destination);
            let (source, object_type) =
                native_reference_receiver(builder, module, original, source_type, backend)?;
            let address = (source_type != object_type).then_some(original);
            let NativeType::Object(layout) = object_type else {
                return Err(native_error("copy-on-write requires a native object"));
            };
            let physical = objects.layouts.physical.get(layout);
            if address.is_none()
                && function.storage_hints[source_value.0 as usize]
                    .is_some_and(|home| mutable_parameter_homes.contains(&home))
            {
                return Ok(Some(source));
            }
            // A builder's unique storage can be mutated directly. Shared values still detach
            // before mutation so snapshots and borrowed callers retain value semantics.
            let pointer_type = module.target_config().pointer_type();
            let count_address = builder
                .ins()
                .iadd_imm_s(source, i64::from(physical.header.strong_count_offset));
            let count =
                builder
                    .ins()
                    .atomic_load(pointer_type, MemFlagsData::trusted(), count_address);
            let unique = builder.ins().icmp_imm_s(IntCC::Equal, count, 1);
            let detach = builder.create_block();
            let ready = builder.create_block();
            builder.append_block_param(ready, pointer_type);
            builder
                .ins()
                .brif(unique, ready, &[source.into()], detach, &[]);
            builder.switch_to_block(detach);
            let copied = match &physical.kind {
                PhysicalKind::Record { fields, .. } => {
                    let copied = objects.allocate(builder, module, layout)?;
                    if contract_candidates(crate::types::DEINIT_SLOT, object_type, &[], backend.ir)?
                        .iter()
                        .any(|candidate| candidate.layout == layout)
                    {
                        let inactive = builder.ins().iconst(types::I32, 1);
                        store_physical_value(
                            builder,
                            source,
                            physical.header.flags_offset,
                            inactive,
                        );
                    }
                    for field in fields {
                        let value =
                            load_physical_value(builder, module, source, field.offset, field.value);
                        if let Some(pointee) = field.value.pointee
                            && objects.layouts.is_managed(pointee)
                        {
                            objects.retain(builder, value, pointee);
                        }
                        store_physical_value(builder, copied, field.offset, value);
                    }
                    copied
                }
                PhysicalKind::Buffer { .. } => {
                    clone_native_buffer(builder, module, source, layout, objects)?
                }
                _ => {
                    return Err(native_error(
                        "native copy-on-write requires a record or buffer layout",
                    ));
                }
            };
            objects.release(builder, module, source, layout)?;
            builder.ins().jump(ready, &[copied.into()]);
            builder.switch_to_block(ready);
            let unique = builder.block_params(ready)[0];
            if let Some(address) = address {
                store_physical_value(builder, address, 0, unique);
                Ok(Some(address))
            } else {
                Ok(Some(unique))
            }
        }
        ir::PortableInstruction::SpawnRemote { destination, value } => {
            lower_native_spawn_remote(builder, module, *destination, *value, false, context)
                .map(Some)
        }
        ir::PortableInstruction::SpawnRemoteBorrow {
            destination,
            source,
        } => lower_native_spawn_remote(builder, module, *destination, *source, true, context)
            .map(Some),
        ir::PortableInstruction::RemoteCall {
            destination,
            remote,
            function: target,
            arguments,
        } => lower_native_remote_call(
            builder,
            module,
            *destination,
            *remote,
            *target,
            arguments,
            context,
        )
        .map(Some),
        ir::PortableInstruction::Await {
            destination,
            future,
        } => lower_native_await(builder, module, *destination, *future, context).map(Some),
        ir::PortableInstruction::MakeList {
            destination,
            elements,
            ..
        } => {
            let NativeType::Object(layout) = function.value_type(*destination) else {
                return Err(native_error("list result uses the wrong native layout"));
            };
            let object = allocate_native_buffer(builder, module, layout, elements.len(), objects)?;
            let PhysicalKind::Buffer {
                data_offset,
                element,
                ..
            } = objects.layouts.physical.get(layout).kind
            else {
                return Err(native_error("list result has a non-buffer layout"));
            };
            let data = load_physical_value(
                builder,
                module,
                object,
                data_offset,
                ValueLayout {
                    size: module.target_config().pointer_type().bytes(),
                    align: u16::try_from(module.target_config().pointer_type().bytes())
                        .expect("pointer alignment fits in u16"),
                    kind: ScalarKind::Pointer,
                    semantic: ValueSemantic::Object,
                    pointee: None,
                },
            );
            for (index, source) in elements.iter().enumerate() {
                let value = get(source);
                if let Some(pointee) = element.pointee
                    && objects.layouts.is_managed(pointee)
                {
                    objects.retain(builder, value, pointee);
                }
                store_physical_value(
                    builder,
                    data,
                    u32::try_from(index).unwrap_or(u32::MAX) * element.size,
                    value,
                );
            }
            Ok(Some(object))
        }
        ir::PortableInstruction::Index {
            destination,
            object,
            index,
        } if matches!(function.value_type(*object), NativeType::Object(_)) => {
            let NativeType::Object(layout) = function.value_type(*object) else {
                unreachable!()
            };
            let (address, element) = match objects.layouts.physical.get(layout).kind {
                PhysicalKind::Bytes {
                    data_offset,
                    length_offset,
                } => {
                    let word = module.target_config().pointer_type();
                    let length = builder.ins().load(
                        word,
                        MemFlagsData::trusted(),
                        get(object),
                        length_offset as i32,
                    );
                    let outside =
                        builder
                            .ins()
                            .icmp(IntCC::UnsignedGreaterThanOrEqual, get(index), length);
                    fail_if(
                        builder,
                        module,
                        outside,
                        abi::failure::INDEX_OUT_OF_BOUNDS,
                        get(index),
                        length,
                    )?;
                    let data = builder.ins().load(
                        word,
                        MemFlagsData::trusted(),
                        get(object),
                        data_offset as i32,
                    );
                    (
                        builder.ins().iadd(data, get(index)),
                        ValueLayout {
                            size: 1,
                            align: 1,
                            kind: ScalarKind::I8,
                            semantic: ValueSemantic::Byte,
                            pointee: None,
                        },
                    )
                }
                _ => native_buffer_element_address(
                    builder,
                    module,
                    get(object),
                    get(index),
                    layout,
                    objects,
                )?,
            };
            let value = builder.ins().load(
                physical_cranelift_type(element.kind, module.target_config().pointer_type()),
                MemFlagsData::trusted(),
                address,
                0,
            );
            if let Some(pointee) = objects
                .layouts
                .managed_layout(function.value_type(*destination))
            {
                objects.retain(builder, value, pointee);
            }
            Ok(Some(value))
        }
        ir::PortableInstruction::StoreIndex {
            object,
            index,
            source,
        } => {
            let (receiver, receiver_type) = native_reference_receiver(
                builder,
                module,
                get(object),
                function.value_type(*object),
                backend,
            )?;
            let NativeType::Object(layout) = receiver_type else {
                return Err(native_error("native indexed store requires a buffer"));
            };
            let (address, element) = native_buffer_element_address(
                builder,
                module,
                receiver,
                get(index),
                layout,
                objects,
            )?;
            if let Some(pointee) = element.pointee
                && objects.layouts.is_managed(pointee)
            {
                let old = builder.ins().load(
                    module.target_config().pointer_type(),
                    MemFlagsData::trusted(),
                    address,
                    0,
                );
                objects.release(builder, module, old, pointee)?;
                objects.retain(builder, get(source), pointee);
            }
            store_physical_value(builder, address, 0, get(source));
            Ok(None)
        }
        ir::PortableInstruction::Append {
            destination,
            object,
            value,
        } => {
            let NativeType::Object(layout) = function.value_type(*destination) else {
                return Err(native_error("native append requires a buffer"));
            };
            let appended =
                append_native_buffer(builder, module, get(object), get(value), layout, objects)?;
            Ok(Some(appended))
        }
        ir::PortableInstruction::Push { object, value, .. } => {
            let (object, object_type) = native_reference_receiver(
                builder,
                module,
                get(object),
                function.value_type(*object),
                backend,
            )?;
            let NativeType::Object(layout) = object_type else {
                return Err(native_error("native push requires a buffer"));
            };
            push_native_buffer(builder, module, object, get(value), layout, objects)?;
            Ok(Some(builder.ins().iconst(types::I8, 0)))
        }
        ir::PortableInstruction::Contains {
            value, candidates, ..
        } => {
            let mut matched = builder.ins().iconst(types::I8, 0);
            for candidate in candidates {
                let equal = lower_binary(
                    builder,
                    module,
                    BinaryOp::Equal,
                    function.value_type(*value),
                    get(value),
                    get(candidate),
                    objects.layouts.logical,
                )?;
                matched = builder.ins().bor(matched, equal);
            }
            Ok(Some(matched))
        }
        ir::PortableInstruction::MakeWholeReference { object, .. } => {
            if let NativeType::Object(layout) = function.value_type(*object)
                && matches!(
                    objects.layouts.logical.get(layout).kind,
                    LayoutKind::Pointer { .. }
                )
            {
                return Ok(Some(get(object)));
            }
            let home = function.storage_hints[object.0 as usize]
                .and_then(|home| homes.get(&home).copied())
                .ok_or_else(|| native_error("referenced value has no native storage home"))?;
            Ok(Some(builder.ins().stack_addr(
                module.target_config().pointer_type(),
                home,
                0,
            )))
        }
        ir::PortableInstruction::MakeReference { object, index, .. } => {
            let (object, object_type) = native_reference_receiver(
                builder,
                module,
                get(object),
                function.value_type(*object),
                backend,
            )?;
            let NativeType::Object(layout) = object_type else {
                return Err(native_error("indexed reference requires a native buffer"));
            };
            let (address, _) = native_buffer_element_address(
                builder,
                module,
                object,
                get(index),
                layout,
                objects,
            )?;
            Ok(Some(address))
        }
        ir::PortableInstruction::MakeFieldReference { object, field, .. }
        | ir::PortableInstruction::LoadField {
            object,
            field,
            by_reference: true,
            ..
        } => {
            let (object, object_type) = native_reference_receiver(
                builder,
                module,
                get(object),
                function.value_type(*object),
                backend,
            )?;
            let NativeType::Object(layout) = object_type else {
                return Err(native_error("field reference requires a native record"));
            };
            let LayoutKind::Record { fields, .. } = &objects.layouts.logical.get(layout).kind
            else {
                return Err(native_error("field reference requires a native record"));
            };
            let slot = fields
                .iter()
                .find(|slot| slot.name == *field)
                .ok_or_else(|| native_error(format!("record has no field `{field}`")))?;
            let physical = objects
                .layouts
                .physical
                .record_field(layout, slot.index)
                .ok_or_else(|| native_error("record field has no physical slot"))?;
            Ok(Some(
                builder.ins().iadd_imm_s(object, i64::from(physical.offset)),
            ))
        }
        ir::PortableInstruction::MoveOut {
            destination,
            source,
            by_reference,
        } => {
            if !by_reference {
                return Ok(Some(get(source)));
            }
            let ty = function.value_type(*destination);
            let lowered = cranelift_type(ty, module.target_config().pointer_type());
            let value = builder
                .ins()
                .load(lowered, MemFlagsData::trusted(), get(source), 0);
            let zero = match ty {
                NativeType::Float => builder.ins().f64const(0.0),
                _ => builder.ins().iconst(lowered, 0),
            };
            builder
                .ins()
                .store(MemFlagsData::trusted(), zero, get(source), 0);
            Ok(Some(value))
        }
        ir::PortableInstruction::MakeRecord {
            destination,
            record,
            type_arguments: _,
            fields: values_to_store,
        } => {
            let layout = objects
                .layouts
                .managed_layout(function.value_type(*destination))
                .ok_or_else(|| native_error("record result uses the wrong native layout"))?;
            let LayoutKind::Record {
                record: layout_record,
                ..
            } = objects.layouts.logical.get(layout).kind
            else {
                return Err(native_error("record has a non-record logical layout"));
            };
            if layout_record != *record {
                return Err(native_error("record result uses the wrong nominal layout"));
            }
            let physical = objects.layouts.physical.get(layout);
            let PhysicalKind::Record { fields, .. } = &physical.kind else {
                return Err(native_error("record has a non-record physical layout"));
            };
            let object = objects.allocate(builder, module, layout)?;
            for ((name, source), field) in values_to_store.iter().zip(fields) {
                if name != &field.name {
                    return Err(native_error(
                        "logical and physical record field order disagree",
                    ));
                }
                let value = get(source);
                if let Some(pointee) = objects.layouts.managed_layout(function.value_type(*source))
                {
                    objects.retain(builder, value, pointee);
                }
                store_physical_value(builder, object, field.offset, value);
            }
            Ok(Some(object))
        }
        ir::PortableInstruction::MakeVariant {
            destination,
            variant,
            type_arguments: _,
            payload,
        } => {
            let NativeType::Object(layout) = function.value_type(*destination) else {
                return Err(native_error("variant result uses the wrong native layout"));
            };
            let LayoutKind::Variant { alternatives, .. } =
                &objects.layouts.logical.get(layout).kind
            else {
                return Err(native_error("variant has a non-variant logical layout"));
            };
            let tag = alternatives
                .iter()
                .find(|alternative| alternative.variant == *variant)
                .map(|alternative| alternative.tag)
                .ok_or_else(|| native_error("variant construction has no native alternative"))?;
            let physical = objects.layouts.physical.get(layout);
            let PhysicalKind::Variant {
                tag_offset,
                alternatives,
                ..
            } = &physical.kind
            else {
                return Err(native_error("variant has a non-variant physical layout"));
            };
            let alternative = alternatives
                .iter()
                .find(|alternative| alternative.tag == tag)
                .ok_or_else(|| native_error("variant alternative has no physical layout"))?;
            if alternative.fields.len() != payload.len() {
                return Err(native_error(
                    "variant payload arity disagrees with its layout",
                ));
            }
            let object = objects.allocate(builder, module, layout)?;
            let tag = builder.ins().iconst(types::I32, i64::from(tag));
            builder
                .ins()
                .store(MemFlagsData::trusted(), tag, object, *tag_offset as i32);
            for (source, field) in payload.iter().zip(&alternative.fields) {
                let value = get(source);
                if let Some(pointee) = objects.layouts.managed_layout(function.value_type(*source))
                {
                    objects.retain(builder, value, pointee);
                }
                store_physical_value(builder, object, field.offset, value);
            }
            Ok(Some(object))
        }
        ir::PortableInstruction::MakeClosure {
            destination,
            function: target,
            captures,
            ..
        } => {
            let NativeType::Object(layout) = function.value_type(*destination) else {
                return Err(native_error("closure result uses the wrong native layout"));
            };
            let physical = objects.layouts.physical.get(layout);
            let PhysicalKind::Closure {
                code_offset,
                signature_offset,
                captures: fields,
            } = &physical.kind
            else {
                return Err(native_error("closure has a non-closure physical layout"));
            };
            if captures.len() != fields.len() {
                return Err(native_error("closure capture layout has the wrong arity"));
            }
            let object = objects.allocate(builder, module, layout)?;
            let reference = module.declare_func_in_func(backend.functions[target], builder.func);
            let code = builder
                .ins()
                .func_addr(module.target_config().pointer_type(), reference);
            store_physical_value(builder, object, *code_offset, code);
            let no_signature = builder
                .ins()
                .iconst(module.target_config().pointer_type(), 0);
            store_physical_value(builder, object, *signature_offset, no_signature);
            for ((_, source), field) in captures.iter().zip(fields) {
                store_physical_value(builder, object, field.offset, get(source));
            }
            Ok(Some(object))
        }
        ir::PortableInstruction::CallValue {
            destination: _,
            callee,
            arguments,
        } => {
            let NativeType::Object(layout) = function.value_type(*callee) else {
                return Err(native_error(
                    "dynamic call requires a concrete closure layout",
                ));
            };
            if let LayoutKind::Builtin {
                ty:
                    crate::vm::VerificationType::Function {
                        parameters, result, ..
                    },
            } = &objects.layouts.logical.get(layout).kind
            {
                let PhysicalKind::Callable {
                    code_offset,
                    environment_offset,
                    ..
                } = objects.layouts.physical.get(layout).kind
                else {
                    return Err(native_error("callable has the wrong physical layout"));
                };
                let callable = get(callee);
                let word = module.target_config().pointer_type();
                let code =
                    builder
                        .ins()
                        .load(word, MemFlagsData::trusted(), callable, code_offset as i32);
                let environment = builder.ins().load(
                    word,
                    MemFlagsData::trusted(),
                    callable,
                    environment_offset as i32,
                );
                let mut lowered = Vec::with_capacity(arguments.len() + 1);
                lowered.push(environment);
                lowered.extend(arguments.iter().map(get));
                let mut abi_parameters = vec![NativeType::Opaque];
                abi_parameters.extend(
                    parameters
                        .iter()
                        .map(|parameter| {
                            native_verification_type(
                                objects.layouts.program,
                                objects.layouts.logical,
                                parameter,
                                None,
                            )
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                );
                let result = native_verification_type(
                    objects.layouts.program,
                    objects.layouts.logical,
                    result,
                    None,
                )?;
                let signature = signature(
                    module,
                    &ir::Signature {
                        parameters: abi_parameters,
                        result,
                    },
                );
                let signature = builder.func.import_signature(signature);
                let call = builder.ins().call_indirect(signature, code, &lowered);
                propagate_native_failure(builder, module)?;
                return Ok(Some(builder.inst_results(call)[0]));
            }
            let LayoutKind::Closure {
                function: target,
                specialization,
                ..
            } = &objects.layouts.logical.get(layout).kind
            else {
                return Err(native_error("dynamic call target is not a closure layout"));
            };
            let target = backend.ir.instances[&SpecializationKey {
                function: *target,
                substitutions: specialization.clone(),
            }];
            let physical = objects.layouts.physical.get(layout);
            let PhysicalKind::Closure {
                code_offset,
                captures,
                ..
            } = &physical.kind
            else {
                return Err(native_error(
                    "dynamic call target is not a physical closure",
                ));
            };
            let closure = get(callee);
            let mut lowered = Vec::with_capacity(captures.len() + arguments.len());
            for field in captures {
                let value =
                    load_physical_value(builder, module, closure, field.offset, field.value);
                if let Some(pointee) = field.value.pointee
                    && objects.layouts.is_managed(pointee)
                {
                    objects.retain(builder, value, pointee);
                }
                lowered.push(value);
            }
            lowered.extend(arguments.iter().map(get));
            let code = builder.ins().load(
                module.target_config().pointer_type(),
                MemFlagsData::trusted(),
                closure,
                *code_offset as i32,
            );
            let signature = signature(module, &backend.ir.function_types[&target]);
            let signature = builder.func.import_signature(signature);
            let call = builder.ins().call_indirect(signature, code, &lowered);
            propagate_native_failure(builder, module)?;
            Ok(Some(builder.inst_results(call)[0]))
        }
        ir::PortableInstruction::CallContractMethod {
            destination,
            receiver,
            slot,
            name,
            arguments,
            ..
        } if *slot == crate::types::CAN_COPY_SLOT || *slot == crate::types::COPY_SLOT => {
            copy::lower(
                builder,
                module,
                get(receiver),
                function.value_type(*receiver),
                *slot == crate::types::CAN_COPY_SLOT,
                backend,
            )
            .map(Some)
        }
        ir::PortableInstruction::CallContractMethod {
            destination,
            receiver,
            slot,
            name,
            arguments,
            ..
        } => dispatch::lower(
            builder,
            module,
            get(receiver),
            function.value_type(*receiver),
            function.value_type(*destination),
            *slot,
            name,
            &arguments.iter().map(get).collect::<Vec<_>>(),
            &arguments
                .iter()
                .map(|argument| function.value_type(*argument))
                .collect::<Vec<_>>(),
            backend,
        )
        .map(Some),
        ir::PortableInstruction::LoadField {
            destination,
            object,
            field,
            by_reference: false,
        } => lower_native_field(
            builder,
            module,
            get(object),
            function.value_type(*object),
            function.value_type(*destination),
            field,
            objects,
        )
        .map(Some),
        ir::PortableInstruction::StoreField {
            object,
            field,
            source,
        } => {
            let (receiver, receiver_type) = native_reference_receiver(
                builder,
                module,
                get(object),
                function.value_type(*object),
                backend,
            )?;
            let NativeType::Object(layout) = receiver_type else {
                return Err(native_error("native field store requires a Foster object"));
            };
            let LayoutKind::Record { fields, .. } = &objects.layouts.logical.get(layout).kind
            else {
                return Err(native_error("native field store requires a record"));
            };
            let slot = fields
                .iter()
                .find(|slot| slot.name == *field)
                .ok_or_else(|| native_error(format!("record has no field `{field}`")))?;
            let physical = objects
                .layouts
                .physical
                .record_field(layout, slot.index)
                .ok_or_else(|| native_error("record field has no physical slot"))?;
            if let Some(pointee) = physical.value.pointee
                && objects.layouts.is_managed(pointee)
            {
                let old =
                    load_physical_value(builder, module, receiver, physical.offset, physical.value);
                objects.release(builder, module, old, pointee)?;
            }
            let source_value = get(source);
            if let Some(pointee) = objects.layouts.managed_layout(function.value_type(*source)) {
                objects.retain(builder, source_value, pointee);
            }
            store_physical_value(builder, receiver, physical.offset, source_value);
            Ok(None)
        }
        ir::PortableInstruction::Builtin {
            destination,
            builtin,
            arguments,
        } => {
            use crate::intrinsics::{NativeInlineIntrinsic, NativeIntrinsic};
            let lowered = arguments.iter().map(get).collect::<Vec<_>>();
            let result = match builtin.descriptor().native {
                NativeIntrinsic::Print { newline } => {
                    for (index, (argument, value)) in
                        arguments.iter().zip(lowered.iter().copied()).enumerate()
                    {
                        if index > 0 {
                            write_native_separator(builder, module)?;
                        }
                        write_native_value(builder, module, value, function.value_type(*argument))?;
                    }
                    if newline {
                        write_native_newline(builder, module)?;
                    }
                    builder.ins().iconst(types::I8, 0)
                }
                NativeIntrinsic::Inline(NativeInlineIntrinsic::IntegerToCodePoint) => {
                    let value = lowered[0];
                    let above =
                        builder
                            .ins()
                            .icmp_imm_u(IntCC::UnsignedGreaterThan, value, 0x10_ffff);
                    let below_surrogate =
                        builder
                            .ins()
                            .icmp_imm_s(IntCC::SignedLessThan, value, 0xd800);
                    let above_surrogate =
                        builder
                            .ins()
                            .icmp_imm_s(IntCC::SignedGreaterThan, value, 0xdfff);
                    let valid_surrogate = builder.ins().bor(below_surrogate, above_surrogate);
                    let invalid_surrogate = builder.ins().bxor_imm_u(valid_surrogate, 1);
                    let invalid = builder.ins().bor(above, invalid_surrogate);
                    let limit = zero_i64(builder);
                    fail_if(
                        builder,
                        module,
                        invalid,
                        abi::failure::INVALID_CODE_POINT,
                        value,
                        limit,
                    )?;
                    builder.ins().ireduce(types::I32, value)
                }
                NativeIntrinsic::Inline(NativeInlineIntrinsic::ByteIsValid) => builder
                    .ins()
                    .icmp_imm_u(IntCC::UnsignedLessThanOrEqual, lowered[0], 255),
                NativeIntrinsic::Inline(NativeInlineIntrinsic::IntegerToByte) => {
                    let invalid =
                        builder
                            .ins()
                            .icmp_imm_u(IntCC::UnsignedGreaterThan, lowered[0], 255);
                    let limit = builder.ins().iconst(types::I64, 255);
                    fail_if(
                        builder,
                        module,
                        invalid,
                        abi::failure::INVALID_BYTE,
                        lowered[0],
                        limit,
                    )?;
                    builder.ins().ireduce(types::I8, lowered[0])
                }
                NativeIntrinsic::Inline(NativeInlineIntrinsic::BytesFromList) => {
                    let NativeType::Object(result_layout) = function.value_type(*destination)
                    else {
                        return Err(native_error("Bytes result has no object layout"));
                    };
                    let NativeType::Object(source_layout) = function.value_type(arguments[0])
                    else {
                        return Err(native_error("Bytes.from requires a native byte list"));
                    };
                    let (source_data, length, _, element) =
                        native_buffer_layout(source_layout, objects)?;
                    if element.kind != ScalarKind::I8 {
                        return Err(native_error("Bytes.from requires List<Byte>"));
                    }
                    let source_data = builder.ins().load(
                        module.target_config().pointer_type(),
                        MemFlagsData::trusted(),
                        lowered[0],
                        source_data as i32,
                    );
                    let length = builder.ins().load(
                        module.target_config().pointer_type(),
                        MemFlagsData::trusted(),
                        lowered[0],
                        length as i32,
                    );
                    let (object, data) =
                        allocate_native_bytes(builder, module, objects, result_layout, length)?;
                    copy_native_bytes(builder, module, data, source_data, length)?;
                    object
                }
                NativeIntrinsic::Inline(NativeInlineIntrinsic::BytesToList) => {
                    let NativeType::Object(result_layout) = function.value_type(*destination)
                    else {
                        return Err(native_error("byte list result has no object layout"));
                    };
                    let NativeType::Object(source_layout) = function.value_type(arguments[0])
                    else {
                        return Err(native_error("Bytes.list requires native Bytes"));
                    };
                    let (source_data_offset, source_length_offset) =
                        native_bytes_layout(source_layout, objects)?;
                    let source_data = builder.ins().load(
                        module.target_config().pointer_type(),
                        MemFlagsData::trusted(),
                        lowered[0],
                        source_data_offset as i32,
                    );
                    let length = builder.ins().load(
                        module.target_config().pointer_type(),
                        MemFlagsData::trusted(),
                        lowered[0],
                        source_length_offset as i32,
                    );
                    let (object, data) = allocate_native_byte_buffer(
                        builder,
                        module,
                        objects,
                        result_layout,
                        length,
                    )?;
                    copy_native_bytes(builder, module, data, source_data, length)?;
                    object
                }
                NativeIntrinsic::Runtime(helper) => runtime_call(
                    builder,
                    module,
                    helper,
                    &runtime_signature(*destination, arguments, &function.value_types),
                    &lowered,
                )?,
                NativeIntrinsic::Host => lower_native_host_intrinsic(
                    builder,
                    module,
                    *builtin,
                    NativeHostArguments {
                        values: arguments,
                        lowered: &lowered,
                    },
                    function.value_type(*destination),
                    function,
                    objects,
                )?,
                NativeIntrinsic::Unavailable => {
                    return Err(native_error(format!(
                        "intrinsic `{builtin:?}` reached Cranelift without a native lowering"
                    )));
                }
            };
            Ok(Some(result))
        }
        unsupported => Err(native_error(format!(
            "portable operation reached Cranelift without native legalization: {unsupported:?}"
        ))),
    }
}
