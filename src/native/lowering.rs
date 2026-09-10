//! Lower native IR blocks, patterns, and instructions to Cranelift.
use super::{
    ClifValue, FloatCC, FosterError, FunctionBuilder, HashMap, HashSet, InstBuilder, IntCC,
    LayoutKind, LayoutRegistry, MemFlagsData, Module, NativeBackend, NativeFunction,
    NativeLowering, NativeType, ObjectModule, ObjectRuntime, Pattern, PatternSubject, PhysicalKind,
    ScalarKind, StackSlotData, StackSlotKind, UnaryOp, ValueLayout, ValueSemantic, abi,
    cranelift_type, fail_if, ir, load_physical_value, lower_binary, lower_native_terminator,
    lower_portable_native, lower_result_error_conversion, native_error, propagate_native_failure,
    runtime_call, store_physical_value, types, zero_i64,
};

pub(super) fn lower_native_ir(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    prepared: &NativeFunction,
    backend: &NativeBackend<'_>,
) -> Result<(), FosterError> {
    let function = &prepared.ir;
    // Address-taken locals are memory-backed. A call can replace an aggregate
    // as well as mutate a scalar without producing a new SSA definition.
    let referenced_homes = function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            ir::Instruction::Portable(ir::PortableInstruction::MakeWholeReference {
                object,
                ..
            }) => function.storage_hints[object.0 as usize],
            _ => None,
        })
        .collect::<HashSet<_>>();
    let mutable_parameter_homes = &prepared.mutable_parameter_homes;
    let pointer_type = module.target_config().pointer_type();
    let homes = prepared
        .home_types
        .iter()
        .map(|(home, ty)| {
            let lowered = cranelift_type(*ty, pointer_type);
            let size = lowered.bytes();
            let align_shift = u8::try_from(size.trailing_zeros()).unwrap_or(0);
            let slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                size,
                align_shift,
            ));
            (*home, slot)
        })
        .collect::<HashMap<_, _>>();
    let prologue = builder.create_block();
    builder.append_block_params_for_function_params(prologue);
    let blocks = function
        .blocks
        .iter()
        .map(|block| {
            let lowered = builder.create_block();
            for parameter in &block.parameters {
                builder.append_block_param(
                    lowered,
                    cranelift_type(function.value_type(*parameter), pointer_type),
                );
            }
            lowered
        })
        .collect::<Vec<_>>();

    builder.switch_to_block(prologue);
    let function_parameters = builder.block_params(prologue).to_vec();
    if function.parameters.len() != function_parameters.len() {
        return Err(native_error(format!(
            "native function `{}` has inconsistent parameter lowering",
            function.name
        )));
    }
    let mut values = function
        .parameters
        .iter()
        .copied()
        .zip(function_parameters)
        .collect::<HashMap<_, _>>();
    for seed in &function.entry_seeds {
        let ty = function.value_type(*seed);
        let value = match ty {
            NativeType::Float => builder.ins().f64const(0.0),
            _ => builder.ins().iconst(cranelift_type(ty, pointer_type), 0),
        };
        values.insert(*seed, value);
    }
    for value in function.parameters.iter().chain(&function.entry_seeds) {
        if let Some(home) = function.storage_hints[value.0 as usize] {
            builder
                .ins()
                .stack_store(pointer_type, values[value], homes[&home], 0);
        }
    }
    let entry_arguments = function
        .entry_arguments
        .iter()
        .map(|value| values[value].into())
        .collect::<Vec<_>>();
    builder
        .ins()
        .jump(blocks[function.entry.0 as usize], &entry_arguments);

    for (index, block) in function.blocks.iter().enumerate() {
        let lowered_block = blocks[index];
        builder.switch_to_block(lowered_block);
        for (parameter, lowered) in block
            .parameters
            .iter()
            .zip(builder.block_params(lowered_block).to_vec())
        {
            values.insert(*parameter, lowered);
            if let Some(home) = function.storage_hints[parameter.0 as usize] {
                builder
                    .ins()
                    .stack_store(pointer_type, lowered, homes[&home], 0);
            }
        }
        for (instruction_index, instruction) in block.instructions.iter().enumerate() {
            for operand in instruction.operands() {
                if let Some(home) = function.storage_hints[operand.0 as usize]
                    && referenced_homes.contains(&home)
                {
                    let loaded = builder.ins().stack_load(
                        pointer_type,
                        cranelift_type(function.value_type(operand), pointer_type),
                        homes[&home],
                        0,
                    );
                    values.insert(operand, loaded);
                }
            }
            builder.cleanup_homes = prepared
                .failure_cleanup
                .values
                .get(&(index, instruction_index))
                .into_iter()
                .flatten()
                .filter_map(|value| {
                    let home = function.storage_hints[value.0 as usize]?;
                    referenced_homes
                        .contains(&home)
                        .then(|| (values[value], homes[&home]))
                })
                .collect();
            builder.cleanup = prepared
                .failure_cleanup
                .values
                .get(&(index, instruction_index))
                .into_iter()
                .flatten()
                .filter_map(|value| {
                    backend
                        .objects
                        .layouts
                        .managed_layout(function.value_type(*value))
                        .map(|layout| (values[value], backend.release_thunks[&layout]))
                })
                .collect();
            if let ir::Instruction::Portable(ir::PortableInstruction::MatchPattern {
                destination,
                subject,
                pattern,
                bindings,
            }) = instruction
            {
                let (matched, lowered_bindings) = lower_native_pattern(
                    builder,
                    module,
                    PatternSubject {
                        value: values[subject],
                        ty: function.value_type(*subject),
                    },
                    pattern,
                    backend.objects,
                    backend.ir.runtime_literal_indices,
                )?;
                if lowered_bindings.len() != bindings.len() {
                    return Err(native_error(
                        "native pattern binding arity changed during lowering",
                    ));
                }
                values.insert(*destination, matched);
                for (binding, value) in bindings.iter().zip(lowered_bindings) {
                    if let Some(layout) = backend
                        .objects
                        .layouts
                        .managed_layout(function.value_type(*binding))
                    {
                        let retain = builder.create_block();
                        let finish = builder.create_block();
                        builder.ins().brif(matched, retain, &[], finish, &[]);
                        builder.switch_to_block(retain);
                        backend.objects.retain(builder, value, layout);
                        builder.ins().jump(finish, &[]);
                        builder.switch_to_block(finish);
                    }
                    values.insert(*binding, value);
                }
                continue;
            }
            let result = lower_native_instruction(
                builder,
                module,
                instruction,
                NativeLowering {
                    function,
                    values: &values,
                    homes: &homes,
                    mutable_parameter_homes,
                    backend,
                },
            )?;
            let destinations = instruction.destinations();
            if let Some(destination) = destinations.first() {
                values.insert(
                    *destination,
                    result.expect("value-producing native instruction"),
                );
                if let Some(home) = function.storage_hints[destination.0 as usize] {
                    builder
                        .ins()
                        .stack_store(pointer_type, values[destination], homes[&home], 0);
                }
            }
        }
        let operands = match &block.terminator {
            ir::Terminator::Jump { arguments, .. } => arguments.clone(),
            ir::Terminator::Branch {
                condition,
                then_arguments,
                else_arguments,
                ..
            } => {
                let mut operands = vec![*condition];
                operands.extend(then_arguments);
                operands.extend(else_arguments);
                operands
            }
            ir::Terminator::Return(value) => vec![*value],
        };
        for operand in operands {
            if let Some(home) = function.storage_hints[operand.0 as usize]
                && referenced_homes.contains(&home)
            {
                let loaded = builder.ins().stack_load(
                    pointer_type,
                    cranelift_type(function.value_type(operand), pointer_type),
                    homes[&home],
                    0,
                );
                values.insert(operand, loaded);
            }
        }
        lower_native_terminator(builder, &block.terminator, &blocks, &values);
    }
    builder.seal_all_blocks();
    Ok(())
}

