//! Control flow, arithmetic, runtime calls, and failure propagation.
use super::{
    AbiParam, BinaryOp, ClifBlock, ClifValue, FloatCC, FosterError, FunctionBuilder, HashMap,
    InstBuilder, IntCC, LayoutKind, LayoutRegistry, Linkage, Module, NativeType, ObjectModule, abi,
    cranelift_representation, ir, native_error, types,
};

pub(super) fn lower_native_terminator(
    builder: &mut FunctionBuilder<'_>,
    terminator: &ir::Terminator,
    blocks: &[ClifBlock],
    values: &HashMap<ir::Value, ClifValue>,
) {
    let arguments = |items: &[ir::Value]| {
        items
            .iter()
            .map(|value| values[value].into())
            .collect::<Vec<_>>()
    };
    match terminator {
        ir::Terminator::Jump {
            target,
            arguments: args,
        } => {
            builder
                .ins()
                .jump(blocks[target.0 as usize], &arguments(args));
        }
        ir::Terminator::Branch {
            condition,
            then_target,
            then_arguments,
            else_target,
            else_arguments,
        } => {
            let condition = builder
                .ins()
                .icmp_imm_s(IntCC::NotEqual, values[condition], 0);
            builder.ins().brif(
                condition,
                blocks[then_target.0 as usize],
                &arguments(then_arguments),
                blocks[else_target.0 as usize],
                &arguments(else_arguments),
            );
        }
        ir::Terminator::Return(value) => {
            builder.ins().return_(&[values[value]]);
        }
    }
}

