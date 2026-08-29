//! Ahead-of-time native compilation through Cranelift.
//!
//! The native backend deliberately accepts a smaller language surface than the VM. Unsupported
//! operations are diagnosed before an object is emitted, which keeps the portable bytecode VM as
//! the reference implementation while native support grows.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use cranelift_codegen::ir::{
    AbiParam, InstBuilder, MemFlagsData, Signature, StackSlotData, StackSlotKind, TrapCode, types,
};
use cranelift_codegen::ir::{condcodes::FloatCC, condcodes::IntCC};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{FuncId, Linkage, Module, default_libcall_names};
use cranelift_object::{ObjectBuilder, ObjectModule};

use crate::ast::{BinaryOp, UnaryOp};
use crate::error::FosterError;
use crate::hir::{Compilation, FunctionId};
use crate::types::{Type, TypeId};
use crate::vm::{self, BytecodeFunction, Constant, Instruction, Program, Register};

/// Primitive Foster values supported by the native ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeType {
    Unit,
    Bool,
    Int,
    Float,
    CodePoint,
    Byte,
    String,
    Arguments,
    StringList,
}

/// Controls machine-code optimization performed by Cranelift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompileOptions {
    pub optimize: bool,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self { optimize: true }
    }
}

/// A linkable native object and the type returned by its exported entry point.
#[derive(Debug)]
pub struct ObjectArtifact {
    pub bytes: Vec<u8>,
    pub result: NativeType,
    pub accepts_arguments: bool,
    runtime_strings: Vec<String>,
}

/// Compile the reachable portion of `main` to a host-native object file.
pub fn compile_object(
    compilation: &Compilation,
    options: CompileOptions,
) -> Result<ObjectArtifact, FosterError> {
    // The non-optimized register program preserves one stable static type per register. Cranelift
    // still performs the requested machine-level optimization below.
    let program = vm::compile_with_options(compilation, vm::CompileOptions { optimize: false })?;
    let main = program.main.ok_or_else(|| {
        FosterError::runtime("native compilation requires a `main` function").with_code("E0900")
    })?;
    let reachable = reachable_functions(&program, main)?;
    let function_types = collect_function_types(compilation, &reachable)?;
    if matches!(
        function_types[&main].1,
        NativeType::Arguments | NativeType::StringList
    ) {
        return Err(native_error(
            "native `main` cannot return Arguments or List<String>",
        ));
    }
    validate_program(compilation, &program, &reachable, &function_types)?;

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
    let (runtime_strings, runtime_string_indices) = runtime_strings(&program);

    let mut native_ids = HashMap::new();
    let mut ordered = reachable.iter().copied().collect::<Vec<_>>();
    ordered.sort_unstable_by_key(|function| function.into_raw().into_u32());
    for function in &ordered {
        let bytecode = &program.functions[function];
        let signature = signature(&mut module, usize::from(bytecode.parameters));
        let linkage = if *function == main {
            Linkage::Export
        } else {
            Linkage::Local
        };
        let symbol = if *function == main {
            "foster_native_entry".to_owned()
        } else {
            format!("foster_fn_{}", function.into_raw().into_u32())
        };
        let id = module
            .declare_function(&symbol, linkage, &signature)
            .map_err(|error| {
                native_error(format!("cannot declare `{}`: {error}", bytecode.name))
            })?;
        native_ids.insert(*function, id);
    }

    for function in ordered {
        define_function(
            &mut module,
            &program,
            function,
            native_ids[&function],
            &native_ids,
            &function_types,
            &runtime_string_indices,
        )?;
    }

    let bytes = module
        .finish()
        .emit()
        .map_err(|error| native_error(format!("cannot encode the native object: {error}")))?;
    Ok(ObjectArtifact {
        bytes,
        result: function_types[&main].1,
        accepts_arguments: program.main_arguments,
        runtime_strings,
    })
}

/// Compile and link a standalone host executable using the installed Rust linker toolchain.
pub fn build_executable(
    compilation: &Compilation,
    output: impl AsRef<Path>,
    options: CompileOptions,
) -> Result<(), FosterError> {
    let output = absolute_path(output.as_ref())?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            native_error(format!(
                "cannot create output directory `{}`: {error}",
                parent.display()
            ))
        })?;
    }
    let artifact = compile_object(compilation, options)?;
    let temporary = TemporaryDirectory::create()?;
    let object = temporary.path.join(if cfg!(windows) {
        "program.obj"
    } else {
        "program.o"
    });
    let shim = temporary.path.join("entry.rs");
    fs::write(&object, artifact.bytes)
        .map_err(|error| native_error(format!("cannot write `{}`: {error}", object.display())))?;
    fs::write(
        &shim,
        entry_source(
            artifact.result,
            artifact.accepts_arguments,
            &artifact.runtime_strings,
        ),
    )
    .map_err(|error| native_error(format!("cannot write linker shim: {error}")))?;

    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let result = Command::new(&rustc)
        .arg("--edition=2024")
        .arg(&shim)
        .arg("-C")
        .arg(if options.optimize {
            "opt-level=2"
        } else {
            "opt-level=0"
        })
        .arg("-C")
        .arg(format!("link-arg={}", object.display()))
        .arg("-o")
        .arg(&output)
        .output()
        .map_err(|error| {
            native_error(format!(
                "cannot run `{}` to link the executable: {error}",
                Path::new(&rustc).display()
            ))
        })?;
    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        return Err(native_error(format!(
            "native linker failed with {}{}{}",
            result.status,
            if stderr.trim().is_empty() { "" } else { ": " },
            stderr.trim()
        )));
    }
    Ok(())
}

