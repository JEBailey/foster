//! Marshal native host calls and decode their responses.
use super::{
    ClifValue, FosterError, FunctionBuilder, InstBuilder, IntCC, LayoutId, Linkage, MemFlagsData,
    Module, NativeType, ObjectModule, ObjectRuntime, PhysicalKind, ValueLayout, ValueSemantic, abi,
    allocate_native_buffer, allocate_native_buffer_dynamic, allocate_native_bytes, ir,
    native_buffer_layout, native_bytes_layout, native_error, propagate_native_failure,
    runtime_call, signature, store_physical_value, types,
};

#[derive(Clone, Copy)]
pub(super) struct NativeHostArguments<'a> {
    pub(super) values: &'a [ir::Value],
    pub(super) lowered: &'a [ClifValue],
}

pub(super) fn lower_native_host_intrinsic(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    builtin: crate::intrinsics::Builtin,
    arguments: NativeHostArguments<'_>,
    result_type: NativeType,
    function: &ir::Function,
    objects: ObjectRuntime<'_>,
) -> Result<ClifValue, FosterError> {
    use crate::intrinsics::Builtin;

    let response = call_native_host(builder, module, builtin, arguments, function, objects)?;
    match builtin {
        Builtin::IoExists | Builtin::IoIsFile | Builtin::IoIsDirectory => {
            require_native_host_response(builder, module, response)?;
            let value = native_host_integer(builder, module, response, 0)?;
            let value = builder.ins().icmp_imm_s(IntCC::NotEqual, value, 0);
            release_native_host_response(builder, module, response)?;
            Ok(value)
        }
        Builtin::IoJoin | Builtin::IoParent | Builtin::IoFileName | Builtin::IoExtension => {
            require_native_host_response(builder, module, response)?;
            let value = native_host_string(builder, module, response, abi::host_string::VALUE, 0)?;
            release_native_host_response(builder, module, response)?;
            Ok(value)
        }
        Builtin::TimeMonotonicNow => {
            require_native_host_response(builder, module, response)?;
            let value = native_host_integer(builder, module, response, 0)?;
            release_native_host_response(builder, module, response)?;
            Ok(value)
        }
        Builtin::TimeWallNow => {
            require_native_host_response(builder, module, response)?;
            let NativeType::Object(layout) = result_type else {
                return Err(native_error("wall-clock result has no native list layout"));
            };
            let object = allocate_native_buffer(builder, module, layout, 2, objects)?;
            let (data_offset, _, _, element) = native_buffer_layout(layout, objects)?;
            if element.semantic != ValueSemantic::Integer {
                return Err(native_error("wall-clock result is not a List<Int>"));
            }
            let data = builder.ins().load(
                module.target_config().pointer_type(),
                MemFlagsData::trusted(),
                object,
                data_offset as i32,
            );
            for index in 0..2 {
                let value = native_host_integer(builder, module, response, index)?;
                store_physical_value(
                    builder,
                    data,
                    u32::try_from(index).unwrap_or(0) * element.size,
                    value,
                );
            }
            release_native_host_response(builder, module, response)?;
            Ok(object)
        }
        _ => {
            let NativeType::Object(layout) = result_type else {
                return Err(native_error(format!(
                    "host intrinsic `{builtin:?}` has a non-object Result ABI"
                )));
            };
            lower_native_host_result(builder, module, builtin, response, layout, objects)
        }
    }
}