pub(super) fn lower_binary(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    operator: BinaryOp,
    operand_type: NativeType,
    left: ClifValue,
    right: ClifValue,
    layouts: &LayoutRegistry,
) -> Result<ClifValue, FosterError> {
    if let NativeType::Object(layout) = operand_type
        && !matches!(layouts.get(layout).kind, LayoutKind::Pointer { .. })
        && matches!(operator, BinaryOp::Equal | BinaryOp::NotEqual)
    {
        let equal = runtime_call(
            builder,
            module,
            abi::OBJECT_EQUAL,
            &ir::Signature {
                parameters: vec![operand_type, operand_type],
                result: NativeType::Bool,
            },
            &[left, right],
        )?;
        return Ok(if operator == BinaryOp::NotEqual {
            builder.ins().bxor_imm_u(equal, 1)
        } else {
            equal
        });
    }
    if operand_type == NativeType::String {
        if operator == BinaryOp::Add {
            return runtime_call(
                builder,
                module,
                abi::STRING_CONCAT,
                &ir::Signature {
                    parameters: vec![NativeType::String, NativeType::String],
                    result: NativeType::String,
                },
                &[left, right],
            );
        }
        let equal = runtime_call(
            builder,
            module,
            abi::STRING_EQUAL,
            &ir::Signature {
                parameters: vec![NativeType::String, NativeType::String],
                result: NativeType::Bool,
            },
            &[left, right],
        )?;
        return match operator {
            BinaryOp::Equal => Ok(equal),
            BinaryOp::NotEqual => {
                let one = builder.ins().iconst(types::I8, 1);
                Ok(builder.ins().bxor(equal, one))
            }
            _ => Err(native_error(format!(
                "native String values do not support operator `{operator:?}`"
            ))),
        };
    }
    if operand_type == NativeType::Float {
        let result = match operator {
            BinaryOp::Add => builder.ins().fadd(left, right),
            BinaryOp::Subtract => builder.ins().fsub(left, right),
            BinaryOp::Multiply => builder.ins().fmul(left, right),
            BinaryOp::Divide => builder.ins().fdiv(left, right),
            BinaryOp::Equal => return Ok(float_comparison(builder, FloatCC::Equal, left, right)),
            BinaryOp::NotEqual => {
                return Ok(float_comparison(builder, FloatCC::NotEqual, left, right));
            }
            BinaryOp::Less => return Ok(float_comparison(builder, FloatCC::LessThan, left, right)),
            BinaryOp::LessEqual => {
                return Ok(float_comparison(
                    builder,
                    FloatCC::LessThanOrEqual,
                    left,
                    right,
                ));
            }
            BinaryOp::Greater => {
                return Ok(float_comparison(builder, FloatCC::GreaterThan, left, right));
            }
            BinaryOp::GreaterEqual => {
                return Ok(float_comparison(
                    builder,
                    FloatCC::GreaterThanOrEqual,
                    left,
                    right,
                ));
            }
            _ => {
                return Err(native_error(format!(
                    "operator `{operator:?}` is invalid for Float"
                )));
            }
        };
        return Ok(result);
    }
    let result = match operator {
        BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply => {
            let pair = match operator {
                BinaryOp::Add => builder.ins().sadd_overflow(left, right),
                BinaryOp::Subtract => builder.ins().ssub_overflow(left, right),
                BinaryOp::Multiply => builder.ins().smul_overflow(left, right),
                _ => unreachable!(),
            };
            let detail = zero_i64(builder);
            let limit = zero_i64(builder);
            fail_if(
                builder,
                module,
                pair.1,
                abi::failure::INTEGER_OVERFLOW,
                detail,
                limit,
            )?;
            pair.0
        }
        BinaryOp::Divide => {
            let zero_divisor = builder.ins().icmp_imm_s(IntCC::Equal, right, 0);
            let minimum = builder.ins().icmp_imm_s(IntCC::Equal, left, i64::MIN);
            let negative_one = builder.ins().icmp_imm_s(IntCC::Equal, right, -1);
            let overflow = builder.ins().band(minimum, negative_one);
            let invalid = builder.ins().bor(zero_divisor, overflow);
            let limit = zero_i64(builder);
            fail_if(
                builder,
                module,
                invalid,
                abi::failure::DIVISION,
                right,
                limit,
            )?;
            builder.ins().sdiv(left, right)
        }
        BinaryOp::BitAnd => builder.ins().band(left, right),
        BinaryOp::BitOr => builder.ins().bor(left, right),
        BinaryOp::BitXor => builder.ins().bxor(left, right),
        BinaryOp::ShiftLeft | BinaryOp::ShiftRight => {
            let invalid = builder
                .ins()
                .icmp_imm_u(IntCC::UnsignedGreaterThan, right, 7);
            let detail = right;
            let limit = builder.ins().iconst(types::I64, 7);
            fail_if(
                builder,
                module,
                invalid,
                abi::failure::INVALID_SHIFT,
                detail,
                limit,
            )?;
            if operator == BinaryOp::ShiftLeft {
                builder.ins().ishl(left, right)
            } else {
                builder.ins().ushr(left, right)
            }
        }
        BinaryOp::Equal => integer_comparison(builder, IntCC::Equal, left, right),
        BinaryOp::NotEqual => integer_comparison(builder, IntCC::NotEqual, left, right),
        BinaryOp::Less => integer_comparison(builder, IntCC::SignedLessThan, left, right),
        BinaryOp::LessEqual => {
            integer_comparison(builder, IntCC::SignedLessThanOrEqual, left, right)
        }
        BinaryOp::Greater => integer_comparison(builder, IntCC::SignedGreaterThan, left, right),
        BinaryOp::GreaterEqual => {
            integer_comparison(builder, IntCC::SignedGreaterThanOrEqual, left, right)
        }
    };
    Ok(result)
}

pub(super) fn runtime_call(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    name: &str,
    source_signature: &ir::Signature,
    arguments: &[ClifValue],
) -> Result<ClifValue, FosterError> {
    let contract = abi::verify_call(name, source_signature).map_err(native_error)?;
    if arguments.len() != source_signature.parameters.len() {
        return Err(native_error(format!(
            "runtime helper `{name}` has inconsistent argument count"
        )));
    }
    let pointer_type = module.target_config().pointer_type();
    let mut signature = module.make_signature();
    signature
        .params
        .extend(contract.parameters.iter().map(|(wire, _)| {
            AbiParam::new(cranelift_representation(
                wire.representation(),
                pointer_type,
            ))
        }));
    signature
        .returns
        .push(AbiParam::new(cranelift_representation(
            contract.result.representation(),
            pointer_type,
        )));
    let function = module
        .declare_function(name, Linkage::Import, &signature)
        .map_err(|error| {
            native_error(format!("cannot declare native runtime `{name}`: {error}"))
        })?;
    let reference = module.declare_func_in_func(function, builder.func);
    let call = builder.ins().call(reference, arguments);
    let result = builder.inst_results(call)[0];
    if matches!(
        name,
        abi::CANCELLATION_POINT
            | abi::ASSERT
            | abi::FAIL
            | abi::STRING_HEAD
            | abi::STRING_GET
            | abi::PARSE_FLOAT
    ) {
        propagate_native_failure(builder, module)?;
    }
    Ok(result)
}