fn reachable_functions(
    program: &Program,
    main: FunctionId,
) -> Result<HashSet<FunctionId>, FosterError> {
    let mut reachable = HashSet::new();
    let mut pending = vec![main];
    while let Some(function) = pending.pop() {
        if !reachable.insert(function) {
            continue;
        }
        let body = program.functions.get(&function).ok_or_else(|| {
            native_error(format!(
                "native call references missing function #{}",
                function.into_raw().into_u32()
            ))
        })?;
        for instruction in &body.instructions {
            match instruction {
                Instruction::Call { function, .. } | Instruction::CallMethod { function, .. } => {
                    pending.push(*function);
                }
                _ => {}
            }
        }
    }
    Ok(reachable)
}

fn collect_function_types(
    compilation: &Compilation,
    reachable: &HashSet<FunctionId>,
) -> Result<HashMap<FunctionId, (Vec<NativeType>, NativeType)>, FosterError> {
    reachable
        .iter()
        .map(|function| {
            let definition = &compilation.hir.functions[*function];
            let signature = compilation.types.function_type(*function).ok_or_else(|| {
                native_error(format!(
                    "missing type information for `{}`",
                    definition.name
                ))
            })?;
            let parameters = signature
                .parameters
                .iter()
                .map(|ty| native_type(compilation, *ty, &definition.name))
                .collect::<Result<Vec<_>, _>>()?;
            let result = native_type(compilation, signature.result, &definition.name)?;
            Ok((*function, (parameters, result)))
        })
        .collect()
}

fn native_type(
    compilation: &Compilation,
    ty: TypeId,
    function: &str,
) -> Result<NativeType, FosterError> {
    match compilation.types.types[ty] {
        Type::Unit => Ok(NativeType::Unit),
        Type::Bool => Ok(NativeType::Bool),
        Type::Int => Ok(NativeType::Int),
        Type::Float => Ok(NativeType::Float),
        Type::CodePoint => Ok(NativeType::CodePoint),
        Type::Byte => Ok(NativeType::Byte),
        Type::Record {
            record,
            ref arguments,
        } if compilation.hir.records[record].name == "String"
            && compilation.hir.modules[compilation.hir.records[record].module].name
                == "core.string" =>
        {
            Ok(NativeType::String)
        }
        Type::Record { .. } if crate::entry::is_arguments_type(compilation, ty) => {
            Ok(NativeType::Arguments)
        }
        Type::Record {
            record,
            ref arguments,
        } if compilation.hir.records[record].name == "List"
            && compilation.hir.modules[compilation.hir.records[record].module].name
                == "core.list"
            && arguments.len() == 1
            && native_type(compilation, arguments[0], function)? == NativeType::String =>
        {
            Ok(NativeType::StringList)
        }
        ref unsupported => Err(native_error(format!(
            "native compilation of `{function}` does not yet support type `{}` ({unsupported:?})",
            compilation.types.display(ty)
        ))
        .with_help("use `foster build` without `--native` for the complete VM language")),
    }
}

fn validate_program(
    compilation: &Compilation,
    program: &Program,
    reachable: &HashSet<FunctionId>,
    function_types: &HashMap<FunctionId, (Vec<NativeType>, NativeType)>,
) -> Result<(), FosterError> {
    let main = program.main.expect("validated above");
    let main_function = &program.functions[&main];
    if main_function.parameters != u16::from(program.main_arguments) || main_function.captures != 0
    {
        return Err(native_error(
            "native `main` must take no parameters or one `std.process.Arguments` parameter",
        ));
    }
    for function in reachable {
        let body = &program.functions[function];
        if body.captures != 0 {
            return Err(native_error(format!(
                "native compilation does not yet support captures in `{}`",
                body.name
            )));
        }
        if usize::from(body.parameters) != function_types[function].0.len() {
            return Err(native_error(format!(
                "parameter metadata for `{}` is inconsistent",
                body.name
            )));
        }
        for (index, instruction) in body.instructions.iter().enumerate() {
            let supported = matches!(
                instruction,
                Instruction::Drop { .. }
                    | Instruction::LoadConstant { .. }
                    | Instruction::Move { .. }
                    | Instruction::Unary { .. }
                    | Instruction::Binary { .. }
                    | Instruction::Jump { .. }
                    | Instruction::JumpIfFalse { .. }
                    | Instruction::Assert { .. }
                    | Instruction::Call { .. }
                    | Instruction::CallMethod { .. }
                    | Instruction::LoadField { .. }
                    | Instruction::Index { .. }
                    | Instruction::CallContractMethod { .. }
                    | Instruction::Return { .. }
            );
            if !supported {
                let mut error = native_error(format!(
                    "native compilation of `{}` does not yet support instruction `{}`",
                    body.name,
                    instruction_name(instruction)
                ))
                .with_help("use `foster build` without `--native` for the complete VM language");
                if let Some(span) = body.instruction_spans.get(index) {
                    error =
                        error.with_primary_label(span.clone(), "unsupported in the native backend");
                }
                return Err(error);
            }
            if let Instruction::LoadConstant { constant, .. } = instruction
                && matches!(
                    program.constants[usize::from(*constant)],
                    Constant::Symbol(_)
                )
            {
                return Err(native_error(format!(
                    "native compilation of `{}` does not yet support symbol constants",
                    body.name
                )));
            }
        }
    }
    let _ = compilation;
    Ok(())
}

