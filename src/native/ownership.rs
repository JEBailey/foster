//! Allocation, management policy, retain/release, and recursive destruction for native values.
use super::*;

/// How a value's representation participates in native lifetime management.
/// This is representation policy, not a claim that every SSA alias owns a reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryManagement {
    Trivial,
    BorrowedAddress,
    ManagedObject(LayoutId),
    /// Legacy String/Arguments and raw runtime pointers have no general retain/release protocol.
    UnmanagedRuntime,
}

/// Target-independent and target-specific layout information used while lowering objects.
#[derive(Clone, Copy)]
pub(super) struct NativeLayouts<'a> {
    pub(super) program: &'a Program,
    pub(super) logical: &'a LayoutRegistry,
    pub(super) physical: &'a PhysicalRegistry,
}

impl NativeLayouts<'_> {
    pub(super) fn management(self, ty: NativeType) -> MemoryManagement {
        match ty {
            NativeType::Object(layout) => {
                let object = self.logical.get(layout);
                if matches!(object.kind, LayoutKind::Pointer { .. }) {
                    MemoryManagement::BorrowedAddress
                } else if !object.materialized
                    || matches!(object.kind,
                    LayoutKind::Record { record, .. } if Some(record) == self.program.string_record)
                {
                    MemoryManagement::UnmanagedRuntime
                } else {
                    MemoryManagement::ManagedObject(layout)
                }
            }
            NativeType::String
            | NativeType::Arguments
            | NativeType::StringList
            | NativeType::Opaque => MemoryManagement::UnmanagedRuntime,
            _ => MemoryManagement::Trivial,
        }
    }

    pub(super) fn is_managed(self, layout: LayoutId) -> bool {
        matches!(
            self.management(NativeType::Object(layout)),
            MemoryManagement::ManagedObject(_)
        )
    }
}

/// Object-runtime symbols needed after portable IR has been legalized.
#[derive(Clone, Copy)]
pub(super) struct ObjectRuntime<'a> {
    pub(super) layouts: NativeLayouts<'a>,
    pub(super) descriptors: &'a HashMap<LayoutId, DataId>,
    pub(super) destructors: &'a HashMap<LayoutId, FuncId>,
}

impl ObjectRuntime<'_> {
    pub(super) fn allocate(
        self,
        builder: &mut FunctionBuilder<'_>,
        module: &mut ObjectModule,
        layout: LayoutId,
    ) -> Result<ClifValue, FosterError> {
        allocate_object(
            builder,
            module,
            layout,
            self.layouts.physical,
            self.descriptors,
        )
    }

    pub(super) fn retain(
        self,
        builder: &mut FunctionBuilder<'_>,
        object: ClifValue,
        layout: LayoutId,
    ) {
        retain_object(builder, object, layout, self.layouts.physical);
    }

    pub(super) fn release(
        self,
        builder: &mut FunctionBuilder<'_>,
        module: &mut ObjectModule,
        object: ClifValue,
        layout: LayoutId,
    ) -> Result<(), FosterError> {
        release_object(
            builder,
            module,
            object,
            layout,
            self.layouts.physical,
            self.destructors,
        )
    }
}

pub(super) fn declare_layout_destructors(
    module: &mut ObjectModule,
    layouts: &PhysicalRegistry,
) -> Result<HashMap<LayoutId, FuncId>, FosterError> {
    layouts
        .layouts()
        .iter()
        .filter(|layout| layout.materialized)
        .map(|layout| {
            let signature = signature(
                module,
                &ir::Signature {
                    parameters: vec![NativeType::Object(layout.id)],
                    result: NativeType::Unit,
                },
            );
            let name = format!("foster_drop_l{}", layout.id.0);
            let function = module
                .declare_function(&name, Linkage::Local, &signature)
                .map_err(|error| native_error(format!("cannot declare `{name}`: {error}")))?;
            Ok((layout.id, function))
        })
        .collect()
}