fn lower_native_pattern(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    subject: PatternSubject,
    pattern: &Pattern,
    objects: ObjectRuntime<'_>,
    runtime_literal_indices: &HashMap<String, u64>,
) -> Result<(ClifValue, Vec<ClifValue>), FosterError> {
    let layouts = objects.layouts;
    let true_value = |builder: &mut FunctionBuilder<'_>| builder.ins().iconst(types::I8, 1);
    match pattern.unspanned() {
        Pattern::Wildcard => Ok((true_value(builder), Vec::new())),
        Pattern::Binding(_) => Ok((true_value(builder), vec![subject.value])),
        Pattern::Bool(expected) => Ok((
            builder
                .ins()
                .icmp_imm_s(IntCC::Equal, subject.value, i64::from(*expected)),
            Vec::new(),
        )),
        Pattern::Integer(expected) => Ok((
            builder
                .ins()
                .icmp_imm_s(IntCC::Equal, subject.value, *expected),
            Vec::new(),
        )),
        Pattern::Float(expected) => {
            let expected = builder.ins().f64const(*expected);
            Ok((
                builder.ins().fcmp(FloatCC::Equal, subject.value, expected),
                Vec::new(),
            ))
        }
        Pattern::CodePoint(expected) => {
            let expected = expected
                .chars()
                .next()
                .ok_or_else(|| native_error("native code-point pattern cannot be empty"))?;
            Ok((
                builder.ins().icmp_imm_s(
                    IntCC::Equal,
                    subject.value,
                    i64::from(u32::from(expected)),
                ),
                Vec::new(),
            ))
        }
        Pattern::String(expected) | Pattern::Symbol(expected) => {
            let index = runtime_literal_indices
                .get(expected)
                .ok_or_else(|| native_error("native string pattern has no runtime constant"))?;
            let index = builder.ins().iconst(types::I64, *index as i64);
            let expected = runtime_call(
                builder,
                module,
                abi::STRING_CONSTANT,
                &ir::Signature {
                    parameters: vec![NativeType::Int],
                    result: NativeType::String,
                },
                &[index],
            )?;
            let matched = runtime_call(
                builder,
                module,
                abi::STRING_EQUAL,
                &ir::Signature {
                    parameters: vec![NativeType::String, NativeType::String],
                    result: NativeType::Bool,
                },
                &[subject.value, expected],
            )?;
            objects.release(builder, module, expected, layouts.string_layout())?;
            Ok((matched, Vec::new()))
        }
        Pattern::Variant { variant, fields } => {
            let NativeType::Object(layout) = subject.ty else {
                return Err(native_error("variant pattern requires a native object"));
            };
            let LayoutKind::Variant { alternatives, .. } = &layouts.logical.get(layout).kind else {
                return Err(native_error("variant pattern has a non-variant subject"));
            };
            let alternative = alternatives
                .iter()
                .find(|alternative| alternative.variant == *variant)
                .ok_or_else(|| native_error("variant pattern has no matching layout tag"))?;
            let physical_layout = layouts.physical.get(layout);
            let PhysicalKind::Variant { tag_offset, .. } = &physical_layout.kind else {
                return Err(native_error(
                    "variant pattern has a non-variant physical layout",
                ));
            };
            let physical = layouts
                .physical
                .variant_alternative(layout, alternative.tag)
                .ok_or_else(|| native_error("variant pattern has no physical alternative"))?;
            if fields.len() != physical.fields.len() {
                return Err(native_error(
                    "variant pattern payload arity is inconsistent",
                ));
            }
            let tag = builder.ins().load(
                types::I32,
                MemFlagsData::trusted(),
                subject.value,
                *tag_offset as i32,
            );
            let matched_tag =
                builder
                    .ins()
                    .icmp_imm_s(IntCC::Equal, tag, i64::from(alternative.tag));
            let payload_block = builder.create_block();
            let failed_block = builder.create_block();
            let join_block = builder.create_block();
            builder
                .ins()
                .brif(matched_tag, payload_block, &[], failed_block, &[]);
            builder.switch_to_block(payload_block);
            let mut matched = true_value(builder);
            let mut bindings = Vec::new();
            for (pattern, field) in fields.iter().zip(&physical.fields) {
                let value =
                    load_physical_value(builder, module, subject.value, field.offset, field.value);
                let field_type = native_type_from_value_layout(field.value);
                let (field_matched, mut field_bindings) = lower_native_pattern(
                    builder,
                    module,
                    PatternSubject {
                        value,
                        ty: field_type,
                    },
                    pattern,
                    objects,
                    runtime_literal_indices,
                )?;
                matched = builder.ins().band(matched, field_matched);
                bindings.append(&mut field_bindings);
            }
            builder.append_block_param(join_block, types::I8);
            for binding in &bindings {
                let ty = builder.func.dfg.value_type(*binding);
                builder.append_block_param(join_block, ty);
            }
            let mut success_arguments = vec![matched.into()];
            success_arguments.extend(
                bindings
                    .iter()
                    .copied()
                    .map(cranelift_codegen::ir::BlockArg::Value),
            );
            builder.ins().jump(join_block, &success_arguments);
            builder.switch_to_block(failed_block);
            let false_value = builder.ins().iconst(types::I8, 0);
            let mut failed_arguments = vec![false_value.into()];
            for binding in &bindings {
                let ty = builder.func.dfg.value_type(*binding);
                let zero = if ty == types::F64 {
                    builder.ins().f64const(0.0)
                } else {
                    builder.ins().iconst(ty, 0)
                };
                failed_arguments.push(zero.into());
            }
            builder.ins().jump(join_block, &failed_arguments);
            builder.switch_to_block(join_block);
            let parameters = builder.block_params(join_block);
            Ok((parameters[0], parameters[1..].to_vec()))
        }
        Pattern::Spanned { .. } => unreachable!(),
    }
}