fn define_function(
    module: &mut ObjectModule,
    program: &Program,
    function_id: FunctionId,
    native_id: FuncId,
    native_ids: &HashMap<FunctionId, FuncId>,
    function_types: &HashMap<FunctionId, (Vec<NativeType>, NativeType)>,
    runtime_string_indices: &HashMap<u16, u64>,
) -> Result<(), FosterError> {
    let function = &program.functions[&function_id];
    let register_types = infer_register_types(
        program,
        function,
        &function_types[&function_id].0,
        function_types,
    )?;
    let frontend_config = module.target_config();
    let pointer_type = frontend_config.pointer_type();
    let mut context = module.make_context();
    context.func.signature = signature(module, usize::from(function.parameters));
    let mut builder_context = FunctionBuilderContext::new();
    {
        let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
        let lowering = LowerBodyContext {
            program,
            native_ids,
            register_types: &register_types,
            pointer_type,
            runtime_string_indices,
        };
        lower_body(&mut builder, module, function, &lowering)?;
        builder.finalize(frontend_config);
    }
    module
        .define_function(native_id, &mut context)
        .map_err(|error| native_error(format!("cannot compile `{}`: {error}", function.name)))?;
    module.clear_context(&mut context);
    Ok(())
}

fn signature(module: &mut ObjectModule, parameters: usize) -> Signature {
    let mut signature = module.make_signature();
    signature.params = vec![AbiParam::new(types::I64); parameters];
    signature.returns.push(AbiParam::new(types::I64));
    signature
}

fn infer_register_types(
    program: &Program,
    function: &BytecodeFunction,
    parameter_types: &[NativeType],
    function_types: &HashMap<FunctionId, (Vec<NativeType>, NativeType)>,
) -> Result<Vec<Option<NativeType>>, FosterError> {
    let mut result = vec![None; usize::from(function.registers)];
    for (index, ty) in parameter_types.iter().enumerate() {
        result[index] = Some(*ty);
    }
    for instruction in &function.instructions {
        match instruction {
            Instruction::LoadConstant {
                destination,
                constant,
            } => {
                result[usize::from(destination.0)] =
                    Some(match program.constants[usize::from(*constant)] {
                        Constant::Unit => NativeType::Unit,
                        Constant::Bool(_) => NativeType::Bool,
                        Constant::Integer(_) => NativeType::Int,
                        Constant::Float(_) => NativeType::Float,
                        Constant::CodePoint(_) => NativeType::CodePoint,
                        Constant::String(_) => NativeType::String,
                        Constant::Symbol(_) => unreachable!("validated above"),
                    });
            }
            Instruction::Move {
                destination,
                source,
            } => {
                result[usize::from(destination.0)] = result[usize::from(source.0)];
            }
            Instruction::Unary {
                destination,
                operator,
                operand,
            } => {
                result[usize::from(destination.0)] = Some(match operator {
                    UnaryOp::Negate => register_type(&result, *operand, function)?,
                    UnaryOp::Not => NativeType::Bool,
                    UnaryOp::BitNot => NativeType::Byte,
                });
            }
            Instruction::Binary {
                destination,
                operator,
                left,
                ..
            } => {
                result[usize::from(destination.0)] = Some(match operator {
                    BinaryOp::Equal
                    | BinaryOp::NotEqual
                    | BinaryOp::Less
                    | BinaryOp::LessEqual
                    | BinaryOp::Greater
                    | BinaryOp::GreaterEqual => NativeType::Bool,
                    BinaryOp::BitAnd
                    | BinaryOp::BitOr
                    | BinaryOp::BitXor
                    | BinaryOp::ShiftLeft
                    | BinaryOp::ShiftRight => NativeType::Byte,
                    _ => register_type(&result, *left, function)?,
                });
            }
            Instruction::Call {
                destination,
                function: callee,
                ..
            }
            | Instruction::CallMethod {
                destination,
                function: callee,
                ..
            } => {
                result[usize::from(destination.0)] = Some(function_types[callee].1);
            }
            Instruction::LoadField {
                destination,
                object,
                field,
                ..
            } => {
                let object = register_type(&result, *object, function)?;
                result[usize::from(destination.0)] = Some(field_type(object, field)?);
            }
            Instruction::Index {
                destination,
                object,
                ..
            } => {
                let object = register_type(&result, *object, function)?;
                result[usize::from(destination.0)] = Some(match object {
                    NativeType::StringList => NativeType::String,
                    NativeType::String => NativeType::CodePoint,
                    _ => {
                        return Err(native_error(format!(
                            "native indexing does not support `{object:?}` in `{}`",
                            function.name
                        )));
                    }
                });
            }
            Instruction::CallContractMethod {
                destination,
                receiver,
                name,
                arguments,
                ..
            } => {
                if !arguments.is_empty() {
                    return Err(native_error(format!(
                        "native contract call `{}` in `{}` does not yet accept arguments",
                        name, function.name
                    )));
                }
                let receiver = register_type(&result, *receiver, function)?;
                result[usize::from(destination.0)] = Some(field_type(receiver, name)?);
            }
            _ => {}
        }
    }
    Ok(result)
}