pub(super) fn declare_release_thunks(
    module: &mut ObjectModule,
    layouts: NativeLayouts<'_>,
    exported_result: Option<LayoutId>,
) -> Result<HashMap<LayoutId, FuncId>, FosterError> {
    let mut result = HashMap::new();
    for layout in layouts
        .physical
        .layouts()
        .iter()
        .filter(|layout| layout.materialized && layouts.is_managed(layout.id))
    {
        let thunk_signature = signature(
            module,
            &ir::Signature {
                parameters: vec![NativeType::Object(layout.id)],
                result: NativeType::Unit,
            },
        );
        let exported = exported_result == Some(layout.id);
        let name = if exported {
            "foster_native_release_result".to_owned()
        } else {
            format!("foster_release_l{}", layout.id.0)
        };
        let id = module
            .declare_function(
                &name,
                if exported {
                    Linkage::Export
                } else {
                    Linkage::Local
                },
                &thunk_signature,
            )
            .map_err(|error| native_error(format!("cannot declare `{name}`: {error}")))?;
        result.insert(layout.id, id);
    }
    Ok(result)
}

pub(super) fn define_layout_destructors(
    module: &mut ObjectModule,
    layouts: NativeLayouts<'_>,
    destructors: &HashMap<LayoutId, FuncId>,
) -> Result<(), FosterError> {
    for layout in layouts
        .physical
        .layouts()
        .iter()
        .filter(|layout| layout.materialized)
    {
        let mut context = module.make_context();
        context.func.signature = signature(
            module,
            &ir::Signature {
                parameters: vec![NativeType::Object(layout.id)],
                result: NativeType::Unit,
            },
        );
        let frontend_config = module.target_config();
        let mut builder_context = FunctionBuilderContext::new();
        {
            let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
            let entry = builder.create_block();
            builder.append_block_params_for_function_params(entry);
            builder.switch_to_block(entry);
            let object = builder.block_params(entry)[0];
            match &layout.drop_plan {
                DropPlan::Fields(fields) => {
                    lower_drop_fields(&mut builder, module, object, fields, layouts, destructors)?;
                }
                DropPlan::Variant {
                    tag_offset,
                    alternatives,
                } => {
                    let tag = builder.ins().load(
                        types::I32,
                        MemFlagsData::trusted(),
                        object,
                        *tag_offset as i32,
                    );
                    let finish = builder.create_block();
                    for alternative in alternatives {
                        let matched = builder.create_block();
                        let next = builder.create_block();
                        let is_match =
                            builder
                                .ins()
                                .icmp_imm_s(IntCC::Equal, tag, i64::from(alternative.tag));
                        builder.ins().brif(is_match, matched, &[], next, &[]);
                        builder.switch_to_block(matched);
                        lower_drop_fields(
                            &mut builder,
                            module,
                            object,
                            &alternative.fields,
                            layouts,
                            destructors,
                        )?;
                        builder.ins().jump(finish, &[]);
                        builder.switch_to_block(next);
                    }
                    builder.ins().jump(finish, &[]);
                    builder.switch_to_block(finish);
                }
                DropPlan::Buffer { element, .. } => {
                    lower_drop_buffer(
                        &mut builder,
                        module,
                        object,
                        layout,
                        *element,
                        layouts,
                        destructors,
                    )?;
                }
                DropPlan::Runtime => match layout.kind {
                    PhysicalKind::Handle { handle_offset, .. } => {
                        let helper = match &layouts.logical.get(layout.id).kind {
                            LayoutKind::Builtin {
                                ty: VerificationType::Remote(_),
                            } => abi::REMOTE_RELEASE,
                            LayoutKind::Builtin {
                                ty: VerificationType::Future(_),
                            } => abi::FUTURE_RELEASE,
                            _ => {
                                return Err(native_error(
                                    "runtime handle is neither Remote nor Future",
                                ));
                            }
                        };
                        let handle = builder.ins().load(
                            module.target_config().pointer_type(),
                            MemFlagsData::trusted(),
                            object,
                            handle_offset as i32,
                        );
                        runtime_call(
                            &mut builder,
                            module,
                            helper,
                            &ir::Signature {
                                parameters: vec![NativeType::Opaque],
                                result: NativeType::Unit,
                            },
                            &[handle],
                        )?;
                    }
                    PhysicalKind::Callable {
                        environment_offset,
                        release_offset,
                        ..
                    } => {
                        let word = module.target_config().pointer_type();
                        let environment = builder.ins().load(
                            word,
                            MemFlagsData::trusted(),
                            object,
                            environment_offset as i32,
                        );
                        let release = builder.ins().load(
                            word,
                            MemFlagsData::trusted(),
                            object,
                            release_offset as i32,
                        );
                        let release_signature = signature(
                            module,
                            &ir::Signature {
                                parameters: vec![NativeType::Opaque],
                                result: NativeType::Unit,
                            },
                        );
                        let release_signature = builder.func.import_signature(release_signature);
                        builder
                            .ins()
                            .call_indirect(release_signature, release, &[environment]);
                    }
                    PhysicalKind::Bytes {
                        data_offset,
                        length_offset,
                    } => {
                        let word = module.target_config().pointer_type();
                        let data = builder.ins().load(
                            word,
                            MemFlagsData::trusted(),
                            object,
                            data_offset as i32,
                        );
                        let length = builder.ins().load(
                            word,
                            MemFlagsData::trusted(),
                            object,
                            length_offset as i32,
                        );
                        let empty = builder.ins().icmp_imm_s(IntCC::Equal, length, 0);
                        let one = builder.ins().iconst(word, 1);
                        let size = builder.ins().select(empty, one, length);
                        let align = builder.ins().iconst(types::I64, 1);
                        runtime_call(
                            &mut builder,
                            module,
                            abi::DEALLOC,
                            &ir::Signature {
                                parameters: vec![
                                    NativeType::Opaque,
                                    NativeType::Int,
                                    NativeType::Int,
                                ],
                                result: NativeType::Unit,
                            },
                            &[data, size, align],
                        )?;
                    }
                    PhysicalKind::Opaque {
                        value_offset,
                        release_offset,
                        ..
                    } => {
                        let word = module.target_config().pointer_type();
                        let release = builder.ins().load(
                            word,
                            MemFlagsData::trusted(),
                            object,
                            release_offset as i32,
                        );
                        let has_release = builder.ins().icmp_imm_s(IntCC::NotEqual, release, 0);
                        let release_block = builder.create_block();
                        let finish = builder.create_block();
                        builder
                            .ins()
                            .brif(has_release, release_block, &[], finish, &[]);
                        builder.switch_to_block(release_block);
                        let value = builder.ins().load(
                            word,
                            MemFlagsData::trusted(),
                            object,
                            value_offset as i32,
                        );
                        let release_signature = signature(
                            module,
                            &ir::Signature {
                                parameters: vec![NativeType::Opaque],
                                result: NativeType::Unit,
                            },
                        );
                        let release_signature = builder.func.import_signature(release_signature);
                        builder
                            .ins()
                            .call_indirect(release_signature, release, &[value]);
                        builder.ins().jump(finish, &[]);
                        builder.switch_to_block(finish);
                    }
                    _ => {}
                },
                DropPlan::Trivial => {}
            }
            let size = builder.ins().iconst(types::I64, i64::from(layout.size));
            let align = builder.ins().iconst(types::I64, i64::from(layout.align));
            runtime_call(
                &mut builder,
                module,
                abi::DEALLOC,
                &ir::Signature {
                    parameters: vec![
                        NativeType::Object(layout.id),
                        NativeType::Int,
                        NativeType::Int,
                    ],
                    result: NativeType::Unit,
                },
                &[object, size, align],
            )?;
            let unit = builder.ins().iconst(types::I8, 0);
            builder.ins().return_(&[unit]);
            builder.seal_all_blocks();
            builder.finalize(frontend_config);
        }
        module
            .define_function(destructors[&layout.id], &mut context)
            .map_err(|error| {
                native_error(format!(
                    "cannot compile destructor for l{}: {error}",
                    layout.id.0
                ))
            })?;
        module.clear_context(&mut context);
    }
    Ok(())
}