pub(super) fn native_type_from_value_layout(value: ValueLayout) -> NativeType {
    match (value.kind, value.pointee) {
        (ScalarKind::I8, _) => NativeType::Byte,
        (ScalarKind::I32, _) => NativeType::CodePoint,
        (ScalarKind::I64, _) => NativeType::Int,
        (ScalarKind::F64, _) => NativeType::Float,
        (ScalarKind::Pointer, Some(layout)) => NativeType::Object(layout),
        (ScalarKind::Pointer, None) => NativeType::Opaque,
    }
}

pub(super) fn native_type_semantic(ty: NativeType, layouts: &LayoutRegistry) -> ValueSemantic {
    match ty {
        NativeType::Unit => ValueSemantic::Unit,
        NativeType::Bool => ValueSemantic::Bool,
        NativeType::Int => ValueSemantic::Integer,
        NativeType::Float => ValueSemantic::Float,
        NativeType::CodePoint => ValueSemantic::CodePoint,
        NativeType::Byte => ValueSemantic::Byte,
        NativeType::String => ValueSemantic::String,
        NativeType::Object(layout)
            if matches!(layouts.get(layout).kind, LayoutKind::Pointer { .. }) =>
        {
            ValueSemantic::Reference
        }
        NativeType::Object(_) => ValueSemantic::Object,
        NativeType::Opaque => ValueSemantic::Opaque,
    }
}