fn register_type(
    types: &[Option<NativeType>],
    register: Register,
    function: &BytecodeFunction,
) -> Result<NativeType, FosterError> {
    types[usize::from(register.0)].ok_or_else(|| {
        native_error(format!(
            "cannot determine the type of register r{} in `{}`",
            register.0, function.name
        ))
    })
}

fn field_type(receiver: NativeType, field: &str) -> Result<NativeType, FosterError> {
    match (receiver, field) {
        (NativeType::Arguments, "executable") => Ok(NativeType::String),
        (NativeType::Arguments, "values") => Ok(NativeType::StringList),
        (NativeType::StringList, "empty?") | (NativeType::String, "empty?") => Ok(NativeType::Bool),
        (NativeType::StringList, "length") | (NativeType::String, "length") => Ok(NativeType::Int),
        (NativeType::StringList, "head") => Ok(NativeType::String),
        (NativeType::String, "head") => Ok(NativeType::CodePoint),
        _ => Err(native_error(format!(
            "native compilation does not support field `{field}` on `{receiver:?}`"
        ))),
    }
}

struct LowerBodyContext<'a> {
    program: &'a Program,
    native_ids: &'a HashMap<FunctionId, FuncId>,
    register_types: &'a [Option<NativeType>],
    pointer_type: cranelift_codegen::ir::Type,
    runtime_string_indices: &'a HashMap<u16, u64>,
}