pub(super) fn define_release_thunks(
    module: &mut ObjectModule,
    layouts: NativeLayouts<'_>,
    destructors: &HashMap<LayoutId, FuncId>,
    release_thunks: &HashMap<LayoutId, FuncId>,
) -> Result<(), FosterError> {
    for (layout, id) in ordered_entries(release_thunks) {
        let mut context = module.make_context();
        context.func.signature = signature(
            module,
            &ir::Signature {
                parameters: vec![NativeType::Object(layout)],
                result: NativeType::Unit,
            },
        );
        let frontend_config = module.target_config();
        let mut builder_context = FunctionBuilderContext::new();
        {
            let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
            let entry = builder.create_block();
            builder.append_block_params_for_function_params(entry);
            builder.switch_to_block(entry);
            let object = builder.block_params(entry)[0];
            release_object(
                &mut builder,
                module,
                object,
                layout,
                layouts.physical,
                destructors,
            )?;
            let unit = builder.ins().iconst(types::I8, 0);
            builder.ins().return_(&[unit]);
            builder.seal_all_blocks();
            builder.finalize(frontend_config);
        }
        module.define_function(id, &mut context).map_err(|error| {
            native_error(format!(
                "cannot compile release thunk l{}: {error}",
                layout.0
            ))
        })?;
        module.clear_context(&mut context);
    }
    Ok(())
}