fn call_native_host(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    builtin: crate::intrinsics::Builtin,
    arguments: NativeHostArguments<'_>,
    function: &ir::Function,
    objects: ObjectRuntime<'_>,
) -> Result<ClifValue, FosterError> {
    use crate::intrinsics::Builtin;

    let operation = builder
        .ins()
        .iconst(types::I64, i64::from(builtin.descriptor().bytecode_tag));
    let call = |builder: &mut FunctionBuilder<'_>,
                module: &mut ObjectModule,
                helper,
                parameters: Vec<NativeType>,
                values: &[ClifValue]| {
        runtime_call(
            builder,
            module,
            helper,
            &ir::Signature {
                parameters,
                result: NativeType::Opaque,
            },
            values,
        )
    };
    match builtin {
        Builtin::IoCurrentDirectory | Builtin::TimeWallNow | Builtin::TimeMonotonicNow => call(
            builder,
            module,
            abi::HOST_CALL_NULLARY,
            vec![NativeType::Int],
            &[operation],
        ),
        Builtin::IoReadText
        | Builtin::IoReadBytes
        | Builtin::IoListDirectory
        | Builtin::IoExists
        | Builtin::IoIsFile
        | Builtin::IoIsDirectory
        | Builtin::IoCreateDirectory
        | Builtin::IoCreateDirectoryAll
        | Builtin::IoRemoveFile
        | Builtin::IoRemoveDirectory
        | Builtin::IoParent
        | Builtin::IoFileName
        | Builtin::IoExtension
        | Builtin::IoCanonicalize
        | Builtin::IoFileLength => call(
            builder,
            module,
            abi::HOST_CALL_STRING,
            vec![NativeType::Int, NativeType::String],
            &[operation, arguments.lowered[0]],
        ),
        Builtin::IoWriteText | Builtin::IoRename | Builtin::IoCopyFile | Builtin::IoJoin => call(
            builder,
            module,
            abi::HOST_CALL_STRINGS,
            vec![NativeType::Int, NativeType::String, NativeType::String],
            &[operation, arguments.lowered[0], arguments.lowered[1]],
        ),
        Builtin::IoReadRange | Builtin::TcpListen | Builtin::TcpConnect => {
            let second = arguments
                .lowered
                .get(2)
                .copied()
                .unwrap_or_else(|| builder.ins().iconst(types::I64, 0));
            call(
                builder,
                module,
                abi::HOST_CALL_STRING_INTS,
                vec![
                    NativeType::Int,
                    NativeType::String,
                    NativeType::Int,
                    NativeType::Int,
                ],
                &[
                    operation,
                    arguments.lowered[0],
                    arguments.lowered[1],
                    second,
                ],
            )
        }
        Builtin::RandomBytes
        | Builtin::TcpAccept
        | Builtin::TcpCloseListener
        | Builtin::TcpCloseConnection => call(
            builder,
            module,
            abi::HOST_CALL_INT,
            vec![NativeType::Int, NativeType::Int],
            &[operation, arguments.lowered[0]],
        ),
        Builtin::TcpRead | Builtin::TcpReadBytes | Builtin::TcpSetTimeout => call(
            builder,
            module,
            abi::HOST_CALL_INTS,
            vec![NativeType::Int, NativeType::Int, NativeType::Int],
            &[operation, arguments.lowered[0], arguments.lowered[1]],
        ),
        Builtin::IoWriteBytes | Builtin::IoAppendBytes => {
            let (data, length) = native_host_bytes_argument(
                builder,
                module,
                arguments.values[1],
                arguments.lowered[1],
                function,
                objects,
            )?;
            call(
                builder,
                module,
                abi::HOST_CALL_STRING_BYTES,
                vec![
                    NativeType::Int,
                    NativeType::String,
                    NativeType::Opaque,
                    NativeType::Int,
                ],
                &[operation, arguments.lowered[0], data, length],
            )
        }
        Builtin::TcpWriteBytes => {
            let (data, length) = native_host_bytes_argument(
                builder,
                module,
                arguments.values[1],
                arguments.lowered[1],
                function,
                objects,
            )?;
            call(
                builder,
                module,
                abi::HOST_CALL_INT_BYTES,
                vec![
                    NativeType::Int,
                    NativeType::Int,
                    NativeType::Opaque,
                    NativeType::Int,
                ],
                &[operation, arguments.lowered[0], data, length],
            )
        }
        Builtin::TcpWrite => call(
            builder,
            module,
            abi::HOST_CALL_INT_STRING,
            vec![NativeType::Int, NativeType::Int, NativeType::String],
            &[operation, arguments.lowered[0], arguments.lowered[1]],
        ),
        unsupported => Err(native_error(format!(
            "host intrinsic `{unsupported:?}` has no platform call shape"
        ))),
    }
}