fn lower_body(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    function: &BytecodeFunction,
    context: &LowerBodyContext<'_>,
) -> Result<(), FosterError> {
    let leaders = block_leaders(function)?;
    let blocks = leaders
        .iter()
        .map(|leader| (*leader, builder.create_block()))
        .collect::<HashMap<_, _>>();
    let slot_size = u32::from(function.registers).max(1) * 8;
    let registers = builder.create_sized_stack_slot(StackSlotData::new(
        StackSlotKind::ExplicitSlot,
        slot_size,
        3,
    ));
    let entry = blocks[&0];
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    for (index, value) in builder.block_params(entry).to_vec().into_iter().enumerate() {
        builder.ins().stack_store(
            context.pointer_type,
            value,
            registers,
            register_offset(Register(index as u16))?,
        );
    }

    for (index, instruction) in function.instructions.iter().enumerate() {
        if index != 0 && blocks.contains_key(&index) {
            builder.switch_to_block(blocks[&index]);
        }
        let terminated = lower_instruction(
            builder,
            module,
            context.program,
            function,
            instruction,
            registers,
            &blocks,
            context.native_ids,
            context.register_types,
            index,
            context.pointer_type,
            context.runtime_string_indices,
        )?;
        let next = index + 1;
        if !terminated && blocks.contains_key(&next) {
            builder.ins().jump(blocks[&next], &[]);
        }
    }
    if function.instructions.is_empty() {
        let unit = builder.ins().iconst(types::I64, 0);
        builder.ins().return_(&[unit]);
    }
    builder.seal_all_blocks();
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn lower_instruction(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    program: &Program,
    function: &BytecodeFunction,
    instruction: &Instruction,
    registers: cranelift_codegen::ir::StackSlot,
    blocks: &HashMap<usize, cranelift_codegen::ir::Block>,
    native_ids: &HashMap<FunctionId, FuncId>,
    register_types: &[Option<NativeType>],
    index: usize,
    pointer_type: cranelift_codegen::ir::Type,
    runtime_string_indices: &HashMap<u16, u64>,
) -> Result<bool, FosterError> {
    let load = |builder: &mut FunctionBuilder<'_>, register: Register| -> Result<_, FosterError> {
        Ok(builder.ins().stack_load(
            pointer_type,
            types::I64,
            registers,
            register_offset(register)?,
        ))
    };
    let store = |builder: &mut FunctionBuilder<'_>,
                 register: Register,
                 value: cranelift_codegen::ir::Value|
     -> Result<(), FosterError> {
        builder
            .ins()
            .stack_store(pointer_type, value, registers, register_offset(register)?);
        Ok(())
    };
    match instruction {
        Instruction::Drop { .. } => {}
        Instruction::LoadConstant {
            destination,
            constant,
        } => {
            let value = match program.constants[usize::from(*constant)] {
                Constant::Unit => builder.ins().iconst(types::I64, 0),
                Constant::Bool(value) => builder.ins().iconst(types::I64, i64::from(value)),
                Constant::Integer(value) => builder.ins().iconst(types::I64, value),
                Constant::Float(value) => {
                    let value = builder.ins().f64const(value);
                    builder
                        .ins()
                        .bitcast(types::I64, MemFlagsData::new(), value)
                }
                Constant::CodePoint(value) => builder
                    .ins()
                    .iconst(types::I64, i64::from(u32::from(value))),
                Constant::String(_) => {
                    let index = builder
                        .ins()
                        .iconst(types::I64, runtime_string_indices[constant] as i64);
                    runtime_call(builder, module, "foster_string_constant", &[index])?
                }
                Constant::Symbol(_) => unreachable!("validated above"),
            };
            store(builder, *destination, value)?;
        }
        Instruction::Move {
            destination,
            source,
        } => {
            let value = load(builder, *source)?;
            store(builder, *destination, value)?;
        }
        Instruction::Unary {
            destination,
            operator,
            operand,
        } => {
            let word = load(builder, *operand)?;
            let value = match operator {
                UnaryOp::Negate
                    if register_type(register_types, *operand, function)? == NativeType::Float =>
                {
                    let float = builder.ins().bitcast(types::F64, MemFlagsData::new(), word);
                    let result = builder.ins().fneg(float);
                    builder
                        .ins()
                        .bitcast(types::I64, MemFlagsData::new(), result)
                }
                UnaryOp::Negate => {
                    let zero = builder.ins().iconst(types::I64, 0);
                    let result = builder.ins().ssub_overflow(zero, word);
                    builder.ins().trapnz(result.1, TrapCode::INTEGER_OVERFLOW);
                    result.0
                }
                UnaryOp::Not => {
                    let value = builder.ins().icmp_imm_s(IntCC::Equal, word, 0);
                    builder.ins().uextend(types::I64, value)
                }
                UnaryOp::BitNot => {
                    let value = builder.ins().bnot(word);
                    builder.ins().band_imm_u(value, 0xff)
                }
            };
            store(builder, *destination, value)?;
        }
        Instruction::Binary {
            destination,
            operator,
            left,
            right,
        } => {
            let left_value = load(builder, *left)?;
            let right_value = load(builder, *right)?;
            let operand_type = register_type(register_types, *left, function)?;
            let value = lower_binary(
                builder,
                module,
                *operator,
                operand_type,
                left_value,
                right_value,
            )?;
            store(builder, *destination, value)?;
        }
        Instruction::LoadField {
            destination,
            object,
            field,
            by_reference,
        } => {
            if *by_reference {
                return Err(native_error(format!(
                    "native compilation does not support reference field `{field}`"
                )));
            }
            let receiver = load(builder, *object)?;
            let receiver_type = register_type(register_types, *object, function)?;
            let helper = match (receiver_type, field.as_str()) {
                (NativeType::Arguments, "executable") => "foster_args_executable",
                (NativeType::Arguments, "values") => "foster_args_values",
                (NativeType::StringList, "empty?") => "foster_string_list_empty",
                (NativeType::StringList, "length") => "foster_string_list_length",
                (NativeType::StringList, "head") => "foster_string_list_head",
                (NativeType::String, "empty?") => "foster_string_empty",
                (NativeType::String, "length") => "foster_string_length",
                (NativeType::String, "head") => "foster_string_head",
                _ => {
                    return Err(native_error(format!(
                        "native compilation does not support field `{field}` on `{receiver_type:?}`"
                    )));
                }
            };
            let value = runtime_call(builder, module, helper, &[receiver])?;
            store(builder, *destination, value)?;
        }
        Instruction::Index {
            destination,
            object,
            index,
        } => {
            let receiver = load(builder, *object)?;
            let index = load(builder, *index)?;
            let receiver_type = register_type(register_types, *object, function)?;
            let helper = match receiver_type {
                NativeType::StringList => "foster_string_list_get",
                NativeType::String => "foster_string_get",
                _ => {
                    return Err(native_error(format!(
                        "native indexing does not support `{receiver_type:?}`"
                    )));
                }
            };
            let value = runtime_call(builder, module, helper, &[receiver, index])?;
            store(builder, *destination, value)?;
        }
        Instruction::CallContractMethod {
            destination,
            receiver,
            name,
            arguments,
            ..
        } => {
            if !arguments.is_empty() {
                return Err(native_error(format!(
                    "native contract call `{}` does not yet accept arguments",
                    name
                )));
            }
            let value = load(builder, *receiver)?;
            let receiver_type = register_type(register_types, *receiver, function)?;
            let helper = match (receiver_type, name.as_str()) {
                (NativeType::StringList, "empty?") => "foster_string_list_empty",
                (NativeType::StringList, "length") => "foster_string_list_length",
                (NativeType::StringList, "head") => "foster_string_list_head",
                (NativeType::String, "empty?") => "foster_string_empty",
                (NativeType::String, "length") => "foster_string_length",
                (NativeType::String, "head") => "foster_string_head",
                _ => {
                    return Err(native_error(format!(
                        "native compilation does not support contract property `{}` on `{receiver_type:?}`",
                        name
                    )));
                }
            };
            let value = runtime_call(builder, module, helper, &[value])?;
            store(builder, *destination, value)?;
        }
        Instruction::Jump { target } => {
            builder.ins().jump(blocks[target], &[]);
            return Ok(true);
        }
        Instruction::JumpIfFalse { condition, target } => {
            let condition = load(builder, *condition)?;
            let truthy = builder.ins().icmp_imm_s(IntCC::NotEqual, condition, 0);
            let fallthrough = blocks.get(&(index + 1)).ok_or_else(|| {
                native_error(format!(
                    "conditional jump at end of `{}` has no fallthrough",
                    function.name
                ))
            })?;
            builder
                .ins()
                .brif(truthy, *fallthrough, &[], blocks[target], &[]);
            return Ok(true);
        }
        Instruction::Assert { condition, message } => {
            let condition = load(builder, *condition)?;
            let message = match message {
                Some(message) => load(builder, *message)?,
                None => builder.ins().iconst(types::I64, 0),
            };
            runtime_call(builder, module, "foster_assert", &[condition, message])?;
        }
        Instruction::Call {
            destination,
            function: callee,
            arguments,
        } => {
            let reference = module.declare_func_in_func(native_ids[callee], builder.func);
            let arguments = arguments
                .iter()
                .map(|argument| load(builder, *argument))
                .collect::<Result<Vec<_>, _>>()?;
            let call = builder.ins().call(reference, &arguments);
            let result = builder.inst_results(call)[0];
            store(builder, *destination, result)?;
        }
        Instruction::CallMethod {
            destination,
            receiver,
            function: callee,
            arguments,
        } => {
            let reference = module.declare_func_in_func(native_ids[callee], builder.func);
            let mut values = vec![load(builder, *receiver)?];
            for argument in arguments {
                values.push(load(builder, *argument)?);
            }
            let call = builder.ins().call(reference, &values);
            let result = builder.inst_results(call)[0];
            store(builder, *destination, result)?;
        }
        Instruction::Return { source } => {
            let value = load(builder, *source)?;
            builder.ins().return_(&[value]);
            return Ok(true);
        }
        _ => unreachable!("validated above"),
    }
    Ok(false)
}