pub(super) fn lower_drop_buffer(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    object: ClifValue,
    layout: &crate::codegen::layout::physical::PhysicalLayout,
    element: ValueLayout,
    layouts: NativeLayouts<'_>,
    destructors: &HashMap<LayoutId, FuncId>,
) -> Result<(), FosterError> {
    let PhysicalKind::Buffer {
        data_offset,
        length_offset,
        capacity_offset,
        ..
    } = layout.kind
    else {
        return Err(native_error("buffer drop plan has a non-buffer layout"));
    };
    let pointer_type = module.target_config().pointer_type();
    let data = builder.ins().load(
        pointer_type,
        MemFlagsData::trusted(),
        object,
        data_offset as i32,
    );
    let length = builder.ins().load(
        pointer_type,
        MemFlagsData::trusted(),
        object,
        length_offset as i32,
    );
    if let Some(pointee) = element.pointee
        && layouts.is_managed(pointee)
    {
        let loop_block = builder.create_block();
        let release = builder.create_block();
        let released = builder.create_block();
        builder.append_block_param(loop_block, pointer_type);
        let zero = builder.ins().iconst(pointer_type, 0);
        builder.ins().jump(loop_block, &[zero.into()]);
        builder.switch_to_block(loop_block);
        let index = builder.block_params(loop_block)[0];
        let done = builder
            .ins()
            .icmp(IntCC::UnsignedGreaterThanOrEqual, index, length);
        builder.ins().brif(done, released, &[], release, &[]);
        builder.switch_to_block(release);
        let offset = builder.ins().imul_imm_u(index, i64::from(element.size));
        let address = builder.ins().iadd(data, offset);
        let value = builder
            .ins()
            .load(pointer_type, MemFlagsData::trusted(), address, 0);
        release_object(
            builder,
            module,
            value,
            pointee,
            layouts.physical,
            destructors,
        )?;
        let next = builder.ins().iadd_imm_s(index, 1);
        builder.ins().jump(loop_block, &[next.into()]);
        builder.switch_to_block(released);
    }
    let capacity = builder.ins().load(
        pointer_type,
        MemFlagsData::trusted(),
        object,
        capacity_offset as i32,
    );
    let has_data = builder.ins().icmp_imm_s(IntCC::NotEqual, capacity, 0);
    let deallocate = builder.create_block();
    let finish = builder.create_block();
    builder.ins().brif(has_data, deallocate, &[], finish, &[]);
    builder.switch_to_block(deallocate);
    let size = builder.ins().imul_imm_u(capacity, i64::from(element.size));
    let align = builder.ins().iconst(types::I64, i64::from(element.align));
    runtime_call(
        builder,
        module,
        abi::DEALLOC,
        &ir::Signature {
            parameters: vec![NativeType::Opaque, NativeType::Int, NativeType::Int],
            result: NativeType::Unit,
        },
        &[data, size, align],
    )?;
    builder.ins().jump(finish, &[]);
    builder.switch_to_block(finish);
    Ok(())
}

