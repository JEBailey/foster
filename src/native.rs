//! Ahead-of-time native compilation through Cranelift.
//!
//! The native backend deliberately accepts a smaller language surface than the VM. Unsupported
//! operations are diagnosed before an object is emitted, which keeps the portable bytecode VM as
//! the reference implementation while native support grows.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use cranelift_codegen::ir::{AbiParam, InstBuilder, Signature as ClifSignature, TrapCode, types};
use cranelift_codegen::ir::{condcodes::FloatCC, condcodes::IntCC};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{FuncId, Linkage, Module, default_libcall_names};
use cranelift_object::{ObjectBuilder, ObjectModule};

use crate::ast::{BinaryOp, UnaryOp};
use crate::codegen::ir;
use crate::compiler::Compilation;
use crate::error::FosterError;
use crate::hir::FunctionId;
use crate::types::{Type, TypeId};
use crate::vm::{self, BytecodeFunction, Constant, Instruction, Program, Register};

/// Primitive Foster values supported by the native ABI.
pub use crate::codegen::ir::Type as NativeType;

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

/// Lower the reachable native subset and render deterministic typed SSA IR.
pub fn emit_ir(compilation: &Compilation) -> Result<String, FosterError> {
    let mut program =
        vm::compile_with_options(compilation, vm::CompileOptions { optimize: false })?;
    let layouts = crate::codegen::layout::legalize(&mut program)?;
    let main = program.main.ok_or_else(|| {
        FosterError::runtime("native compilation requires a `main` function").with_code("E0900")
    })?;
    let reachable = reachable_functions(&program, main)?;
    let function_types = collect_function_types(compilation, &reachable)?;
    if matches!(
        function_types[&main].result,
        NativeType::Arguments | NativeType::StringList
    ) {
        return Err(native_error(
            "native `main` cannot return Arguments or List<String>",
        ));
    }
    validate_program(compilation, &program, &reachable, &function_types, &layouts)?;
    let (_, runtime_string_indices) = runtime_strings(&program);
    let mut ordered = reachable.into_iter().collect::<Vec<_>>();
    ordered.sort_unstable_by_key(|function| function.into_raw().into_u32());
    let mut output = String::from("foster-codegen-ir 1\n\n");
    for function_id in ordered {
        let function = &program.functions[&function_id];
        let lowered = lower_to_native_ir(
            &program,
            function,
            &function_types[&function_id],
            &function_types,
            &runtime_string_indices,
        )?;
        lowered.verify(&function_types).map_err(|error| {
            native_error(format!(
                "invalid native IR for `{}`: {error}",
                function.name
            ))
        })?;
        output.push_str(&format!(
            "; function #{}\n{lowered}\n",
            function_id.into_raw().into_u32()
        ));
    }
    Ok(output)
}

