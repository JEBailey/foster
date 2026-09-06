//! Optional Copy capability dispatch, preserving the concrete result representation.
use super::*;

pub(super) fn lower(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    receiver: ClifValue,
    ty: NativeType,
    query: bool,
    backend: &NativeBackend<'_>,
) -> Result<ClifValue, FosterError> {
    let trivial = matches!(
        ty,
        NativeType::Unit
            | NativeType::Bool
            | NativeType::Int
            | NativeType::Float
            | NativeType::CodePoint
            | NativeType::Byte
            | NativeType::String
    ) || matches!(ty, NativeType::Object(layout) if matches!(backend.ir.layouts.get(layout).kind, LayoutKind::Builtin { ty: VerificationType::Bytes }));
    if trivial {
        if query {
            return Ok(builder.ins().iconst(types::I8, 1));
        }
        if let Some(layout) = backend.objects.layouts.managed_layout(ty) {
            backend.objects.retain(builder, receiver, layout);
        }
        return Ok(receiver);
    }
    let word = module.target_config().pointer_type();
    let result_type = if query {
        types::I8
    } else {
        cranelift_type(ty, word)
    };
    let join = builder.create_block();
    builder.append_block_param(join, result_type);
    let opaque = match ty {
        NativeType::Object(layout) => match backend.ir.physical_layouts.get(layout).kind {
            PhysicalKind::Opaque {
                value_offset,
                release_offset,
                semantic_offset,
                ..
            } => Some((layout, value_offset, release_offset, semantic_offset)),
            _ => None,
        },
        _ => None,
    };
    let payload = if let Some((layout, value_offset, _, semantic_offset)) = opaque {
        let semantic = builder.ins().load(
            types::I8,
            MemFlagsData::trusted(),
            receiver,
            semantic_offset as i32,
        );
        let scalar = builder.ins().icmp_imm_u(
            IntCC::UnsignedLessThanOrEqual,
            semantic,
            ValueSemantic::Symbol as i64,
        );
        let primitive = builder.create_block();
        let object = builder.create_block();
        let absent = builder.create_block();
        builder.ins().brif(scalar, primitive, &[], absent, &[]);
        builder.switch_to_block(primitive);
        let result = if query {
            builder.ins().iconst(types::I8, 1)
        } else {
            backend.objects.retain(builder, receiver, layout);
            receiver
        };
        builder.ins().jump(join, &[result.into()]);
        builder.switch_to_block(absent);
        let managed =
            builder
                .ins()
                .icmp_imm_s(IntCC::Equal, semantic, ValueSemantic::Object as i64);
        let unsupported = builder.create_block();
        builder.ins().brif(managed, object, &[], unsupported, &[]);
        builder.switch_to_block(unsupported);
        let zero = builder.ins().iconst(result_type, 0);
        builder.ins().jump(join, &[zero.into()]);
        builder.switch_to_block(object);
        builder
            .ins()
            .load(word, MemFlagsData::trusted(), receiver, value_offset as i32)
    } else {
        receiver
    };
    // Erased Bytes values retain immutable storage just like statically typed Bytes.
    if let Some((boxed_layout, ..)) = opaque {
        let descriptor = builder.ins().load(
            word,
            MemFlagsData::trusted(),
            payload,
            backend.ir.physical_layouts.header().descriptor_offset as i32,
        );
        for layout in backend.ir.layouts.layouts().iter().filter(|layout| {
            layout.materialized
                && matches!(
                    layout.kind,
                    LayoutKind::Builtin {
                        ty: VerificationType::Bytes
                    }
                )
        }) {
            let expected =
                module.declare_data_in_func(backend.objects.descriptors[&layout.id], builder.func);
            let expected = builder.ins().symbol_value(word, expected);
            let matched = builder.ins().icmp(IntCC::Equal, descriptor, expected);
            let copied = builder.create_block();
            let next = builder.create_block();
            builder.ins().brif(matched, copied, &[], next, &[]);
            builder.switch_to_block(copied);
            let result = if query {
                builder.ins().iconst(types::I8, 1)
            } else {
                backend.objects.retain(builder, receiver, boxed_layout);
                receiver
            };
            builder.ins().jump(join, &[result.into()]);
            builder.switch_to_block(next);
        }
    }
    let candidates = contract_candidates(crate::types::COPY_SLOT, ty, &[], backend.ir)?;
    if !candidates.is_empty() {
        let descriptor = builder.ins().load(
            word,
            MemFlagsData::trusted(),
            payload,
            backend.ir.physical_layouts.header().descriptor_offset as i32,
        );
        for candidate in candidates {
            let call = builder.create_block();
            let next = builder.create_block();
            let expected = module
                .declare_data_in_func(backend.objects.descriptors[&candidate.layout], builder.func);
            let expected = builder.ins().symbol_value(word, expected);
            let matches = builder.ins().icmp(IntCC::Equal, descriptor, expected);
            builder.ins().brif(matches, call, &[], next, &[]);
            builder.switch_to_block(call);
            let result = if query {
                builder.ins().iconst(types::I8, 1)
            } else {
                backend.objects.retain(builder, payload, candidate.layout);
                let target = module
                    .declare_func_in_func(backend.functions[&candidate.function], builder.func);
                let call = builder.ins().call(target, &[payload]);
                propagate_native_failure(builder, module)?;
                let copied = builder.inst_results(call)[0];
                if let Some((layout, value_offset, release_offset, semantic_offset)) = opaque {
                    let boxed = backend.objects.allocate(builder, module, layout)?;
                    store_physical_value(builder, boxed, value_offset, copied);
                    let release = builder.ins().load(
                        word,
                        MemFlagsData::trusted(),
                        receiver,
                        release_offset as i32,
                    );
                    store_physical_value(builder, boxed, release_offset, release);
                    let semantic = builder
                        .ins()
                        .iconst(types::I8, ValueSemantic::Object as i64);
                    store_physical_value(builder, boxed, semantic_offset, semantic);
                    boxed
                } else {
                    copied
                }
            };
            builder.ins().jump(join, &[result.into()]);
            builder.switch_to_block(next);
        }
    }
    let zero = builder.ins().iconst(result_type, 0);
    builder.ins().jump(join, &[zero.into()]);
    builder.switch_to_block(join);
    Ok(builder.block_params(join)[0])
}