fn lower_binary(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    operator: BinaryOp,
    operand_type: NativeType,
    left: cranelift_codegen::ir::Value,
    right: cranelift_codegen::ir::Value,
) -> Result<cranelift_codegen::ir::Value, FosterError> {
    if operand_type == NativeType::String {
        let equal = runtime_call(builder, module, "foster_string_equal", &[left, right])?;
        return match operator {
            BinaryOp::Equal => Ok(equal),
            BinaryOp::NotEqual => {
                let one = builder.ins().iconst(types::I64, 1);
                Ok(builder.ins().bxor(equal, one))
            }
            _ => Err(native_error(format!(
                "native String values do not support operator `{operator:?}`"
            ))),
        };
    }
    if operand_type == NativeType::Float {
        let left = builder.ins().bitcast(types::F64, MemFlagsData::new(), left);
        let right = builder
            .ins()
            .bitcast(types::F64, MemFlagsData::new(), right);
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
        return Ok(builder
            .ins()
            .bitcast(types::I64, MemFlagsData::new(), result));
    }
    let result = match operator {
        BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply => {
            let pair = match operator {
                BinaryOp::Add => builder.ins().sadd_overflow(left, right),
                BinaryOp::Subtract => builder.ins().ssub_overflow(left, right),
                BinaryOp::Multiply => builder.ins().smul_overflow(left, right),
                _ => unreachable!(),
            };
            builder.ins().trapnz(pair.1, TrapCode::INTEGER_OVERFLOW);
            pair.0
        }
        BinaryOp::Divide => builder.ins().sdiv(left, right),
        BinaryOp::BitAnd => builder.ins().band(left, right),
        BinaryOp::BitOr => builder.ins().bor(left, right),
        BinaryOp::BitXor => builder.ins().bxor(left, right),
        BinaryOp::ShiftLeft | BinaryOp::ShiftRight => {
            let invalid = builder
                .ins()
                .icmp_imm_u(IntCC::UnsignedGreaterThan, right, 7);
            builder
                .ins()
                .trapnz(invalid, TrapCode::BAD_CONVERSION_TO_INTEGER);
            let shifted = if operator == BinaryOp::ShiftLeft {
                builder.ins().ishl(left, right)
            } else {
                builder.ins().ushr(left, right)
            };
            builder.ins().band_imm_u(shifted, 0xff)
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

fn runtime_call(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    name: &str,
    arguments: &[cranelift_codegen::ir::Value],
) -> Result<cranelift_codegen::ir::Value, FosterError> {
    let signature = signature(module, arguments.len());
    let function = module
        .declare_function(name, Linkage::Import, &signature)
        .map_err(|error| {
            native_error(format!("cannot declare native runtime `{name}`: {error}"))
        })?;
    let reference = module.declare_func_in_func(function, builder.func);
    let call = builder.ins().call(reference, arguments);
    Ok(builder.inst_results(call)[0])
}

fn integer_comparison(
    builder: &mut FunctionBuilder<'_>,
    condition: IntCC,
    left: cranelift_codegen::ir::Value,
    right: cranelift_codegen::ir::Value,
) -> cranelift_codegen::ir::Value {
    let result = builder.ins().icmp(condition, left, right);
    builder.ins().uextend(types::I64, result)
}

fn float_comparison(
    builder: &mut FunctionBuilder<'_>,
    condition: FloatCC,
    left: cranelift_codegen::ir::Value,
    right: cranelift_codegen::ir::Value,
) -> cranelift_codegen::ir::Value {
    let result = builder.ins().fcmp(condition, left, right);
    builder.ins().uextend(types::I64, result)
}

fn block_leaders(function: &BytecodeFunction) -> Result<Vec<usize>, FosterError> {
    let mut leaders = HashSet::from([0]);
    for (index, instruction) in function.instructions.iter().enumerate() {
        match instruction {
            Instruction::Jump { target } | Instruction::JumpIfFalse { target, .. } => {
                if *target >= function.instructions.len() {
                    return Err(native_error(format!(
                        "invalid jump target {target} in `{}`",
                        function.name
                    )));
                }
                leaders.insert(*target);
                if index + 1 < function.instructions.len() {
                    leaders.insert(index + 1);
                }
            }
            Instruction::Return { .. } if index + 1 < function.instructions.len() => {
                leaders.insert(index + 1);
            }
            _ => {}
        }
    }
    let mut leaders = leaders.into_iter().collect::<Vec<_>>();
    leaders.sort_unstable();
    Ok(leaders)
}

fn register_offset(register: Register) -> Result<i32, FosterError> {
    i32::try_from(u32::from(register.0) * 8)
        .map_err(|_| native_error("native function has too many registers"))
}

fn instruction_name(instruction: &Instruction) -> &'static str {
    match instruction {
        Instruction::MakeList { .. } => "MakeList",
        Instruction::Index { .. } => "Index",
        Instruction::MakeRecord { .. } => "MakeRecord",
        Instruction::MakeVariant { .. } => "MakeVariant",
        Instruction::LoadField { .. } => "LoadField",
        Instruction::StoreField { .. } => "StoreField",
        Instruction::StoreIndex { .. } => "StoreIndex",
        Instruction::MakeReference { .. } => "MakeReference",
        Instruction::MakeFieldReference { .. } => "MakeFieldReference",
        Instruction::MoveOut { .. } => "MoveOut",
        Instruction::Push { .. } => "Push",
        Instruction::Append { .. } => "Append",
        Instruction::Contains { .. } => "Contains",
        Instruction::Builtin { .. } => "Builtin",
        Instruction::SpawnRemote { .. } => "SpawnRemote",
        Instruction::SpawnRemoteBorrow { .. } => "SpawnRemoteBorrow",
        Instruction::RemoteCall { .. } => "RemoteCall",
        Instruction::Await { .. } => "Await",
        Instruction::MatchPattern { .. } => "MatchPattern",
        Instruction::Assert { .. } => "Assert",
        Instruction::CallContractMethod { .. } => "CallContractMethod",
        Instruction::MakeClosure { .. } => "MakeClosure",
        Instruction::CallValue { .. } => "CallValue",
        Instruction::CallClosure { .. } => "CallClosure",
        _ => "supported instruction",
    }
}

fn runtime_strings(program: &Program) -> (Vec<String>, HashMap<u16, u64>) {
    let mut values = Vec::new();
    let mut indices = HashMap::new();
    for (index, constant) in program.constants.iter().enumerate() {
        if let Constant::String(value) = constant {
            indices.insert(index as u16, values.len() as u64);
            values.push(value.clone());
        }
    }
    (values, indices)
}

fn entry_source(result: NativeType, accepts_arguments: bool, runtime_strings: &[String]) -> String {
    let print = match result {
        NativeType::Unit => String::new(),
        NativeType::Bool => {
            "println!(\"{}\", if value == 0 { \"false\" } else { \"true\" });".to_owned()
        }
        NativeType::Int => "println!(\"{value}\");".to_owned(),
        NativeType::Float => "println!(\"{}\", f64::from_bits(value as u64));".to_owned(),
        NativeType::CodePoint => {
            "println!(\"{}\", char::from_u32(value as u32).unwrap_or(char::REPLACEMENT_CHARACTER));"
                .to_owned()
        }
        NativeType::Byte => "println!(\"{}\", value as u8);".to_owned(),
        NativeType::String => "println!(\"{}\", unsafe { &*(value as *const String) });".to_owned(),
        NativeType::Arguments | NativeType::StringList => unreachable!("rejected above"),
    };
    let constants = runtime_strings
        .iter()
        .map(|value| format!("{value:?}.to_owned()"))
        .collect::<Vec<_>>()
        .join(", ");
    let declaration = if accepts_arguments {
        "unsafe extern \"C\" { fn foster_native_entry(arguments: i64) -> i64; }"
    } else {
        "unsafe extern \"C\" { fn foster_native_entry() -> i64; }"
    };
    let invocation = if accepts_arguments {
        "let mut supplied = std::env::args_os();\n    let executable = supplied.next().map(unicode_argument).unwrap_or_default();\n    let arguments = FosterArguments { executable, values: supplied.map(unicode_argument).collect() };\n    let value = unsafe { foster_native_entry(&arguments as *const FosterArguments as i64) };"
    } else {
        "let value = unsafe { foster_native_entry() };"
    };
    format!(
        r#"use std::ffi::OsString;
use std::sync::OnceLock;

struct FosterArguments {{
    executable: String,
    values: Vec<String>,
}}

static FOSTER_STRINGS: OnceLock<Vec<String>> = OnceLock::new();

fn constants() -> &'static Vec<String> {{
    FOSTER_STRINGS.get_or_init(|| vec![{constants}])
}}

fn unicode_argument(value: OsString) -> String {{
    value.into_string().unwrap_or_else(|_| {{
        eprintln!("error: command arguments must be valid Unicode");
        std::process::exit(2);
    }})
}}

fn bounds_error(kind: &str, index: i64, length: usize) -> ! {{
    eprintln!("error: {{kind}} index {{index}} is outside 0..{{length}}");
    std::process::exit(2);
}}

#[unsafe(no_mangle)]
extern "C" fn foster_assert(condition: i64, message: i64) -> i64 {{
    if condition == 0 {{
        if message == 0 {{
            eprintln!("error: assertion failed");
        }} else {{
            eprintln!("error: assertion failed: {{}}", unsafe {{ string_value(message) }});
        }}
        std::process::exit(2);
    }}
    0
}}

unsafe fn command_arguments<'a>(value: i64) -> &'a FosterArguments {{
    unsafe {{ &*(value as *const FosterArguments) }}
}}

unsafe fn string_list<'a>(value: i64) -> &'a Vec<String> {{
    unsafe {{ &*(value as *const Vec<String>) }}
}}

unsafe fn string_value<'a>(value: i64) -> &'a String {{
    unsafe {{ &*(value as *const String) }}
}}

#[unsafe(no_mangle)]
extern "C" fn foster_string_constant(index: i64) -> i64 {{
    let index = usize::try_from(index).unwrap_or_else(|_| bounds_error("constant", index, constants().len()));
    constants().get(index).map(|value| value as *const String as i64)
        .unwrap_or_else(|| bounds_error("constant", index as i64, constants().len()))
}}

#[unsafe(no_mangle)]
extern "C" fn foster_args_executable(value: i64) -> i64 {{
    unsafe {{ &command_arguments(value).executable as *const String as i64 }}
}}

#[unsafe(no_mangle)]
extern "C" fn foster_args_values(value: i64) -> i64 {{
    unsafe {{ &command_arguments(value).values as *const Vec<String> as i64 }}
}}

#[unsafe(no_mangle)]
extern "C" fn foster_string_list_empty(value: i64) -> i64 {{
    i64::from(unsafe {{ string_list(value).is_empty() }})
}}

#[unsafe(no_mangle)]
extern "C" fn foster_string_list_length(value: i64) -> i64 {{
    unsafe {{ string_list(value).len() as i64 }}
}}

#[unsafe(no_mangle)]
extern "C" fn foster_string_list_get(value: i64, index: i64) -> i64 {{
    let values = unsafe {{ string_list(value) }};
    let index = usize::try_from(index).unwrap_or_else(|_| bounds_error("argument", index, values.len()));
    values.get(index).map(|value| value as *const String as i64)
        .unwrap_or_else(|| bounds_error("argument", index as i64, values.len()))
}}

#[unsafe(no_mangle)]
extern "C" fn foster_string_list_head(value: i64) -> i64 {{
    foster_string_list_get(value, 0)
}}

#[unsafe(no_mangle)]
extern "C" fn foster_string_empty(value: i64) -> i64 {{
    i64::from(unsafe {{ string_value(value).is_empty() }})
}}

#[unsafe(no_mangle)]
extern "C" fn foster_string_length(value: i64) -> i64 {{
    unsafe {{ string_value(value).chars().count() as i64 }}
}}

#[unsafe(no_mangle)]
extern "C" fn foster_string_head(value: i64) -> i64 {{
    unsafe {{ string_value(value).chars().next() }}
        .map(|value| value as u32 as i64)
        .unwrap_or_else(|| bounds_error("string", 0, 0))
}}

#[unsafe(no_mangle)]
extern "C" fn foster_string_get(value: i64, index: i64) -> i64 {{
    let text = unsafe {{ string_value(value) }};
    let index = usize::try_from(index).unwrap_or_else(|_| bounds_error("string", index, text.chars().count()));
    text.chars().nth(index).map(|value| value as u32 as i64)
        .unwrap_or_else(|| bounds_error("string", index as i64, text.chars().count()))
}}

#[unsafe(no_mangle)]
extern "C" fn foster_string_equal(left: i64, right: i64) -> i64 {{
    i64::from(unsafe {{ string_value(left) == string_value(right) }})
}}

{declaration}

fn main() {{
    {invocation}
    {print}
}}
"#
    )
}

fn absolute_path(path: &Path) -> Result<PathBuf, FosterError> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|directory| directory.join(path))
            .map_err(|error| native_error(format!("cannot resolve output path: {error}")))
    }
}

fn native_error(message: impl Into<String>) -> FosterError {
    FosterError::runtime(message).with_code("E0900")
}

struct TemporaryDirectory {
    path: PathBuf,
}

impl TemporaryDirectory {
    fn create() -> Result<Self, FosterError> {
        let unique = format!(
            "foster-native-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| native_error(format!("system clock error: {error}")))?
                .as_nanos()
        );
        let path = std::env::temp_dir().join(unique);
        fs::create_dir(&path).map_err(|error| {
            native_error(format!(
                "cannot create native build directory `{}`: {error}",
                path.display()
            ))
        })?;
        Ok(Self { path })
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
