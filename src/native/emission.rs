//! Object assembly consumes a prepared native program without repeating legalization.
use super::*;

/// Keep helper definitions stable despite randomized lookup-table iteration.
pub(super) fn ordered_entries<K: Copy + Ord, V: Copy>(map: &HashMap<K, V>) -> Vec<(K, V)> {
    let mut entries: Vec<_> = map.iter().map(|(&key, &value)| (key, value)).collect();
    entries.sort_unstable_by_key(|&(key, _)| key);
    entries
}

pub(super) fn emit_object(
    prepared: &NativeProgram<'_>,
    options: CompileOptions,
) -> Result<ObjectArtifact, FosterError> {
    let compilation = prepared.compilation;
    let program = &prepared.program;
    let layouts = &prepared.layouts;
    let physical_layouts = &prepared.physical_layouts;
    let main = prepared.main;
    let instances = &prepared.instances;
    let instance_ids = &prepared.instance_ids;
    let function_types = &prepared.function_types;
    let main_instance = instances
        .iter()
        .find(|instance| instance.key.function == main && instance.key.substitutions.is_empty())
        .expect("main specialization is reachable");
    let mut flag_builder = settings::builder();
    flag_builder
        .set("is_pic", "true")
        .map_err(|error| native_error(format!("cannot configure Cranelift PIC: {error}")))?;
    flag_builder
        .set("opt_level", if options.optimize { "speed" } else { "none" })
        .map_err(|error| {
            native_error(format!("cannot configure Cranelift optimization: {error}"))
        })?;
    let isa_builder = cranelift_native::builder()
        .map_err(|error| native_error(format!("host architecture is not supported: {error}")))?;
    let isa = isa_builder
        .finish(settings::Flags::new(flag_builder))
        .map_err(|error| native_error(format!("cannot create the native target: {error}")))?;
    let object_builder = ObjectBuilder::new(isa, "foster", default_libcall_names())
        .map_err(|error| native_error(format!("cannot create a native object: {error}")))?;
    let mut module = ObjectModule::new(object_builder);
    let pointer_size = u8::try_from(module.target_config().pointer_type().bytes())
        .map_err(|_| native_error("native target pointer size does not fit in u8"))?;
    let target_layout = TargetLayout::host();
    if target_layout.pointer_size() != pointer_size {
        return Err(native_error(
            "Cranelift host target disagrees with the compiler process pointer size",
        ));
    }
    let layout_descriptors = emit_layout_descriptors(&mut module, physical_layouts)?;

    let mut native_ids = HashMap::new();
    for instance in instances {
        let bytecode = &program.functions[&instance.key.function];
        let signature = signature(&mut module, &function_types[&instance.ir_function]);
        let linkage = if instance.key.function == main && instance.key.substitutions.is_empty() {
            Linkage::Export
        } else {
            Linkage::Local
        };
        let symbol = if instance.key.function == main && instance.key.substitutions.is_empty() {
            "foster_native_entry".to_owned()
        } else {
            format!("foster_fn_{}", instance.ir_function.into_raw().into_u32())
        };
        let id = module
            .declare_function(&symbol, linkage, &signature)
            .map_err(|error| {
                native_error(format!("cannot declare `{}`: {error}", bytecode.name))
            })?;
        native_ids.insert(instance.ir_function, id);
    }

    let native_layouts = NativeLayouts {
        program,
        logical: layouts,
        physical: physical_layouts,
    };
    let drop_ids = declare_layout_destructors(&mut module, physical_layouts)?;
    let callable_thunks =
        declare_callable_thunks(&mut module, native_layouts, instance_ids, function_types)?;
    let method_receivers = compilation
        .hir
        .functions
        .iter()
        .filter_map(|(function, declaration)| declaration.receiver.is_some().then_some(function))
        .collect::<HashSet<_>>();
    let remote_thunks = declare_remote_thunks(&mut module, instances, &method_receivers)?;
    let main_result = function_types[&main_instance.ir_function].result;
    let exported_result_layout = match main_result {
        NativeType::Object(layout) if native_layouts.is_managed(layout) => Some(layout),
        _ => None,
    };
    let release_thunks =
        declare_release_thunks(&mut module, native_layouts, exported_result_layout)?;
    define_layout_destructors(&mut module, native_layouts, &drop_ids)?;
    define_release_thunks(&mut module, native_layouts, &drop_ids, &release_thunks)?;

    let backend = NativeBackend {
        ir: prepared.environment(),
        functions: &native_ids,
        callable_thunks: &callable_thunks,
        remote_thunks: &remote_thunks,
        release_thunks: &release_thunks,
        objects: ObjectRuntime {
            layouts: native_layouts,
            descriptors: &layout_descriptors,
            destructors: &drop_ids,
        },
    };

    for function in &prepared.functions {
        define_function(
            &mut module,
            function,
            native_ids[&function.instance.ir_function],
            &backend,
        )?;
    }
    define_callable_thunks(&mut module, &backend)?;
    define_remote_thunks(&mut module, &backend)?;

    let bytes = module
        .finish()
        .emit()
        .map_err(|error| native_error(format!("cannot encode the native object: {error}")))?;
    Ok(ObjectArtifact {
        bytes,
        result: main_result,
        accepts_arguments: program.main_arguments,
        runtime_strings: prepared.runtime_strings.clone(),
        releases_result: exported_result_layout.is_some(),
    })
}

/// Emit deterministic read-only metadata for every physical object layout.
///
/// The records are intentionally versioned and contain no process addresses. Allocation lowering
/// can reference these symbols as object-header descriptors without making portable bytecode
/// target-dependent.
fn emit_layout_descriptors(
    module: &mut ObjectModule,
    layouts: &PhysicalRegistry,
) -> Result<HashMap<LayoutId, DataId>, FosterError> {
    let mut descriptors = HashMap::new();
    for layout in layouts
        .layouts()
        .iter()
        .filter(|layout| layout.materialized)
    {
        let symbol = format!("foster_layout_{}", layout.id.0);
        let data_id = module
            .declare_data(&symbol, Linkage::Local, false, false)
            .map_err(|error| native_error(format!("cannot declare `{symbol}`: {error}")))?;
        let mut description = DataDescription::new();
        description.define(layout.descriptor_bytes().into_boxed_slice());
        description.set_align(u64::from(layouts.target().pointer_align()));
        description.set_used(true);
        module
            .define_data(data_id, &description)
            .map_err(|error| native_error(format!("cannot define `{symbol}`: {error}")))?;
        descriptors.insert(layout.id, data_id);
    }
    Ok(descriptors)
}