fn lower_native_instruction(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    instruction: &ir::Instruction,
    context: NativeLowering<'_, '_>,
) -> Result<Option<ClifValue>, FosterError> {
    let function = context.function;
    let values = context.values;
    let backend = context.backend;
    let get = |value: &ir::Value| values[value];
    let result = match instruction {
        ir::Instruction::Constant { value, .. } => match value {
            ir::Constant::Unit => builder.ins().iconst(types::I8, 0),
            ir::Constant::Bool(value) => builder.ins().iconst(types::I8, i64::from(*value)),
            ir::Constant::Integer(value) => builder.ins().iconst(types::I64, *value),
            ir::Constant::Float(value) => builder.ins().f64const(*value),
            ir::Constant::CodePoint(value) => builder
                .ins()
                .iconst(types::I32, i64::from(u32::from(*value))),
            ir::Constant::RuntimeString(index) => {
                let index = builder.ins().iconst(types::I64, *index as i64);
                runtime_call(
                    builder,
                    module,
                    abi::STRING_CONSTANT,
                    &ir::Signature {
                        parameters: vec![NativeType::Int],
                        result: NativeType::String,
                    },
                    &[index],
                )?
            }
        },
        ir::Instruction::Unary {
            operator, operand, ..
        } => {
            let word = get(operand);
            match operator {
                UnaryOp::Negate if function.value_type(*operand) == NativeType::Float => {
                    builder.ins().fneg(word)
                }
                UnaryOp::Negate => {
                    let zero = builder.ins().iconst(
                        cranelift_type(
                            function.value_type(*operand),
                            module.target_config().pointer_type(),
                        ),
                        0,
                    );
                    let result = builder.ins().ssub_overflow(zero, word);
                    let detail = zero_i64(builder);
                    let limit = zero_i64(builder);
                    fail_if(
                        builder,
                        module,
                        result.1,
                        abi::failure::INTEGER_OVERFLOW,
                        detail,
                        limit,
                    )?;
                    result.0
                }
                UnaryOp::Not => builder.ins().icmp_imm_s(IntCC::Equal, word, 0),
                UnaryOp::BitNot => builder.ins().bnot(word),
            }
        }
        ir::Instruction::IntegerExtend { operand, .. } => {
            builder.ins().uextend(types::I64, get(operand))
        }
        ir::Instruction::Binary {
            operator,
            left,
            right,
            ..
        } => lower_binary(
            builder,
            module,
            *operator,
            function.value_type(*left),
            get(left),
            get(right),
            backend.objects.layouts.logical,
        )?,
        ir::Instruction::Call {
            function,
            arguments,
            ..
        } => {
            let reference = module.declare_func_in_func(backend.functions[function], builder.func);
            let arguments = arguments.iter().map(get).collect::<Vec<_>>();
            let call = builder.ins().call(reference, &arguments);
            propagate_native_failure(builder, module)?;
            builder.inst_results(call)[0]
        }
        ir::Instruction::WrapCallable {
            destination,
            source,
        } => {
            let NativeType::Object(callable_layout) = function.value_type(*destination) else {
                return Err(native_error("callable wrapper has a non-object result"));
            };
            let NativeType::Object(environment_layout) = function.value_type(*source) else {
                return Err(native_error(
                    "callable wrapper has a non-object environment",
                ));
            };
            if !matches!(
                backend.objects.layouts.logical.get(environment_layout).kind,
                LayoutKind::Closure { .. }
            ) {
                return Err(native_error(
                    "callable environment is not a concrete closure",
                ));
            }
            let PhysicalKind::Callable {
                code_offset,
                environment_offset,
                release_offset,
            } = backend.objects.layouts.physical.get(callable_layout).kind
            else {
                return Err(native_error("callable wrapper result has the wrong layout"));
            };
            let object = backend.objects.allocate(builder, module, callable_layout)?;
            let code = module
                .declare_func_in_func(backend.callable_thunks[&environment_layout], builder.func);
            let code = builder
                .ins()
                .func_addr(module.target_config().pointer_type(), code);
            let release = module
                .declare_func_in_func(backend.release_thunks[&environment_layout], builder.func);
            let release = builder
                .ins()
                .func_addr(module.target_config().pointer_type(), release);
            let environment = get(source);
            backend
                .objects
                .retain(builder, environment, environment_layout);
            store_physical_value(builder, object, code_offset, code);
            store_physical_value(builder, object, environment_offset, environment);
            store_physical_value(builder, object, release_offset, release);
            object
        }
        ir::Instruction::StringToBytes {
            destination,
            source,
        } => {
            let NativeType::Object(layout) = function.value_type(*destination) else {
                return Err(native_error("String bytes require an object layout"));
            };
            let string_layout = backend.objects.layouts.string_layout();
            let field = backend
                .objects
                .layouts
                .physical
                .record_field(string_layout, 0)
                .ok_or_else(|| native_error("String requires byte storage"))?;
            let object = builder.ins().load(
                module.target_config().pointer_type(),
                MemFlagsData::trusted(),
                get(source),
                field.offset as i32,
            );
            backend.objects.retain(builder, object, layout);
            object
        }
        ir::Instruction::BoxValue {
            destination,
            source,
        } => {
            let NativeType::Object(layout) = function.value_type(*destination) else {
                return Err(native_error("erased box has a non-object layout"));
            };
            let PhysicalKind::Opaque {
                value_offset,
                release_offset,
                semantic_offset,
                ..
            } = backend.objects.layouts.physical.get(layout).kind
            else {
                return Err(native_error("erased value has the wrong physical layout"));
            };
            let object = backend.objects.allocate(builder, module, layout)?;
            let value = get(source);
            store_physical_value(builder, object, value_offset, value);
            let release = if let Some(source_layout) = backend
                .objects
                .layouts
                .managed_layout(function.value_type(*source))
            {
                backend.objects.retain(builder, value, source_layout);
                let release = module
                    .declare_func_in_func(backend.release_thunks[&source_layout], builder.func);
                builder
                    .ins()
                    .func_addr(module.target_config().pointer_type(), release)
            } else {
                builder
                    .ins()
                    .iconst(module.target_config().pointer_type(), 0)
            };
            store_physical_value(builder, object, release_offset, release);
            let semantic = builder.ins().iconst(
                types::I8,
                i64::from(
                    native_type_semantic(function.value_type(*source), backend.ir.layouts) as u8,
                ),
            );
            store_physical_value(builder, object, semantic_offset, semantic);
            object
        }
        ir::Instruction::UnboxValue {
            destination,
            source,
        } => {
            let NativeType::Object(layout) = function.value_type(*source) else {
                return Err(native_error("erased box source has a non-object layout"));
            };
            let PhysicalKind::Opaque { value_offset, .. } =
                backend.objects.layouts.physical.get(layout).kind
            else {
                return Err(native_error("unboxed value has the wrong physical layout"));
            };
            let ty = function.value_type(*destination);
            let value = builder.ins().load(
                cranelift_type(ty, module.target_config().pointer_type()),
                MemFlagsData::trusted(),
                get(source),
                value_offset as i32,
            );
            if let Some(value_layout) = backend.objects.layouts.managed_layout(ty) {
                backend.objects.retain(builder, value, value_layout);
            }
            value
        }
        ir::Instruction::ConvertResultError {
            destination,
            source,
        } => lower_result_error_conversion(
            builder,
            module,
            get(source),
            function.value_type(*source),
            function.value_type(*destination),
            backend.objects,
        )?,
        ir::Instruction::RuntimeCall {
            helper,
            signature,
            arguments,
            ..
        } => {
            let arguments = arguments.iter().map(get).collect::<Vec<_>>();
            runtime_call(builder, module, helper, signature, &arguments)?
        }
        ir::Instruction::Assert { condition, message } => {
            let condition = get(condition);
            let message = message.as_ref().map(get).unwrap_or_else(|| {
                builder
                    .ins()
                    .iconst(module.target_config().pointer_type(), 0)
            });
            runtime_call(
                builder,
                module,
                abi::ASSERT,
                &ir::Signature {
                    parameters: vec![NativeType::Bool, NativeType::String],
                    result: NativeType::Unit,
                },
                &[condition, message],
            )?;
            return Ok(None);
        }
        ir::Instruction::Portable(instruction) => {
            return lower_portable_native(builder, module, instruction, context);
        }
    };
    Ok(Some(result))
}