fn native_host_bytes_argument(
    builder: &mut FunctionBuilder<'_>,
    module: &ObjectModule,
    argument: ir::Value,
    object: ClifValue,
    function: &ir::Function,
    objects: ObjectRuntime<'_>,
) -> Result<(ClifValue, ClifValue), FosterError> {
    let NativeType::Object(layout) = function.value_type(argument) else {
        return Err(native_error(
            "host byte argument has no native Bytes layout",
        ));
    };
    let (data_offset, length_offset) = native_bytes_layout(layout, objects)?;
    let word = module.target_config().pointer_type();
    Ok((
        builder
            .ins()
            .load(word, MemFlagsData::trusted(), object, data_offset as i32),
        builder
            .ins()
            .load(word, MemFlagsData::trusted(), object, length_offset as i32),
    ))
}

fn lower_native_host_result(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    builtin: crate::intrinsics::Builtin,
    response: ClifValue,
    layout: LayoutId,
    objects: ObjectRuntime<'_>,
) -> Result<ClifValue, FosterError> {
    let PhysicalKind::Variant {
        tag_offset,
        alternatives,
        ..
    } = &objects.layouts.physical.get(layout).kind
    else {
        return Err(native_error(
            "native host result does not use a variant layout",
        ));
    };
    let tag_offset = *tag_offset;
    let success = alternatives
        .iter()
        .find(|alternative| alternative.name == "Ok")
        .cloned()
        .ok_or_else(|| native_error("native host Result is missing its Ok alternative"))?;
    let failure = alternatives
        .iter()
        .find(|alternative| alternative.name == "Error")
        .cloned()
        .ok_or_else(|| native_error("native host Result is missing its Error alternative"))?;
    if success.fields.len() != 1 || failure.fields.len() != 1 {
        return Err(native_error(
            "native host Result alternatives must have one payload",
        ));
    }

    let success_block = builder.create_block();
    let failure_block = builder.create_block();
    let finish = builder.create_block();
    builder.append_block_param(finish, module.target_config().pointer_type());
    let ok = runtime_call(
        builder,
        module,
        abi::HOST_OK,
        &ir::Signature {
            parameters: vec![NativeType::Opaque],
            result: NativeType::Bool,
        },
        &[response],
    )?;
    builder
        .ins()
        .brif(ok, success_block, &[], failure_block, &[]);

    builder.switch_to_block(success_block);
    let object = objects.allocate(builder, module, layout)?;
    let tag = builder.ins().iconst(types::I32, i64::from(success.tag));
    builder
        .ins()
        .store(MemFlagsData::trusted(), tag, object, tag_offset as i32);
    let value = lower_native_host_success(
        builder,
        module,
        builtin,
        response,
        success.fields[0].value,
        objects,
    )?;
    store_physical_value(builder, object, success.fields[0].offset, value);
    release_native_host_response(builder, module, response)?;
    builder.ins().jump(finish, &[object.into()]);

    builder.switch_to_block(failure_block);
    let object = objects.allocate(builder, module, layout)?;
    let tag = builder.ins().iconst(types::I32, i64::from(failure.tag));
    builder
        .ins()
        .store(MemFlagsData::trusted(), tag, object, tag_offset as i32);
    let error_layout = failure.fields[0]
        .value
        .pointee
        .ok_or_else(|| native_error("native host Result error has no record layout"))?;
    let error = lower_native_host_error(builder, module, response, error_layout, objects)?;
    store_physical_value(builder, object, failure.fields[0].offset, error);
    release_native_host_response(builder, module, response)?;
    builder.ins().jump(finish, &[object.into()]);

    builder.switch_to_block(finish);
    Ok(builder.block_params(finish)[0])
}

