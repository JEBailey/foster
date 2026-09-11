//! Descriptor dispatch for explicit methods and structural sequence accessors.
use super::*;

struct Candidate {
    layout: LayoutId,
    receiver: NativeType,
    result: NativeType,
    method: Option<ContractCandidate>,
}

fn opaque(ty: NativeType, layouts: &LayoutRegistry) -> bool {
    matches!(ty, NativeType::Object(layout) if matches!(layouts.get(layout).kind, LayoutKind::Opaque))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn lower(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    receiver: ClifValue,
    receiver_type: NativeType,
    result_type: NativeType,
    slot: crate::types::DispatchSlot,
    name: &str,
    arguments: &[ClifValue],
    argument_types: &[NativeType],
    backend: &NativeBackend<'_>,
) -> Result<ClifValue, FosterError> {
    let environment = backend.ir;
    let (receiver, receiver_type) =
        native_reference_receiver(builder, module, receiver, receiver_type, backend)?;
    let mut candidates = contract_candidates(slot, receiver_type, argument_types, environment)?
        .into_iter()
        .map(|method| Candidate {
            layout: method.layout,
            receiver: environment.function_types[&method.function].parameters[0],
            result: environment.function_types[&method.function].result,
            method: Some(method),
        })
        .collect::<Vec<_>>();
    if arguments.is_empty() {
        if receiver_type == NativeType::String && candidates.is_empty() {
            let actual = field_type(
                environment.program,
                environment.layouts,
                environment.physical_layouts,
                receiver_type,
                name,
            )?;
            let value = getter(
                builder,
                module,
                receiver,
                receiver_type,
                actual,
                name,
                backend,
            )?;
            return adapt_result(builder, module, value, actual, result_type, backend);
        }
        for layout in environment
            .layouts
            .layouts()
            .iter()
            .filter(|layout| layout.materialized)
        {
            if !opaque(receiver_type, environment.layouts)
                && receiver_type != NativeType::Object(layout.id)
            {
                continue;
            }
            // A declared implementation takes precedence over a stored/computed accessor.
            if candidates
                .iter()
                .any(|candidate| candidate.layout == layout.id)
            {
                continue;
            }
            let ty = match &layout.kind {
                LayoutKind::Record { record, .. }
                    if Some(*record) == environment.program.string_record =>
                {
                    NativeType::String
                }
                LayoutKind::Record { .. }
                | LayoutKind::Builtin {
                    ty:
                        VerificationType::List(_)
                        | VerificationType::Bytes
                        | VerificationType::ByteBuffer,
                } => NativeType::Object(layout.id),
                _ => continue,
            };
            if let Ok(result) = field_type(
                environment.program,
                environment.layouts,
                environment.physical_layouts,
                ty,
                name,
            ) {
                candidates.push(Candidate {
                    layout: layout.id,
                    receiver: ty,
                    result,
                    method: None,
                });
            }
        }
    }
    // The call's checked result type distinguishes, for example, Iterator<Int> from
    // Iterator<String> when both implementations occur in the same native program.
    candidates.retain(|candidate| {
        candidate.result == result_type
            || opaque(result_type, environment.layouts)
            || opaque(candidate.result, environment.layouts)
            || contract_argument_matches(candidate.result, result_type, environment)
    });
    candidates.sort_by_key(|candidate| candidate.layout);
    if candidates.is_empty() {
        return Err(native_error(format!(
            "value {receiver_type:?} has no native implementation of required method `{name}` returning {result_type:?}"
        )));
    }
    let word = module.target_config().pointer_type();
    let failure = builder.create_block();
    let payload = if opaque(receiver_type, environment.layouts) {
        let NativeType::Object(layout) = receiver_type else {
            unreachable!()
        };
        let PhysicalKind::Opaque {
            value_offset,
            semantic_offset,
            ..
        } = environment.physical_layouts.get(layout).kind
        else {
            unreachable!()
        };
        let semantic = builder.ins().load(
            types::I8,
            MemFlagsData::trusted(),
            receiver,
            semantic_offset as i32,
        );
        let object = builder
            .ins()
            .icmp_imm_s(IntCC::Equal, semantic, ValueSemantic::Object as i64);
        let string = builder
            .ins()
            .icmp_imm_s(IntCC::Equal, semantic, ValueSemantic::String as i64);
        let backed = builder.ins().bor(object, string);
        let valid = builder.create_block();
        builder.ins().brif(backed, valid, &[], failure, &[]);
        builder.switch_to_block(valid);
        builder
            .ins()
            .load(word, MemFlagsData::trusted(), receiver, value_offset as i32)
    } else {
        receiver
    };
    let descriptor = builder.ins().load(
        word,
        MemFlagsData::trusted(),
        payload,
        environment.physical_layouts.header().descriptor_offset as i32,
    );
    let join = builder.create_block();
    builder.append_block_param(join, cranelift_type(result_type, word));
    for candidate in candidates {
        let selected = builder.create_block();
        let next = builder.create_block();
        let expected = module
            .declare_data_in_func(backend.objects.descriptors[&candidate.layout], builder.func);
        let expected = builder.ins().symbol_value(word, expected);
        let matches = builder.ins().icmp(IntCC::Equal, descriptor, expected);
        builder.ins().brif(matches, selected, &[], next, &[]);
        builder.switch_to_block(selected);
        let value = if let Some(method) = candidate.method {
            backend.objects.retain(builder, payload, candidate.layout);
            let mut lowered = vec![payload];
            lowered.extend_from_slice(arguments);
            let target =
                module.declare_func_in_func(backend.functions[&method.function], builder.func);
            let call = builder.ins().call(target, &lowered);
            propagate_native_failure(builder, module)?;
            builder.inst_results(call)[0]
        } else {
            getter(
                builder,
                module,
                payload,
                candidate.receiver,
                candidate.result,
                name,
                backend,
            )?
        };
        let value = adapt_result(
            builder,
            module,
            value,
            candidate.result,
            result_type,
            backend,
        )?;
        builder.ins().jump(join, &[value.into()]);
        builder.switch_to_block(next);
    }
    builder.ins().jump(failure, &[]);
    builder.switch_to_block(failure);
    let kind = builder
        .ins()
        .iconst(types::I64, abi::failure::CONTRACT_DISPATCH);
    let detail = builder.ins().iconst(types::I64, i64::from(slot.0));
    let limit = zero_i64(builder);
    runtime_call(
        builder,
        module,
        abi::FAIL,
        &ir::Signature {
            parameters: vec![NativeType::Int, NativeType::Int, NativeType::Int],
            result: NativeType::Unit,
        },
        &[kind, detail, limit],
    )?;
    propagate_native_failure(builder, module)?;
    let zero = if result_type == NativeType::Float {
        builder.ins().f64const(0.0)
    } else {
        builder.ins().iconst(cranelift_type(result_type, word), 0)
    };
    builder.ins().jump(join, &[zero.into()]);
    builder.switch_to_block(join);
    Ok(builder.block_params(join)[0])
}

fn getter(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    receiver: ClifValue,
    receiver_type: NativeType,
    result_type: NativeType,
    name: &str,
    backend: &NativeBackend<'_>,
) -> Result<ClifValue, FosterError> {
    if receiver_type == NativeType::String {
        let value = runtime_call(
            builder,
            module,
            native_field_helper(receiver_type, name)?,
            &ir::Signature {
                parameters: vec![receiver_type],
                result: result_type,
            },
            &[receiver],
        )?;
        propagate_native_failure(builder, module)?;
        Ok(value)
    } else {
        lower_native_field(
            builder,
            module,
            receiver,
            receiver_type,
            result_type,
            name,
            backend.objects,
        )
    }
}

/// Transfer an owned method/accessor result into the representation promised by its contract.
fn adapt_result(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    value: ClifValue,
    actual: NativeType,
    expected: NativeType,
    backend: &NativeBackend<'_>,
) -> Result<ClifValue, FosterError> {
    if callable_conversion(actual, expected, backend.ir.layouts) {
        let (NativeType::Object(environment_layout), NativeType::Object(callable_layout)) =
            (actual, expected)
        else {
            unreachable!()
        };
        let PhysicalKind::Callable {
            code_offset,
            environment_offset,
            release_offset,
        } = backend.ir.physical_layouts.get(callable_layout).kind
        else {
            return Err(native_error(
                "dispatched callable result has the wrong layout",
            ));
        };
        let object = backend.objects.allocate(builder, module, callable_layout)?;
        let word = module.target_config().pointer_type();
        let code =
            module.declare_func_in_func(backend.callable_thunks[&environment_layout], builder.func);
        let code = builder.ins().func_addr(word, code);
        let release =
            module.declare_func_in_func(backend.release_thunks[&environment_layout], builder.func);
        let release = builder.ins().func_addr(word, release);
        store_physical_value(builder, object, code_offset, code);
        // The method returned an owned environment; transfer that reference.
        store_physical_value(builder, object, environment_offset, value);
        store_physical_value(builder, object, release_offset, release);
        return Ok(object);
    }
    let Some(conversion) = erased_conversion(actual, expected, backend.ir.layouts) else {
        return Ok(value);
    };
    let layout = backend.ir.layouts.opaque();
    let PhysicalKind::Opaque {
        value_offset,
        release_offset,
        semantic_offset,
        ..
    } = backend.ir.physical_layouts.get(layout).kind
    else {
        unreachable!()
    };
    let word = module.target_config().pointer_type();
    match conversion {
        ErasedConversion::Box => {
            let object = backend.objects.allocate(builder, module, layout)?;
            store_physical_value(builder, object, value_offset, value);
            let release = if let Some(layout) = backend.objects.layouts.managed_layout(actual) {
                let release =
                    module.declare_func_in_func(backend.release_thunks[&layout], builder.func);
                builder.ins().func_addr(word, release)
            } else {
                builder.ins().iconst(word, 0)
            };
            store_physical_value(builder, object, release_offset, release);
            let semantic = builder.ins().iconst(
                types::I8,
                native_type_semantic(actual, backend.ir.layouts) as i64,
            );
            store_physical_value(builder, object, semantic_offset, semantic);
            Ok(object)
        }
        ErasedConversion::Unbox => {
            let result = builder.ins().load(
                cranelift_type(expected, word),
                MemFlagsData::trusted(),
                value,
                value_offset as i32,
            );
            if let Some(layout) = backend.objects.layouts.managed_layout(expected) {
                backend.objects.retain(builder, result, layout);
            }
            backend.objects.release(builder, module, value, layout)?;
            propagate_native_failure(builder, module)?;
            Ok(result)
        }
    }
}