pub(super) fn lower_drop_fields(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    object: ClifValue,
    fields: &[DropField],
    layouts: NativeLayouts<'_>,
    destructors: &HashMap<LayoutId, FuncId>,
) -> Result<(), FosterError> {
    let pointer_type = module.target_config().pointer_type();
    for field in fields {
        if !layouts.is_managed(field.pointee) {
            continue;
        }
        let child = builder.ins().load(
            pointer_type,
            MemFlagsData::trusted(),
            object,
            field.offset as i32,
        );
        release_object(
            builder,
            module,
            child,
            field.pointee,
            layouts.physical,
            destructors,
        )?;
    }
    Ok(())
}

pub(super) fn allocate_object(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    layout: LayoutId,
    physical_layouts: &PhysicalRegistry,
    descriptors: &HashMap<LayoutId, DataId>,
) -> Result<ClifValue, FosterError> {
    let physical = physical_layouts.get(layout);
    let size = builder.ins().iconst(types::I64, i64::from(physical.size));
    let align = builder.ins().iconst(types::I64, i64::from(physical.align));
    let object = runtime_call(
        builder,
        module,
        abi::ALLOC,
        &ir::Signature {
            parameters: vec![NativeType::Int, NativeType::Int],
            result: NativeType::Object(layout),
        },
        &[size, align],
    )?;
    let pointer_type = module.target_config().pointer_type();
    let descriptor = module.declare_data_in_func(descriptors[&layout], builder.func);
    let descriptor = builder.ins().symbol_value(pointer_type, descriptor);
    builder.ins().store(
        MemFlagsData::trusted(),
        descriptor,
        object,
        physical.header.descriptor_offset as i32,
    );
    let one = builder.ins().iconst(pointer_type, 1);
    builder.ins().store(
        MemFlagsData::trusted(),
        one,
        object,
        physical.header.strong_count_offset as i32,
    );
    let zero = builder.ins().iconst(types::I32, 0);
    builder.ins().store(
        MemFlagsData::trusted(),
        zero,
        object,
        physical.header.flags_offset as i32,
    );
    Ok(object)
}

pub(super) fn retain_object(
    builder: &mut FunctionBuilder<'_>,
    object: ClifValue,
    layout: LayoutId,
    physical_layouts: &PhysicalRegistry,
) {
    let physical = physical_layouts.get(layout);
    let pointer_type = builder.func.dfg.value_type(object);
    let count = builder.ins().load(
        pointer_type,
        MemFlagsData::trusted(),
        object,
        physical.header.strong_count_offset as i32,
    );
    let count = builder.ins().iadd_imm_s(count, 1);
    builder.ins().store(
        MemFlagsData::trusted(),
        count,
        object,
        physical.header.strong_count_offset as i32,
    );
}

pub(super) fn release_object(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    object: ClifValue,
    layout: LayoutId,
    physical_layouts: &PhysicalRegistry,
    destructors: &HashMap<LayoutId, FuncId>,
) -> Result<(), FosterError> {
    let physical = physical_layouts.get(layout);
    let pointer_type = builder.func.dfg.value_type(object);
    let count = builder.ins().load(
        pointer_type,
        MemFlagsData::trusted(),
        object,
        physical.header.strong_count_offset as i32,
    );
    let count = builder.ins().iadd_imm_s(count, -1);
    builder.ins().store(
        MemFlagsData::trusted(),
        count,
        object,
        physical.header.strong_count_offset as i32,
    );
    let is_zero = builder.ins().icmp_imm_s(IntCC::Equal, count, 0);
    let destroy = builder.create_block();
    let continuation = builder.create_block();
    builder.ins().brif(is_zero, destroy, &[], continuation, &[]);
    builder.switch_to_block(destroy);
    let destructor = module.declare_func_in_func(destructors[&layout], builder.func);
    builder.ins().call(destructor, &[object]);
    builder.ins().jump(continuation, &[]);
    builder.switch_to_block(continuation);
    Ok(())
}