fn lower_native_host_success(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    builtin: crate::intrinsics::Builtin,
    response: ClifValue,
    value: ValueLayout,
    objects: ObjectRuntime<'_>,
) -> Result<ClifValue, FosterError> {
    use crate::intrinsics::Builtin;

    match builtin {
        Builtin::IoWriteText
        | Builtin::IoWriteBytes
        | Builtin::IoCreateDirectory
        | Builtin::IoCreateDirectoryAll
        | Builtin::IoRemoveFile
        | Builtin::IoRemoveDirectory
        | Builtin::IoRename
        | Builtin::TcpWrite
        | Builtin::TcpWriteBytes
        | Builtin::TcpSetTimeout
        | Builtin::TcpCloseListener
        | Builtin::TcpCloseConnection => Ok(builder.ins().iconst(types::I8, 0)),
        Builtin::IoAppendBytes
        | Builtin::IoFileLength
        | Builtin::IoCopyFile
        | Builtin::TcpListen
        | Builtin::TcpConnect
        | Builtin::TcpAccept => native_host_integer(builder, module, response, 0),
        Builtin::IoReadText
        | Builtin::IoCanonicalize
        | Builtin::IoCurrentDirectory
        | Builtin::TcpRead => {
            native_host_string(builder, module, response, abi::host_string::VALUE, 0)
        }
        Builtin::IoReadBytes
        | Builtin::IoReadRange
        | Builtin::TcpReadBytes
        | Builtin::RandomBytes => {
            let layout = value
                .pointee
                .ok_or_else(|| native_error("native host byte result has no Bytes layout"))?;
            lower_native_host_bytes(builder, module, response, layout, objects)
        }
        Builtin::IoListDirectory => {
            let layout = value
                .pointee
                .ok_or_else(|| native_error("native directory result has no list layout"))?;
            lower_native_host_string_list(builder, module, response, layout, objects)
        }
        unsupported => Err(native_error(format!(
            "host intrinsic `{unsupported:?}` has no success-value lowering"
        ))),
    }
}

fn lower_native_host_error(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    response: ClifValue,
    layout: LayoutId,
    objects: ObjectRuntime<'_>,
) -> Result<ClifValue, FosterError> {
    let PhysicalKind::Record { fields, .. } = &objects.layouts.physical.get(layout).kind else {
        return Err(native_error("native host error has a non-record layout"));
    };
    let fields = fields.clone();
    let error = objects.allocate(builder, module, layout)?;
    for field in fields {
        let value = match field.name.as_str() {
            "operation" => native_host_string(
                builder,
                module,
                response,
                abi::host_string::ERROR_OPERATION,
                0,
            )?,
            "path" => {
                native_host_string(builder, module, response, abi::host_string::ERROR_PATH, 0)?
            }
            "message" => native_host_string(
                builder,
                module,
                response,
                abi::host_string::ERROR_MESSAGE,
                0,
            )?,
            "value" => runtime_call(
                builder,
                module,
                abi::HOST_ERROR_VALUE,
                &ir::Signature {
                    parameters: vec![NativeType::Opaque],
                    result: NativeType::Int,
                },
                &[response],
            )?,
            name => {
                return Err(native_error(format!(
                    "native host error has unsupported field `{name}`"
                )));
            }
        };
        store_physical_value(builder, error, field.offset, value);
    }
    Ok(error)
}

fn lower_native_host_bytes(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    response: ClifValue,
    layout: LayoutId,
    objects: ObjectRuntime<'_>,
) -> Result<ClifValue, FosterError> {
    let length = runtime_call(
        builder,
        module,
        abi::HOST_BYTES_LENGTH,
        &ir::Signature {
            parameters: vec![NativeType::Opaque],
            result: NativeType::Int,
        },
        &[response],
    )?;
    let native_length = native_int_to_word(builder, module, length);
    let (object, data) = allocate_native_bytes(builder, module, objects, layout, native_length)?;
    runtime_call(
        builder,
        module,
        abi::HOST_COPY_BYTES,
        &ir::Signature {
            parameters: vec![NativeType::Opaque, NativeType::Opaque],
            result: NativeType::Unit,
        },
        &[response, data],
    )?;
    Ok(object)
}