/// Compile the reachable portion of `main` to a host-native object file.
pub fn compile_object(
    compilation: &Compilation,
    options: CompileOptions,
) -> Result<ObjectArtifact, FosterError> {
    // The non-optimized register program preserves one stable static type per register. Cranelift
    // still performs the requested machine-level optimization below.
    let mut program =
        vm::compile_with_options(compilation, vm::CompileOptions { optimize: false })?;
    let layouts = crate::codegen::layout::legalize(&mut program)?;
    let main = program.main.ok_or_else(|| {
        FosterError::runtime("native compilation requires a `main` function").with_code("E0900")
    })?;
    let reachable = reachable_functions(&program, main)?;
    let function_types = collect_function_types(compilation, &reachable)?;
    if matches!(
        function_types[&main].result,
        NativeType::Arguments | NativeType::StringList
    ) {
        return Err(native_error(
            "native `main` cannot return Arguments or List<String>",
        ));
    }
    validate_program(compilation, &program, &reachable, &function_types, &layouts)?;

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
        let signature = signature(&mut module, &function_types[function]);
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
        result: function_types[&main].result,
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
) -> Result<HashMap<FunctionId, ir::Signature>, FosterError> {
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
            Ok((*function, ir::Signature { parameters, result }))
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
        Type::Record { .. }
            if crate::entry::is_arguments_type(&compilation.hir, &compilation.types, ty) =>
        {
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
    function_types: &HashMap<FunctionId, ir::Signature>,
    layouts: &crate::codegen::layout::Registry,
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
        if usize::from(body.parameters) != function_types[function].parameters.len() {
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
                let layout_note = match instruction {
                    Instruction::MakeRecord { record, .. } => layouts
                        .record(*record)
                        .map(|layout| format!(" (legalized as boxed layout l{})", layout.0))
                        .unwrap_or_default(),
                    Instruction::MakeVariant { variant, .. } => program
                        .variants
                        .get(variant)
                        .and_then(|variant| layouts.variant(variant.parent))
                        .map(|layout| format!(" (legalized as boxed layout l{})", layout.0))
                        .unwrap_or_default(),
                    Instruction::MakeClosure { function, .. } => layouts
                        .closure(*function)
                        .map(|layout| format!(" (legalized as boxed layout l{})", layout.0))
                        .unwrap_or_default(),
                    _ => String::new(),
                };
                let mut error = native_error(format!(
                    "native compilation of `{}` does not yet support instruction `{}`{}",
                    body.name,
                    instruction_name(instruction),
                    layout_note
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
    function_types: &HashMap<FunctionId, ir::Signature>,
    runtime_string_indices: &HashMap<u16, u64>,
) -> Result<(), FosterError> {
    let function = &program.functions[&function_id];
    let native_function = lower_to_native_ir(
        program,
        function,
        &function_types[&function_id],
        function_types,
        runtime_string_indices,
    )?;
    native_function.verify(function_types).map_err(|error| {
        native_error(format!(
            "invalid native IR for `{}`: {error}",
            function.name
        ))
    })?;
    let frontend_config = module.target_config();
    let mut context = module.make_context();
    context.func.signature = signature(module, &function_types[&function_id]);
    let mut builder_context = FunctionBuilderContext::new();
    {
        let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
        lower_native_ir(&mut builder, module, &native_function, native_ids)?;
        builder.finalize(frontend_config);
    }
    module
        .define_function(native_id, &mut context)
        .map_err(|error| native_error(format!("cannot compile `{}`: {error}", function.name)))?;
    module.clear_context(&mut context);
    Ok(())
}

fn signature(module: &mut ObjectModule, source: &ir::Signature) -> ClifSignature {
    let mut signature = module.make_signature();
    let pointer_type = module.target_config().pointer_type();
    signature.params = source
        .parameters
        .iter()
        .map(|ty| AbiParam::new(cranelift_type(*ty, pointer_type)))
        .collect();
    signature
        .returns
        .push(AbiParam::new(cranelift_type(source.result, pointer_type)));
    signature
}

fn cranelift_type(
    ty: NativeType,
    pointer_type: cranelift_codegen::ir::Type,
) -> cranelift_codegen::ir::Type {
    match ty.representation() {
        ir::Representation::I8 => types::I8,
        ir::Representation::I32 => types::I32,
        ir::Representation::I64 => types::I64,
        ir::Representation::F64 => types::F64,
        ir::Representation::Pointer => pointer_type,
    }
}

fn infer_register_types(
    program: &Program,
    function: &BytecodeFunction,
    parameter_types: &[NativeType],
    function_types: &HashMap<FunctionId, ir::Signature>,
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
                result[usize::from(destination.0)] = Some(function_types[callee].result);
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

fn native_block_live_ins(
    function: &BytecodeFunction,
    leaders: &[usize],
    block_for_leader: &HashMap<usize, ir::Block>,
) -> Result<Vec<HashSet<Register>>, FosterError> {
    let mut uses = vec![HashSet::new(); leaders.len()];
    let mut definitions = vec![HashSet::new(); leaders.len()];
    let mut successors = vec![Vec::new(); leaders.len()];
    for (block_index, start) in leaders.iter().copied().enumerate() {
        let end = leaders
            .get(block_index + 1)
            .copied()
            .unwrap_or(function.instructions.len());
        for instruction in &function.instructions[start..end] {
            for register in native_instruction_uses(instruction) {
                if !definitions[block_index].contains(&register) {
                    uses[block_index].insert(register);
                }
            }
            definitions[block_index].extend(native_instruction_definitions(instruction));
        }
        successors[block_index] = match function.instructions.get(end.saturating_sub(1)) {
            Some(Instruction::Jump { target }) => vec![block_for_leader[target]],
            Some(Instruction::JumpIfFalse { target, .. }) => {
                let fallthrough = leaders.get(block_index + 1).ok_or_else(|| {
                    native_error(format!(
                        "conditional jump at end of `{}` has no fallthrough",
                        function.name
                    ))
                })?;
                vec![block_for_leader[target], block_for_leader[fallthrough]]
            }
            Some(Instruction::Return { .. }) | None => Vec::new(),
            Some(_) if block_index + 1 < leaders.len() => {
                vec![ir::Block((block_index + 1) as u32)]
            }
            Some(_) => Vec::new(),
        };
    }

    let mut live_in = vec![HashSet::new(); leaders.len()];
    let mut live_out = vec![HashSet::new(); leaders.len()];
    loop {
        let mut changed = false;
        for block in (0..leaders.len()).rev() {
            let next_out = successors[block]
                .iter()
                .flat_map(|successor| live_in[successor.0 as usize].iter().copied())
                .collect::<HashSet<_>>();
            let mut next_in = uses[block].clone();
            next_in.extend(
                next_out
                    .iter()
                    .filter(|register| !definitions[block].contains(register))
                    .copied(),
            );
            changed |= next_in != live_in[block] || next_out != live_out[block];
            live_in[block] = next_in;
            live_out[block] = next_out;
        }
        if !changed {
            return Ok(live_in);
        }
    }
}

fn native_instruction_definitions(instruction: &Instruction) -> Vec<Register> {
    match instruction {
        Instruction::Drop { register } => vec![*register],
        Instruction::LoadConstant { destination, .. }
        | Instruction::Move { destination, .. }
        | Instruction::Unary { destination, .. }
        | Instruction::Binary { destination, .. }
        | Instruction::LoadField { destination, .. }
        | Instruction::Index { destination, .. }
        | Instruction::Call { destination, .. }
        | Instruction::CallMethod { destination, .. }
        | Instruction::CallContractMethod { destination, .. } => vec![*destination],
        Instruction::Jump { .. }
        | Instruction::JumpIfFalse { .. }
        | Instruction::Assert { .. }
        | Instruction::Return { .. } => Vec::new(),
        _ => unreachable!("validated native instruction subset"),
    }
}

fn native_instruction_uses(instruction: &Instruction) -> Vec<Register> {
    match instruction {
        Instruction::Drop { .. } | Instruction::LoadConstant { .. } | Instruction::Jump { .. } => {
            Vec::new()
        }
        Instruction::Move { source, .. } => vec![*source],
        Instruction::Unary { operand, .. } => vec![*operand],
        Instruction::Binary { left, right, .. } => vec![*left, *right],
        Instruction::LoadField { object, .. } => vec![*object],
        Instruction::Index { object, index, .. } => vec![*object, *index],
        Instruction::JumpIfFalse { condition, .. } => vec![*condition],
        Instruction::Assert { condition, message } => {
            let mut uses = vec![*condition];
            uses.extend(message);
            uses
        }
        Instruction::Call { arguments, .. } => arguments.clone(),
        Instruction::CallMethod {
            receiver,
            arguments,
            ..
        }
        | Instruction::CallContractMethod {
            receiver,
            arguments,
            ..
        } => {
            let mut uses = vec![*receiver];
            uses.extend(arguments);
            uses
        }
        Instruction::Return { source } => vec![*source],
        _ => unreachable!("validated native instruction subset"),
    }
}

fn lower_to_native_ir(
    program: &Program,
    function: &BytecodeFunction,
    function_signature: &ir::Signature,
    function_types: &HashMap<FunctionId, ir::Signature>,
    runtime_string_indices: &HashMap<u16, u64>,
) -> Result<ir::Function, FosterError> {
    let inferred = infer_register_types(
        program,
        function,
        &function_signature.parameters,
        function_types,
    )?;
    let register_types = inferred
        .iter()
        .map(|ty| ty.unwrap_or(NativeType::Unit))
        .collect::<Vec<_>>();
    let leaders = block_leaders(function)?;
    let block_for_leader = leaders
        .iter()
        .enumerate()
        .map(|(block, leader)| (*leader, ir::Block(block as u32)))
        .collect::<HashMap<_, _>>();
    let live_ins = native_block_live_ins(function, &leaders, &block_for_leader)?;
    let mut value_types = Vec::new();
    let function_parameters = function_signature
        .parameters
        .iter()
        .map(|ty| allocate_native_value(&mut value_types, *ty))
        .collect::<Vec<_>>();
    let mut parameter_registers = Vec::with_capacity(leaders.len());
    let mut block_parameters = Vec::with_capacity(leaders.len());
    for live_in in &live_ins {
        let mut registers = live_in.iter().copied().collect::<Vec<_>>();
        registers.sort_unstable_by_key(|register| register.0);
        let values = registers
            .iter()
            .map(|register| {
                register_type(&inferred, *register, function)
                    .map(|ty| allocate_native_value(&mut value_types, ty))
            })
            .collect::<Result<Vec<_>, _>>()?;
        parameter_registers.push(registers);
        block_parameters.push(values);
    }
    let entry_arguments = parameter_registers[0]
        .iter()
        .map(|register| {
            function_parameters
                .get(usize::from(register.0))
                .copied()
                .ok_or_else(|| {
                    native_error(format!(
                        "entry block of `{}` reads non-parameter r{}",
                        function.name, register.0
                    ))
                })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut blocks = Vec::with_capacity(leaders.len());
    for (block_index, start) in leaders.iter().copied().enumerate() {
        let end = leaders
            .get(block_index + 1)
            .copied()
            .unwrap_or(function.instructions.len());
        let mut state = vec![None; usize::from(function.registers)];
        for (register, value) in parameter_registers[block_index]
            .iter()
            .zip(&block_parameters[block_index])
        {
            state[usize::from(register.0)] = Some(*value);
        }
        let mut instructions = Vec::new();
        let mut terminator = None;

        for (index, instruction) in function.instructions[start..end].iter().enumerate() {
            let source_index = start + index;
            match instruction {
                Instruction::Drop { register } => {
                    state[usize::from(register.0)] = None;
                }
                Instruction::LoadConstant {
                    destination,
                    constant,
                } => {
                    let constant = match program.constants[usize::from(*constant)] {
                        Constant::Unit => ir::Constant::Unit,
                        Constant::Bool(value) => ir::Constant::Bool(value),
                        Constant::Integer(value) => ir::Constant::Integer(value),
                        Constant::Float(value) => ir::Constant::Float(value),
                        Constant::CodePoint(value) => ir::Constant::CodePoint(value),
                        Constant::String(_) => {
                            ir::Constant::RuntimeString(runtime_string_indices[constant])
                        }
                        Constant::Symbol(_) => unreachable!("validated above"),
                    };
                    let destination = define_native_register(
                        *destination,
                        &register_types,
                        &mut state,
                        &mut value_types,
                    );
                    instructions.push(ir::Instruction::Constant {
                        destination,
                        value: constant,
                    });
                }
                Instruction::Move {
                    destination,
                    source,
                } => {
                    state[usize::from(destination.0)] =
                        Some(native_register(&state, *source, function)?);
                }
                Instruction::Unary {
                    destination,
                    operator,
                    operand,
                } => {
                    let operand = native_register(&state, *operand, function)?;
                    let destination = define_native_register(
                        *destination,
                        &register_types,
                        &mut state,
                        &mut value_types,
                    );
                    instructions.push(ir::Instruction::Unary {
                        destination,
                        operator: *operator,
                        operand,
                    });
                }
                Instruction::Binary {
                    destination,
                    operator,
                    left,
                    right,
                } => {
                    let left_type = register_type(&inferred, *left, function)?;
                    let right_type = register_type(&inferred, *right, function)?;
                    let mut left = native_register(&state, *left, function)?;
                    let mut right = native_register(&state, *right, function)?;
                    if left_type == NativeType::Int
                        && matches!(right_type, NativeType::Byte | NativeType::CodePoint)
                    {
                        right = extend_native_integer(right, &mut instructions, &mut value_types);
                    } else if right_type == NativeType::Int
                        && matches!(left_type, NativeType::Byte | NativeType::CodePoint)
                    {
                        left = extend_native_integer(left, &mut instructions, &mut value_types);
                    }
                    let destination = define_native_register(
                        *destination,
                        &register_types,
                        &mut state,
                        &mut value_types,
                    );
                    instructions.push(ir::Instruction::Binary {
                        destination,
                        operator: *operator,
                        left,
                        right,
                    });
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
                    let helper =
                        native_field_helper(register_type(&inferred, *object, function)?, field)?;
                    let arguments = vec![native_register(&state, *object, function)?];
                    let destination = define_native_register(
                        *destination,
                        &register_types,
                        &mut state,
                        &mut value_types,
                    );
                    instructions.push(ir::Instruction::RuntimeCall {
                        destination,
                        helper,
                        signature: runtime_signature(destination, &arguments, &value_types),
                        arguments,
                    });
                }
                Instruction::Index {
                    destination,
                    object,
                    index,
                } => {
                    let receiver_type = register_type(&inferred, *object, function)?;
                    let helper = match receiver_type {
                        NativeType::StringList => "foster_string_list_get",
                        NativeType::String => "foster_string_get",
                        _ => {
                            return Err(native_error(format!(
                                "native indexing does not support `{receiver_type:?}`"
                            )));
                        }
                    };
                    let arguments = vec![
                        native_register(&state, *object, function)?,
                        native_register(&state, *index, function)?,
                    ];
                    let destination = define_native_register(
                        *destination,
                        &register_types,
                        &mut state,
                        &mut value_types,
                    );
                    instructions.push(ir::Instruction::RuntimeCall {
                        destination,
                        helper,
                        signature: runtime_signature(destination, &arguments, &value_types),
                        arguments,
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
                            "native contract call `{name}` does not yet accept arguments"
                        )));
                    }
                    let helper =
                        native_field_helper(register_type(&inferred, *receiver, function)?, name)?;
                    let arguments = vec![native_register(&state, *receiver, function)?];
                    let destination = define_native_register(
                        *destination,
                        &register_types,
                        &mut state,
                        &mut value_types,
                    );
                    instructions.push(ir::Instruction::RuntimeCall {
                        destination,
                        helper,
                        signature: runtime_signature(destination, &arguments, &value_types),
                        arguments,
                    });
                }
                Instruction::Jump { target } => {
                    terminator = Some(ir::Terminator::Jump {
                        target: block_for_leader[target],
                        arguments: native_edge_arguments(
                            block_for_leader[target],
                            &parameter_registers,
                            &state,
                            function,
                        )?,
                    });
                    break;
                }
                Instruction::JumpIfFalse { condition, target } => {
                    let fallthrough =
                        block_for_leader.get(&(source_index + 1)).ok_or_else(|| {
                            native_error(format!(
                                "conditional jump at end of `{}` has no fallthrough",
                                function.name
                            ))
                        })?;
                    terminator = Some(ir::Terminator::Branch {
                        condition: native_register(&state, *condition, function)?,
                        then_target: *fallthrough,
                        then_arguments: native_edge_arguments(
                            *fallthrough,
                            &parameter_registers,
                            &state,
                            function,
                        )?,
                        else_target: block_for_leader[target],
                        else_arguments: native_edge_arguments(
                            block_for_leader[target],
                            &parameter_registers,
                            &state,
                            function,
                        )?,
                    });
                    break;
                }
                Instruction::Assert { condition, message } => {
                    instructions.push(ir::Instruction::Assert {
                        condition: native_register(&state, *condition, function)?,
                        message: message
                            .map(|message| native_register(&state, message, function))
                            .transpose()?,
                    });
                }
                Instruction::Call {
                    destination,
                    function: callee,
                    arguments,
                } => {
                    let arguments = arguments
                        .iter()
                        .map(|argument| native_register(&state, *argument, function))
                        .collect::<Result<Vec<_>, _>>()?;
                    let destination = define_native_register(
                        *destination,
                        &register_types,
                        &mut state,
                        &mut value_types,
                    );
                    instructions.push(ir::Instruction::Call {
                        destination,
                        function: *callee,
                        arguments,
                    });
                }
                Instruction::CallMethod {
                    destination,
                    receiver,
                    function: callee,
                    arguments,
                } => {
                    let mut values = vec![native_register(&state, *receiver, function)?];
                    values.extend(
                        arguments
                            .iter()
                            .map(|argument| native_register(&state, *argument, function))
                            .collect::<Result<Vec<_>, _>>()?,
                    );
                    let destination = define_native_register(
                        *destination,
                        &register_types,
                        &mut state,
                        &mut value_types,
                    );
                    instructions.push(ir::Instruction::Call {
                        destination,
                        function: *callee,
                        arguments: values,
                    });
                }
                Instruction::Return { source } => {
                    terminator = Some(ir::Terminator::Return(native_register(
                        &state, *source, function,
                    )?));
                    break;
                }
                _ => unreachable!("validated above"),
            }
        }

        let terminator = match terminator {
            Some(terminator) => terminator,
            None if block_index + 1 < leaders.len() => ir::Terminator::Jump {
                target: ir::Block((block_index + 1) as u32),
                arguments: native_edge_arguments(
                    ir::Block((block_index + 1) as u32),
                    &parameter_registers,
                    &state,
                    function,
                )?,
            },
            None => {
                let unit = allocate_native_value(&mut value_types, NativeType::Unit);
                instructions.push(ir::Instruction::Constant {
                    destination: unit,
                    value: ir::Constant::Unit,
                });
                ir::Terminator::Return(unit)
            }
        };
        blocks.push(ir::BlockData {
            parameters: block_parameters[block_index].clone(),
            instructions,
            terminator,
        });
    }

    Ok(ir::Function {
        name: function.name.clone(),
        signature: function_signature.clone(),
        parameters: function_parameters,
        entry: ir::Block(0),
        entry_arguments,
        value_types,
        blocks,
    })
}

fn allocate_native_value(value_types: &mut Vec<NativeType>, ty: NativeType) -> ir::Value {
    let value = ir::Value(value_types.len() as u32);
    value_types.push(ty);
    value
}

fn define_native_register(
    register: Register,
    register_types: &[NativeType],
    state: &mut [Option<ir::Value>],
    value_types: &mut Vec<NativeType>,
) -> ir::Value {
    let index = usize::from(register.0);
    let value = allocate_native_value(value_types, register_types[index]);
    state[index] = Some(value);
    value
}

fn native_register(
    state: &[Option<ir::Value>],
    register: Register,
    function: &BytecodeFunction,
) -> Result<ir::Value, FosterError> {
    state[usize::from(register.0)].ok_or_else(|| {
        native_error(format!(
            "native lowering reads unavailable r{} in `{}`",
            register.0, function.name
        ))
    })
}

fn native_edge_arguments(
    target: ir::Block,
    parameter_registers: &[Vec<Register>],
    state: &[Option<ir::Value>],
    function: &BytecodeFunction,
) -> Result<Vec<ir::Value>, FosterError> {
    parameter_registers[target.0 as usize]
        .iter()
        .map(|register| native_register(state, *register, function))
        .collect()
}

fn runtime_signature(
    destination: ir::Value,
    arguments: &[ir::Value],
    value_types: &[NativeType],
) -> ir::Signature {
    ir::Signature {
        parameters: arguments
            .iter()
            .map(|value| value_types[value.0 as usize])
            .collect(),
        result: value_types[destination.0 as usize],
    }
}

fn extend_native_integer(
    operand: ir::Value,
    instructions: &mut Vec<ir::Instruction>,
    value_types: &mut Vec<NativeType>,
) -> ir::Value {
    let destination = allocate_native_value(value_types, NativeType::Int);
    instructions.push(ir::Instruction::IntegerExtend {
        destination,
        operand,
    });
    destination
}

fn native_field_helper(receiver: NativeType, field: &str) -> Result<&'static str, FosterError> {
    match (receiver, field) {
        (NativeType::Arguments, "executable") => Ok("foster_args_executable"),
        (NativeType::Arguments, "values") => Ok("foster_args_values"),
        (NativeType::StringList, "empty?") => Ok("foster_string_list_empty"),
        (NativeType::StringList, "length") => Ok("foster_string_list_length"),
        (NativeType::StringList, "head") => Ok("foster_string_list_head"),
        (NativeType::String, "empty?") => Ok("foster_string_empty"),
        (NativeType::String, "length") => Ok("foster_string_length"),
        (NativeType::String, "head") => Ok("foster_string_head"),
        _ => Err(native_error(format!(
            "native compilation does not support field `{field}` on `{receiver:?}`"
        ))),
    }
}

fn lower_native_ir(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    function: &ir::Function,
    native_ids: &HashMap<FunctionId, FuncId>,
) -> Result<(), FosterError> {
    let pointer_type = module.target_config().pointer_type();
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
        }
        for instruction in &block.instructions {
            let result = lower_native_instruction(
                builder,
                module,
                function,
                instruction,
                &values,
                native_ids,
            )?;
            if let Some(destination) = instruction.destination() {
                values.insert(
                    destination,
                    result.expect("value-producing native instruction"),
                );
            }
        }
        lower_native_terminator(builder, &block.terminator, &blocks, &values);
    }
    builder.seal_all_blocks();
    Ok(())
}

fn lower_native_instruction(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    function: &ir::Function,
    instruction: &ir::Instruction,
    values: &HashMap<ir::Value, cranelift_codegen::ir::Value>,
    native_ids: &HashMap<FunctionId, FuncId>,
) -> Result<Option<cranelift_codegen::ir::Value>, FosterError> {
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
                    "foster_string_constant",
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
                    builder.ins().trapnz(result.1, TrapCode::INTEGER_OVERFLOW);
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
        )?,
        ir::Instruction::Call {
            function,
            arguments,
            ..
        } => {
            let reference = module.declare_func_in_func(native_ids[function], builder.func);
            let arguments = arguments.iter().map(get).collect::<Vec<_>>();
            let call = builder.ins().call(reference, &arguments);
            builder.inst_results(call)[0]
        }
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
                "foster_assert",
                &ir::Signature {
                    parameters: vec![NativeType::Bool, NativeType::String],
                    result: NativeType::Unit,
                },
                &[condition, message],
            )?;
            return Ok(None);
        }
    };
    Ok(Some(result))
}

