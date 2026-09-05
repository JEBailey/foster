//! Generated storage adapters at the host/text boundary. Rust never owns a Foster String.
use super::*;

pub(super) fn define(
    module: &mut ObjectModule,
    backend: &NativeBackend<'_>,
) -> Result<(), FosterError> {
    let objects = backend.objects;
    let string = objects.layouts.string_layout();
    let field = objects
        .layouts
        .physical
        .record_field(string, 0)
        .ok_or_else(|| native_error("String requires byte storage"))?;
    let bytes = objects
        .layouts
        .logical
        .builtin(&VerificationType::Bytes)
        .ok_or_else(|| native_error("String requires Bytes layout"))?;
    let (data_offset, length_offset) = native_bytes_layout(bytes, objects)?;
    if field.value.pointee != Some(bytes) {
        return Err(native_error(
            "String storage must use the managed Bytes layout",
        ));
    }
    for (name, parameters, result) in [
        (
            "foster_native_string",
            vec![NativeType::Opaque, NativeType::Int],
            NativeType::String,
        ),
        (
            "foster_native_string_data",
            vec![NativeType::String],
            NativeType::Opaque,
        ),
        (
            "foster_native_string_length",
            vec![NativeType::String],
            NativeType::Int,
        ),
    ] {
        let mut context = module.make_context();
        context.func.signature = signature(module, &ir::Signature { parameters, result });
        let id = module
            .declare_function(name, Linkage::Export, &context.func.signature)
            .map_err(|error| native_error(format!("cannot declare {name}: {error}")))?;
        let mut frontend = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut context.func, &mut frontend);
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        let input = builder.block_params(entry).to_vec();
        let value = if name == "foster_native_string" {
            let (storage, data) =
                allocate_native_bytes(&mut builder, module, objects, bytes, input[1])?;
            copy_native_bytes(&mut builder, module, data, input[0], input[1])?;
            let text = objects.allocate(&mut builder, module, string)?;
            store_physical_value(&mut builder, text, field.offset, storage);
            text
        } else {
            let word = module.target_config().pointer_type();
            let storage =
                builder
                    .ins()
                    .load(word, MemFlagsData::trusted(), input[0], field.offset as i32);
            builder.ins().load(
                word,
                MemFlagsData::trusted(),
                storage,
                if name == "foster_native_string_data" {
                    data_offset
                } else {
                    length_offset
                } as i32,
            )
        };
        builder.ins().return_(&[value]);
        builder.seal_all_blocks();
        builder.finalize(module.target_config());
        module
            .define_function(id, &mut context)
            .map_err(|error| native_error(format!("cannot define {name}: {error}")))?;
    }
    if backend.ir.program.main_arguments {
        define_arguments(module, backend)?;
    }
    Ok(())
}

/// Transfer host-imported strings into an ordinary Arguments record and List<String>.
fn define_arguments(
    module: &mut ObjectModule,
    backend: &NativeBackend<'_>,
) -> Result<(), FosterError> {
    let objects = backend.objects;
    let main = backend.ir.program.main.expect("native entry");
    let ty = &backend.ir.program.functions[&main].parameter_types[0];
    let NativeType::Object(layout) =
        native_verification_type(backend.ir.program, backend.ir.layouts, ty, None)?
    else {
        return Err(native_error("Arguments must have a record layout"));
    };
    let PhysicalKind::Record { fields, .. } = &objects.layouts.physical.get(layout).kind else {
        return Err(native_error("Arguments must be a record"));
    };
    let executable = fields
        .iter()
        .find(|field| field.name == "executable")
        .ok_or_else(|| native_error("Arguments.executable is missing"))?;
    let values = fields
        .iter()
        .find(|field| field.name == "values")
        .ok_or_else(|| native_error("Arguments.values is missing"))?;
    let list = values
        .value
        .pointee
        .ok_or_else(|| native_error("Arguments.values must be a list"))?;
    let (data_offset, length_offset, capacity_offset, element) =
        native_buffer_layout(list, objects)?;
    if fields.len() != 2
        || executable.value.semantic != ValueSemantic::String
        || element.semantic != ValueSemantic::String
        || element.size != module.target_config().pointer_type().bytes()
    {
        return Err(native_error(
            "Arguments requires String executable and List<String> values",
        ));
    }
    let mut context = module.make_context();
    context.func.signature = signature(
        module,
        &ir::Signature {
            parameters: vec![NativeType::String, NativeType::Opaque, NativeType::Int],
            result: NativeType::Object(layout),
        },
    );
    let id = module
        .declare_function(
            "foster_native_arguments",
            Linkage::Export,
            &context.func.signature,
        )
        .map_err(|error| native_error(format!("cannot declare argument importer: {error}")))?;
    let mut frontend = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut context.func, &mut frontend);
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    let input = builder.block_params(entry).to_vec();
    let object = objects.allocate(&mut builder, module, layout)?;
    let items = objects.allocate(&mut builder, module, list)?;
    let empty = builder.ins().icmp_imm_s(IntCC::Equal, input[2], 0);
    let one = builder.ins().iconst(types::I64, 1);
    let capacity = builder.ins().select(empty, one, input[2]);
    let size = builder.ins().imul_imm_s(capacity, i64::from(element.size));
    let align = builder.ins().iconst(types::I64, i64::from(element.align));
    let data = runtime_call(
        &mut builder,
        module,
        abi::ALLOC,
        &ir::Signature {
            parameters: vec![NativeType::Int, NativeType::Int],
            result: NativeType::Opaque,
        },
        &[size, align],
    )?;
    let used = builder.ins().imul_imm_s(input[2], i64::from(element.size));
    copy_native_bytes(&mut builder, module, data, input[1], used)?;
    store_physical_value(&mut builder, items, data_offset, data);
    store_physical_value(&mut builder, items, length_offset, input[2]);
    store_physical_value(&mut builder, items, capacity_offset, capacity);
    store_physical_value(&mut builder, object, executable.offset, input[0]);
    store_physical_value(&mut builder, object, values.offset, items);
    builder.ins().return_(&[object]);
    builder.seal_all_blocks();
    builder.finalize(module.target_config());
    module
        .define_function(id, &mut context)
        .map_err(|error| native_error(format!("cannot define argument importer: {error}")))?;
    Ok(())
}