/// Generated frames return normally on failure: no unwinding through Cranelift frames.
/// The thread retains the error; callers check it before treating a return as a result.
pub(super) fn propagate_native_failure(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
) -> Result<(), FosterError> {
    let pending = runtime_call(
        builder,
        module,
        abi::FAILURE_PENDING,
        &ir::Signature {
            parameters: vec![],
            result: NativeType::Bool,
        },
        &[],
    )?;
    let failed = builder.create_block();
    let continuation = builder.create_block();
    builder.ins().brif(pending, failed, &[], continuation, &[]);
    builder.switch_to_block(failed);
    for (value, release) in builder.cleanup.clone() {
        let value = if let Some(home) = builder.cleanup_homes.get(&value).copied() {
            let pointer_type = module.target_config().pointer_type();
            builder
                .ins()
                .stack_load(pointer_type, pointer_type, home, 0)
        } else {
            value
        };
        let release = module.declare_func_in_func(release, builder.func);
        builder.ins().call(release, &[value]);
    }
    let ty = builder.func.signature.returns[0].value_type;
    let placeholder = if ty == types::F64 {
        builder.ins().f64const(0.0)
    } else {
        builder.ins().iconst(ty, 0)
    };
    builder.ins().return_(&[placeholder]);
    builder.switch_to_block(continuation);
    Ok(())
}

pub(super) fn fail_if(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    condition: ClifValue,
    kind: i64,
    detail: ClifValue,
    limit: ClifValue,
) -> Result<(), FosterError> {
    let failed = builder.create_block();
    let continuation = builder.create_block();
    builder
        .ins()
        .brif(condition, failed, &[], continuation, &[]);
    builder.switch_to_block(failed);
    let kind = builder.ins().iconst(types::I64, kind);
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
    builder.ins().jump(continuation, &[]);
    builder.switch_to_block(continuation);
    Ok(())
}

pub(super) fn zero_i64(builder: &mut FunctionBuilder<'_>) -> ClifValue {
    builder.ins().iconst(types::I64, 0)
}

pub(super) fn write_native_value(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    value: ClifValue,
    ty: NativeType,
) -> Result<(), FosterError> {
    let (helper, parameters) = match ty {
        NativeType::Unit => (abi::WRITE_UNIT, Vec::new()),
        NativeType::Bool => (abi::WRITE_BOOL, vec![NativeType::Bool]),
        NativeType::Int => (abi::WRITE_INT, vec![NativeType::Int]),
        NativeType::Float => (abi::WRITE_FLOAT, vec![NativeType::Float]),
        NativeType::CodePoint => (abi::WRITE_CODE_POINT, vec![NativeType::CodePoint]),
        NativeType::Byte => (abi::WRITE_BYTE, vec![NativeType::Byte]),
        NativeType::String => (abi::WRITE_STRING, vec![NativeType::String]),
        NativeType::Object(_) | NativeType::Opaque => (abi::WRITE_OBJECT, vec![NativeType::Opaque]),
    };
    let arguments = if parameters.is_empty() {
        Vec::new()
    } else {
        vec![value]
    };
    runtime_call(
        builder,
        module,
        helper,
        &ir::Signature {
            parameters,
            result: NativeType::Unit,
        },
        &arguments,
    )?;
    Ok(())
}

pub(super) fn write_native_separator(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
) -> Result<(), FosterError> {
    runtime_call(
        builder,
        module,
        abi::WRITE_SEPARATOR,
        &ir::Signature {
            parameters: Vec::new(),
            result: NativeType::Unit,
        },
        &[],
    )?;
    Ok(())
}

pub(super) fn write_native_newline(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
) -> Result<(), FosterError> {
    runtime_call(
        builder,
        module,
        abi::WRITE_NEWLINE,
        &ir::Signature {
            parameters: Vec::new(),
            result: NativeType::Unit,
        },
        &[],
    )?;
    Ok(())
}

fn integer_comparison(
    builder: &mut FunctionBuilder<'_>,
    condition: IntCC,
    left: ClifValue,
    right: ClifValue,
) -> ClifValue {
    builder.ins().icmp(condition, left, right)
}

fn float_comparison(
    builder: &mut FunctionBuilder<'_>,
    condition: FloatCC,
    left: ClifValue,
    right: ClifValue,
) -> ClifValue {
    builder.ins().fcmp(condition, left, right)
}