fn lower_native_terminator(
    builder: &mut FunctionBuilder<'_>,
    terminator: &ir::Terminator,
    blocks: &[cranelift_codegen::ir::Block],
    values: &HashMap<ir::Value, cranelift_codegen::ir::Value>,
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

fn lower_binary(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    operator: BinaryOp,
    operand_type: NativeType,
    left: cranelift_codegen::ir::Value,
    right: cranelift_codegen::ir::Value,
) -> Result<cranelift_codegen::ir::Value, FosterError> {
    if operand_type == NativeType::String {
        let equal = runtime_call(
            builder,
            module,
            "foster_string_equal",
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

fn runtime_call(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    name: &str,
    source_signature: &ir::Signature,
    arguments: &[cranelift_codegen::ir::Value],
) -> Result<cranelift_codegen::ir::Value, FosterError> {
    let signature = signature(module, source_signature);
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
    builder.ins().icmp(condition, left, right)
}

fn float_comparison(
    builder: &mut FunctionBuilder<'_>,
    condition: FloatCC,
    left: cranelift_codegen::ir::Value,
    right: cranelift_codegen::ir::Value,
) -> cranelift_codegen::ir::Value {
    builder.ins().fcmp(condition, left, right)
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
        Instruction::MakeWholeReference { .. } => "MakeWholeReference",
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
        NativeType::Float => "println!(\"{value}\");".to_owned(),
        NativeType::CodePoint => {
            "println!(\"{}\", char::from_u32(value).unwrap_or(char::REPLACEMENT_CHARACTER));"
                .to_owned()
        }
        NativeType::Byte => "println!(\"{value}\");".to_owned(),
        NativeType::String => "println!(\"{}\", unsafe { &*(value as *const String) });".to_owned(),
        NativeType::Arguments | NativeType::StringList => unreachable!("rejected above"),
    };
    let constants = runtime_strings
        .iter()
        .map(|value| format!("{value:?}.to_owned()"))
        .collect::<Vec<_>>()
        .join(", ");
    let result_type = match result {
        NativeType::Unit | NativeType::Bool | NativeType::Byte => "u8",
        NativeType::CodePoint => "u32",
        NativeType::Int => "i64",
        NativeType::String => "usize",
        NativeType::Float => "f64",
        NativeType::Arguments | NativeType::StringList => unreachable!("rejected above"),
    };
    let declaration = if accepts_arguments {
        format!(
            "unsafe extern \"C\" {{ fn foster_native_entry(arguments: usize) -> {result_type}; }}"
        )
    } else {
        format!("unsafe extern \"C\" {{ fn foster_native_entry() -> {result_type}; }}")
    };
    let invocation = if accepts_arguments {
        "let mut supplied = std::env::args_os();\n    let executable = supplied.next().map(unicode_argument).unwrap_or_default();\n    let arguments = FosterArguments { executable, values: supplied.map(unicode_argument).collect() };\n    let value = unsafe { foster_native_entry(&arguments as *const FosterArguments as usize) };"
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
extern "C" fn foster_assert(condition: u8, message: usize) -> u8 {{
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

unsafe fn command_arguments<'a>(value: usize) -> &'a FosterArguments {{
    unsafe {{ &*(value as *const FosterArguments) }}
}}

unsafe fn string_list<'a>(value: usize) -> &'a Vec<String> {{
    unsafe {{ &*(value as *const Vec<String>) }}
}}

unsafe fn string_value<'a>(value: usize) -> &'a String {{
    unsafe {{ &*(value as *const String) }}
}}

#[unsafe(no_mangle)]
extern "C" fn foster_string_constant(index: i64) -> usize {{
    let index = usize::try_from(index).unwrap_or_else(|_| bounds_error("constant", index, constants().len()));
    constants().get(index).map(|value| value as *const String as usize)
        .unwrap_or_else(|| bounds_error("constant", index as i64, constants().len()))
}}

#[unsafe(no_mangle)]
extern "C" fn foster_args_executable(value: usize) -> usize {{
    unsafe {{ &command_arguments(value).executable as *const String as usize }}
}}

#[unsafe(no_mangle)]
extern "C" fn foster_args_values(value: usize) -> usize {{
    unsafe {{ &command_arguments(value).values as *const Vec<String> as usize }}
}}

#[unsafe(no_mangle)]
extern "C" fn foster_string_list_empty(value: usize) -> u8 {{
    u8::from(unsafe {{ string_list(value).is_empty() }})
}}

#[unsafe(no_mangle)]
extern "C" fn foster_string_list_length(value: usize) -> i64 {{
    unsafe {{ string_list(value).len() as i64 }}
}}

#[unsafe(no_mangle)]
extern "C" fn foster_string_list_get(value: usize, index: i64) -> usize {{
    let values = unsafe {{ string_list(value) }};
    let index = usize::try_from(index).unwrap_or_else(|_| bounds_error("argument", index, values.len()));
    values.get(index).map(|value| value as *const String as usize)
        .unwrap_or_else(|| bounds_error("argument", index as i64, values.len()))
}}

#[unsafe(no_mangle)]
extern "C" fn foster_string_list_head(value: usize) -> usize {{
    foster_string_list_get(value, 0)
}}

#[unsafe(no_mangle)]
extern "C" fn foster_string_empty(value: usize) -> u8 {{
    u8::from(unsafe {{ string_value(value).is_empty() }})
}}

#[unsafe(no_mangle)]
extern "C" fn foster_string_length(value: usize) -> i64 {{
    unsafe {{ string_value(value).chars().count() as i64 }}
}}

#[unsafe(no_mangle)]
extern "C" fn foster_string_head(value: usize) -> u32 {{
    unsafe {{ string_value(value).chars().next() }}
        .map(|value| value as u32)
        .unwrap_or_else(|| bounds_error("string", 0, 0))
}}

#[unsafe(no_mangle)]
extern "C" fn foster_string_get(value: usize, index: i64) -> u32 {{
    let text = unsafe {{ string_value(value) }};
    let index = usize::try_from(index).unwrap_or_else(|_| bounds_error("string", index, text.chars().count()));
    text.chars().nth(index).map(|value| value as u32)
        .unwrap_or_else(|| bounds_error("string", index as i64, text.chars().count()))
}}

#[unsafe(no_mangle)]
extern "C" fn foster_string_equal(left: usize, right: usize) -> u8 {{
    u8::from(unsafe {{ string_value(left) == string_value(right) }})
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_ir_is_ssa_and_control_flow_carries_block_arguments() {
        let compilation = crate::compile(
            r#"
func main() -> Int {
    let value = 0
    loop {
        value = value + 1
        break if value == 3
    }
    value
}
"#,
        )
        .unwrap();
        let program =
            vm::compile_with_options(&compilation, vm::CompileOptions { optimize: false }).unwrap();
        let main = program.main.unwrap();
        let reachable = reachable_functions(&program, main).unwrap();
        let function_types = collect_function_types(&compilation, &reachable).unwrap();
        let (_, runtime_string_indices) = runtime_strings(&program);
        let function = lower_to_native_ir(
            &program,
            &program.functions[&main],
            &function_types[&main],
            &function_types,
            &runtime_string_indices,
        )
        .unwrap();

        assert!(function.blocks.len() > 1);
        function.verify(&function_types).unwrap();
        let mut definitions = function.parameters.iter().copied().collect::<HashSet<_>>();
        let mut has_branch = false;
        let mut has_back_edge = false;
        let mut has_pruned_parameters = false;
        for (block_index, block) in function.blocks.iter().enumerate() {
            has_pruned_parameters |=
                block.parameters.len() < usize::from(program.functions[&main].registers);
            for parameter in &block.parameters {
                assert!(definitions.insert(*parameter));
            }
            for instruction in &block.instructions {
                if let Some(destination) = instruction.destination() {
                    assert!(definitions.insert(destination));
                }
            }
            match &block.terminator {
                ir::Terminator::Jump { target, arguments } => {
                    assert_eq!(
                        arguments.len(),
                        function.blocks[target.0 as usize].parameters.len()
                    );
                    has_back_edge |= target.0 as usize <= block_index;
                }
                ir::Terminator::Branch {
                    then_target,
                    then_arguments,
                    else_target,
                    else_arguments,
                    ..
                } => {
                    has_branch = true;
                    assert_eq!(
                        then_arguments.len(),
                        function.blocks[then_target.0 as usize].parameters.len()
                    );
                    assert_eq!(
                        else_arguments.len(),
                        function.blocks[else_target.0 as usize].parameters.len()
                    );
                    has_back_edge |= then_target.0 as usize <= block_index
                        || else_target.0 as usize <= block_index;
                }
                ir::Terminator::Return(_) => {}
            }
        }
        assert_eq!(definitions.len(), function.value_types.len());
        assert!(has_branch);
        assert!(has_back_edge);
        assert!(has_pruned_parameters);
        assert_eq!(NativeType::Bool.representation(), ir::Representation::I8);
        assert_eq!(NativeType::Float.representation(), ir::Representation::F64);
    }
}