fn lower_native_host_string_list(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    response: ClifValue,
    layout: LayoutId,
    objects: ObjectRuntime<'_>,
) -> Result<ClifValue, FosterError> {
    let length = runtime_call(
        builder,
        module,
        abi::HOST_STRINGS_LENGTH,
        &ir::Signature {
            parameters: vec![NativeType::Opaque],
            result: NativeType::Int,
        },
        &[response],
    )?;
    let length = native_int_to_word(builder, module, length);
    let (data_offset, _, _, element) = native_buffer_layout(layout, objects)?;
    if element.semantic != ValueSemantic::String {
        return Err(native_error(
            "native directory result is not a List<String>",
        ));
    }
    let object = allocate_native_buffer_dynamic(builder, module, layout, length, objects)?;
    let word = module.target_config().pointer_type();
    let data = builder
        .ins()
        .load(word, MemFlagsData::trusted(), object, data_offset as i32);
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
    let runtime_index = native_word_to_int(builder, module, index);
    let value = native_host_string(
        builder,
        module,
        response,
        abi::host_string::LIST_VALUE,
        runtime_index,
    )?;
    let offset = builder.ins().imul_imm_u(index, i64::from(element.size));
    let address = builder.ins().iadd(data, offset);
    store_physical_value(builder, address, 0, value);
    let next = builder.ins().iadd_imm_s(index, 1);
    builder.ins().jump(loop_block, &[next.into()]);
    builder.switch_to_block(finish);
    Ok(object)
}

fn native_int_to_word(
    builder: &mut FunctionBuilder<'_>,
    module: &ObjectModule,
    value: ClifValue,
) -> ClifValue {
    let word = module.target_config().pointer_type();
    if word == types::I64 {
        value
    } else {
        builder.ins().ireduce(word, value)
    }
}

fn native_word_to_int(
    builder: &mut FunctionBuilder<'_>,
    module: &ObjectModule,
    value: ClifValue,
) -> ClifValue {
    if module.target_config().pointer_type() == types::I64 {
        value
    } else {
        builder.ins().uextend(types::I64, value)
    }
}

fn native_host_integer(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    response: ClifValue,
    index: i64,
) -> Result<ClifValue, FosterError> {
    let index = builder.ins().iconst(types::I64, index);
    runtime_call(
        builder,
        module,
        abi::HOST_INTEGER,
        &ir::Signature {
            parameters: vec![NativeType::Opaque, NativeType::Int],
            result: NativeType::Int,
        },
        &[response, index],
    )
}

fn native_host_string(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    response: ClifValue,
    field: i64,
    index: impl IntoNativeHostIndex,
) -> Result<ClifValue, FosterError> {
    let field = builder.ins().iconst(types::I64, field);
    let index = index.into_native_host_index(builder);
    runtime_call(
        builder,
        module,
        abi::HOST_STRING,
        &ir::Signature {
            parameters: vec![NativeType::Opaque, NativeType::Int, NativeType::Int],
            result: NativeType::String,
        },
        &[response, field, index],
    )
}

trait IntoNativeHostIndex {
    fn into_native_host_index(self, builder: &mut FunctionBuilder<'_>) -> ClifValue;
}

impl IntoNativeHostIndex for i64 {
    fn into_native_host_index(self, builder: &mut FunctionBuilder<'_>) -> ClifValue {
        builder.ins().iconst(types::I64, self)
    }
}

impl IntoNativeHostIndex for ClifValue {
    fn into_native_host_index(self, _builder: &mut FunctionBuilder<'_>) -> ClifValue {
        self
    }
}

fn require_native_host_response(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    response: ClifValue,
) -> Result<(), FosterError> {
    runtime_call(
        builder,
        module,
        abi::HOST_REQUIRE_OK,
        &ir::Signature {
            parameters: vec![NativeType::Opaque],
            result: NativeType::Unit,
        },
        &[response],
    )?;
    // This opaque response is owned inside the current IR instruction, so it
    // needs a failure-only release in addition to that instruction's live values.
    let release_signature = signature(
        module,
        &ir::Signature {
            parameters: vec![NativeType::Opaque],
            result: NativeType::Unit,
        },
    );
    let release = module
        .declare_function(abi::HOST_RELEASE, Linkage::Import, &release_signature)
        .map_err(|error| native_error(format!("cannot declare host response cleanup: {error}")))?;
    builder.cleanup.insert(0, (response, release));
    propagate_native_failure(builder, module)?;
    builder.cleanup.remove(0);
    Ok(())
}

fn release_native_host_response(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    response: ClifValue,
) -> Result<(), FosterError> {
    runtime_call(
        builder,
        module,
        abi::HOST_RELEASE,
        &ir::Signature {
            parameters: vec![NativeType::Opaque],
            result: NativeType::Unit,
        },
        &[response],
    )?;
    Ok(())
}
