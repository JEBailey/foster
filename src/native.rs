//! Ahead-of-time native compilation through Cranelift.
//!
//! The native backend deliberately accepts a smaller language surface than the VM. Unsupported
//! operations are diagnosed before an object is emitted, which keeps the portable bytecode VM as
//! the reference implementation while native support grows.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::Command;

use cranelift_codegen::ir::{
    AbiParam, Block as ClifBlock, InstBuilder, MemFlagsData, Signature as ClifSignature, StackSlot,
    StackSlotData, StackSlotKind, TrapCode, Type as ClifType, Value as ClifValue, types,
};
use cranelift_codegen::ir::{condcodes::FloatCC, condcodes::IntCC};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module, default_libcall_names};
use cranelift_object::{ObjectBuilder, ObjectModule};
use la_arena::RawIdx;

use crate::ast::{BinaryOp, ParameterMode, UnaryOp};
use crate::codegen::ir;
use crate::codegen::layout::physical::{
    DropField, DropPlan, PhysicalKind, PhysicalRegistry, ScalarKind, TargetLayout, ValueLayout,
};
use crate::codegen::layout::{LayoutId, LayoutKind, Registry as LayoutRegistry};
use crate::compiler::Compilation;
use crate::error::FosterError;
use crate::hir::{FunctionId, Pattern};
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

/// Target-independent and target-specific layout information used while lowering objects.
#[derive(Clone, Copy)]
struct NativeLayouts<'a> {
    program: &'a Program,
    logical: &'a LayoutRegistry,
    physical: &'a PhysicalRegistry,
}

impl NativeLayouts<'_> {
    fn is_managed(self, layout: LayoutId) -> bool {
        self.logical.get(layout).materialized
            && !matches!(self.logical.get(layout).kind, LayoutKind::Pointer { .. })
            && !matches!(
                self.logical.get(layout).kind,
                LayoutKind::Record { record, .. } if Some(record) == self.program.string_record
            )
    }
}

/// Object-runtime symbols needed after portable IR has been legalized.
#[derive(Clone, Copy)]
struct ObjectRuntime<'a> {
    layouts: NativeLayouts<'a>,
    descriptors: &'a HashMap<LayoutId, DataId>,
    destructors: &'a HashMap<LayoutId, FuncId>,
}

impl ObjectRuntime<'_> {
    fn allocate(
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

    fn retain(self, builder: &mut FunctionBuilder<'_>, object: ClifValue, layout: LayoutId) {
        retain_object(builder, object, layout, self.layouts.physical);
    }

    fn release(
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

/// Inputs shared while rebuilding specialized portable bytecode as native SSA.
#[derive(Clone, Copy)]
struct NativeIrEnvironment<'a> {
    program: &'a Program,
    shared_functions: &'a HashMap<FunctionId, ir::Function>,
    function_types: &'a HashMap<FunctionId, ir::Signature>,
    runtime_string_indices: &'a HashMap<u16, u64>,
    layouts: &'a LayoutRegistry,
    physical_layouts: &'a PhysicalRegistry,
    instances: &'a HashMap<SpecializationKey, FunctionId>,
}

/// Shared immutable state for lowering one module's functions to Cranelift.
struct NativeBackend<'a> {
    ir: NativeIrEnvironment<'a>,
    functions: &'a HashMap<FunctionId, FuncId>,
    objects: ObjectRuntime<'a>,
}

#[derive(Clone, Copy)]
struct PatternSubject {
    value: ClifValue,
    ty: NativeType,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct SpecializationKey {
    function: FunctionId,
    substitutions: crate::vm::Specialization,
}

#[derive(Debug, Clone)]
struct NativeInstance {
    key: SpecializationKey,
    ir_function: FunctionId,
}

/// Lower the reachable native subset and render deterministic typed SSA IR.
pub fn emit_ir(compilation: &Compilation) -> Result<String, FosterError> {
    let shared = vm::compile_shared(compilation)?;
    let mut program = shared.metadata;
    let shared_functions = shared.functions;
    let mut layouts = crate::codegen::layout::legalize(&mut program)?;
    let main = program.main.ok_or_else(|| {
        FosterError::runtime("native compilation requires a `main` function").with_code("E0900")
    })?;
    let instances = reachable_instances(&program, &shared_functions, main)?;
    let instance_ids = instances
        .iter()
        .map(|instance| (instance.key.clone(), instance.ir_function))
        .collect::<HashMap<_, _>>();
    let function_types = collect_function_types(compilation, &program, &instances, &mut layouts)?;
    let physical_layouts =
        PhysicalRegistry::build(&layouts, TargetLayout::host()).map_err(|error| {
            native_error(format!("cannot calculate native object layouts: {error}"))
        })?;
    let main_instance = instances
        .iter()
        .find(|instance| instance.key.function == main && instance.key.substitutions.is_empty())
        .expect("main specialization is reachable");
    validate_program(compilation, &program, &instances, &function_types, &layouts)?;
    if matches!(
        function_types[&main_instance.ir_function].result,
        NativeType::Arguments | NativeType::StringList | NativeType::Object(_)
    ) {
        return Err(native_error(
            "native `main` cannot return Arguments or List<String>",
        ));
    }
    let (_, runtime_string_indices) = runtime_strings(&program);
    let environment = NativeIrEnvironment {
        program: &program,
        shared_functions: &shared_functions,
        function_types: &function_types,
        runtime_string_indices: &runtime_string_indices,
        layouts: &layouts,
        physical_layouts: &physical_layouts,
        instances: &instance_ids,
    };
    let mut output = String::from("foster-codegen-ir 1\n\n");
    for instance in &instances {
        let function = &program.functions[&instance.key.function];
        let lowered = lower_shared_to_native_ir(
            &shared_functions[&instance.key.function],
            function,
            &function_types[&instance.ir_function],
            &instance.key,
            environment,
        )?;
        lowered.verify(&function_types).map_err(|error| {
            native_error(format!(
                "invalid native IR for `{}`: {error}",
                function.name
            ))
        })?;
        output.push_str(&format!(
            "; function #{} {:?}\n{lowered}\n",
            instance.key.function.into_raw().into_u32(),
            instance.key.substitutions
        ));
    }
    Ok(output)
}

/// Compile the reachable portion of `main` to a host-native object file.
pub fn compile_object(
    compilation: &Compilation,
    options: CompileOptions,
) -> Result<ObjectArtifact, FosterError> {
    // Consume the compiler's verified first SSA artifact directly. Construction metadata remains
    // available for source spans, nominal identities, and storage-home type specialization;
    // Cranelift performs the requested machine-level optimization.
    let shared = vm::compile_shared(compilation)?;
    let mut program = shared.metadata;
    let shared_functions = shared.functions;
    let mut layouts = crate::codegen::layout::legalize(&mut program)?;
    let main = program.main.ok_or_else(|| {
        FosterError::runtime("native compilation requires a `main` function").with_code("E0900")
    })?;
    let instances = reachable_instances(&program, &shared_functions, main)?;
    let instance_ids = instances
        .iter()
        .map(|instance| (instance.key.clone(), instance.ir_function))
        .collect::<HashMap<_, _>>();
    let function_types = collect_function_types(compilation, &program, &instances, &mut layouts)?;
    let main_instance = instances
        .iter()
        .find(|instance| instance.key.function == main && instance.key.substitutions.is_empty())
        .expect("main specialization is reachable");
    validate_program(compilation, &program, &instances, &function_types, &layouts)?;
    if matches!(
        function_types[&main_instance.ir_function].result,
        NativeType::Arguments | NativeType::StringList | NativeType::Object(_)
    ) {
        return Err(native_error(
            "native `main` cannot return Arguments or List<String>",
        ));
    }
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
    let physical_layouts = PhysicalRegistry::build(&layouts, target_layout).map_err(|error| {
        native_error(format!("cannot calculate native object layouts: {error}"))
    })?;
    let layout_descriptors = emit_layout_descriptors(&mut module, &physical_layouts)?;
    let (runtime_strings, runtime_string_indices) = runtime_strings(&program);

    let mut native_ids = HashMap::new();
    for instance in &instances {
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

    let drop_ids = declare_layout_destructors(&mut module, &physical_layouts)?;
    let native_layouts = NativeLayouts {
        program: &program,
        logical: &layouts,
        physical: &physical_layouts,
    };
    define_layout_destructors(&mut module, native_layouts, &drop_ids)?;

    let backend = NativeBackend {
        ir: NativeIrEnvironment {
            program: &program,
            shared_functions: &shared_functions,
            function_types: &function_types,
            runtime_string_indices: &runtime_string_indices,
            layouts: &layouts,
            physical_layouts: &physical_layouts,
            instances: &instance_ids,
        },
        functions: &native_ids,
        objects: ObjectRuntime {
            layouts: native_layouts,
            descriptors: &layout_descriptors,
            destructors: &drop_ids,
        },
    };

    for instance in &instances {
        define_function(
            &mut module,
            instance,
            native_ids[&instance.ir_function],
            &backend,
        )?;
    }

    let bytes = module
        .finish()
        .emit()
        .map_err(|error| native_error(format!("cannot encode the native object: {error}")))?;
    Ok(ObjectArtifact {
        bytes,
        result: function_types[&main_instance.ir_function].result,
        accepts_arguments: program.main_arguments,
        runtime_strings,
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

fn reachable_instances(
    program: &Program,
    shared_functions: &HashMap<FunctionId, ir::Function>,
    main: FunctionId,
) -> Result<Vec<NativeInstance>, FosterError> {
    let mut reachable = BTreeSet::new();
    let mut pending = vec![SpecializationKey {
        function: main,
        substitutions: Vec::new(),
    }];
    while let Some(instance) = pending.pop() {
        if instance.substitutions.iter().any(|(_, ty)| ty.depth() > 64) {
            return Err(native_error(
                "native monomorphization encountered expanding polymorphic recursion",
            )
            .with_help(
                "use a non-expanding recursive type argument or an explicit boxed boundary",
            ));
        }
        if !reachable.insert(instance.clone()) {
            continue;
        }
        if reachable.len() > 16_384 {
            return Err(native_error(
                "native monomorphization exceeds 16384 reachable function instances",
            ));
        }
        let body = program.functions.get(&instance.function).ok_or_else(|| {
            native_error(format!(
                "native call references missing function #{}",
                instance.function.into_raw().into_u32()
            ))
        })?;
        let shared = shared_functions.get(&instance.function).ok_or_else(|| {
            native_error(format!(
                "native function `{}` has no shared SSA body",
                body.name
            ))
        })?;
        for instruction in shared.blocks.iter().flat_map(|block| &block.instructions) {
            let target = match instruction {
                ir::Instruction::Call {
                    function,
                    specialization,
                    ..
                } => Some((*function, specialization)),
                ir::Instruction::Portable(
                    ir::PortableInstruction::Call {
                        function,
                        specialization,
                        ..
                    }
                    | ir::PortableInstruction::CallMethod {
                        function,
                        specialization,
                        ..
                    }
                    | ir::PortableInstruction::MakeClosure {
                        function,
                        specialization,
                        ..
                    }
                    | ir::PortableInstruction::CallClosure {
                        function,
                        specialization,
                        ..
                    },
                ) => Some((*function, specialization)),
                _ => None,
            };
            if let Some((function, specialization)) = target {
                pending.push(SpecializationKey {
                    function,
                    substitutions: resolve_specialization(specialization, &instance.substitutions),
                });
            }
        }
    }
    let first_synthetic = program
        .functions
        .keys()
        .map(|function| function.into_raw().into_u32())
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| native_error("native function identity space is exhausted"))?;
    reachable
        .into_iter()
        .enumerate()
        .map(|(index, key)| {
            let raw = first_synthetic
                .checked_add(index as u32)
                .ok_or_else(|| native_error("native function identity space is exhausted"))?;
            Ok(NativeInstance {
                key,
                ir_function: FunctionId::from_raw(RawIdx::from_u32(raw)),
            })
        })
        .collect()
}

fn resolve_specialization(
    specialization: &crate::vm::Specialization,
    outer: &crate::vm::Specialization,
) -> crate::vm::Specialization {
    specialization
        .iter()
        .map(|(name, ty)| (name.clone(), ty.specialize(outer)))
        .collect()
}

fn collect_function_types(
    compilation: &Compilation,
    program: &Program,
    instances: &[NativeInstance],
    layouts: &mut LayoutRegistry,
) -> Result<HashMap<FunctionId, ir::Signature>, FosterError> {
    instances
        .iter()
        .map(|instance| {
            let function = instance.key.function;
            let definition = &compilation.hir.functions[function];
            let signature = compilation.types.function_type(function).ok_or_else(|| {
                native_error(format!(
                    "missing type information for `{}`",
                    definition.name
                ))
            })?;
            let mut parameters = program.functions[&function]
                .capture_types
                .iter()
                .map(|ty| {
                    let concrete = ty.specialize(&instance.key.substitutions);
                    layouts.instantiate_type(&concrete)?;
                    concrete_native_type(compilation, layouts, &concrete, &definition.name)
                })
                .collect::<Result<Vec<_>, FosterError>>()?;
            parameters.extend(
                signature
                    .parameters
                    .iter()
                    .map(|ty| {
                        native_type(
                            compilation,
                            layouts,
                            *ty,
                            &instance.key.substitutions,
                            &definition.name,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            );
            if layouts.closure(function).is_some() {
                layouts.instantiate_closure(function, &instance.key.substitutions)?;
            }
            for instruction in &program.functions[&function].instructions {
                if let Some(ty) =
                    instruction_layout_type(program, instruction, &instance.key.substitutions)
                {
                    layouts.instantiate_type(&ty)?;
                }
            }
            let result = if matches!(compilation.types.types[signature.result], Type::Function(_)) {
                concrete_closure_result(program, layouts, &instance.key)?
            } else {
                native_type(
                    compilation,
                    layouts,
                    signature.result,
                    &instance.key.substitutions,
                    &definition.name,
                )?
            };
            Ok((instance.ir_function, ir::Signature { parameters, result }))
        })
        .collect()
}

fn instruction_layout_type(
    program: &Program,
    instruction: &Instruction,
    specialization: &crate::vm::Specialization,
) -> Option<crate::vm::VerificationType> {
    use crate::vm::VerificationType;
    match instruction {
        Instruction::MakeRecord {
            record,
            type_arguments,
            ..
        } => Some(VerificationType::Record {
            record: *record,
            arguments: type_arguments
                .iter()
                .map(|ty| ty.specialize(specialization))
                .collect(),
        }),
        Instruction::MakeVariant {
            variant,
            type_arguments,
            ..
        } => Some(VerificationType::Variant {
            variant: program.variants[variant].parent,
            arguments: type_arguments
                .iter()
                .map(|ty| ty.specialize(specialization))
                .collect(),
        }),
        Instruction::MakeList { element_type, .. } => Some(VerificationType::List(Box::new(
            element_type.specialize(specialization),
        ))),
        Instruction::MakeReference { pointee_type, .. }
        | Instruction::MakeWholeReference { pointee_type, .. }
        | Instruction::MakeFieldReference { pointee_type, .. } => Some(
            VerificationType::Reference(Box::new(pointee_type.specialize(specialization))),
        ),
        _ => None,
    }
}

fn concrete_closure_result(
    program: &Program,
    layouts: &mut LayoutRegistry,
    instance: &SpecializationKey,
) -> Result<NativeType, FosterError> {
    let body = &program.functions[&instance.function];
    let mut result = None;
    for (index, instruction) in body.instructions.iter().enumerate() {
        let Instruction::Return { source } = instruction else {
            continue;
        };
        let key = closure_definition_before(body, index, *source, &instance.substitutions)
            .ok_or_else(|| {
                native_error(format!(
                    "native function `{}` returns an erased callable value",
                    body.name
                ))
                .with_help(
                    "return one statically known closure, or keep this explicitly dynamic call on the VM",
                )
            })?;
        if result.as_ref().is_some_and(|previous| previous != &key) {
            return Err(native_error(format!(
                "native function `{}` returns multiple concrete closure layouts",
                body.name
            ))
            .with_help("use the VM for a callable value selected dynamically"));
        }
        result = Some(key);
    }
    let key = result.ok_or_else(|| {
        native_error(format!(
            "native function `{}` has no concrete closure result",
            body.name
        ))
    })?;
    Ok(NativeType::Object(
        layouts.instantiate_closure(key.function, &key.substitutions)?,
    ))
}

fn closure_definition_before(
    function: &BytecodeFunction,
    before: usize,
    register: Register,
    outer: &crate::vm::Specialization,
) -> Option<SpecializationKey> {
    for (index, instruction) in function.instructions[..before].iter().enumerate().rev() {
        match instruction {
            Instruction::MakeClosure {
                destination,
                function,
                specialization,
                ..
            } if *destination == register => {
                return Some(SpecializationKey {
                    function: *function,
                    substitutions: resolve_specialization(specialization, outer),
                });
            }
            Instruction::Move {
                destination,
                source,
            } if *destination == register => {
                return closure_definition_before(function, index, *source, outer);
            }
            _ => {}
        }
    }
    None
}

fn native_type(
    compilation: &Compilation,
    layouts: &mut LayoutRegistry,
    ty: TypeId,
    substitutions: &crate::vm::Specialization,
    function: &str,
) -> Result<NativeType, FosterError> {
    if let Type::Generic(name) = &compilation.types.types[ty] {
        let concrete = substitutions
            .iter()
            .find_map(|(candidate, ty)| (candidate == name).then_some(ty))
            .ok_or_else(|| {
                native_error(format!(
                    "native specialization of `{function}` does not resolve generic `{name}`"
                ))
            })?;
        layouts.instantiate_type(concrete)?;
        return concrete_native_type(compilation, layouts, concrete, function);
    }
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
            && arguments.len() == 1 =>
        {
            let element =
                specialized_verification_type(compilation, arguments[0], substitutions, 0)?;
            let concrete = crate::vm::VerificationType::List(Box::new(element));
            layouts.instantiate_type(&concrete)?;
            layouts
                .builtin(&concrete)
                .map(NativeType::Object)
                .ok_or_else(|| native_error(format!("list type in `{function}` has no layout")))
        }
        Type::Record {
            record,
            ref arguments,
        } => {
            let arguments = arguments
                .iter()
                .map(|ty| specialized_verification_type(compilation, *ty, substitutions, 0))
                .collect::<Result<Vec<_>, _>>()?;
            let concrete = crate::vm::VerificationType::Record {
                record,
                arguments: arguments.clone(),
            };
            layouts.instantiate_type(&concrete)?;
            layouts
                .record_instance(record, &arguments)
                .map(NativeType::Object)
                .ok_or_else(|| native_error(format!("record type in `{function}` has no layout")))
        }
        Type::Variant {
            variant,
            ref arguments,
        } => {
            let arguments = arguments
                .iter()
                .map(|ty| specialized_verification_type(compilation, *ty, substitutions, 0))
                .collect::<Result<Vec<_>, _>>()?;
            let concrete = crate::vm::VerificationType::Variant {
                variant,
                arguments: arguments.clone(),
            };
            layouts.instantiate_type(&concrete)?;
            layouts
                .variant_instance(variant, &arguments)
                .map(NativeType::Object)
                .ok_or_else(|| native_error(format!("variant type in `{function}` has no layout")))
        }
        Type::RawBytes
        | Type::RawByteBuffer
        | Type::Reference { .. }
        | Type::RawList(_)
        | Type::Sequence(_)
        | Type::Remote(_)
        | Type::Future(_)
        | Type::Function(_)
        | Type::Intersection(_) => {
            let concrete = specialized_verification_type(compilation, ty, substitutions, 0)?;
            concrete_native_type(compilation, layouts, &concrete, function)
        }
        ref unsupported => Err(native_error(format!(
            "native compilation of `{function}` does not yet support type `{}` ({unsupported:?})",
            compilation.types.display(ty)
        ))
        .with_help("use `foster build` without `--native` for the complete VM language")),
    }
}

fn specialized_verification_type(
    compilation: &Compilation,
    ty: TypeId,
    substitutions: &crate::vm::Specialization,
    depth: usize,
) -> Result<crate::vm::VerificationType, FosterError> {
    use crate::vm::VerificationType;
    if depth >= 64 {
        return Err(native_error(
            "native specialization type nesting exceeds 64 levels",
        ));
    }
    let nested = |ty| specialized_verification_type(compilation, ty, substitutions, depth + 1);
    Ok(match &compilation.types.types[ty] {
        Type::Generic(name) => substitutions
            .iter()
            .find_map(|(candidate, ty)| (candidate == name).then(|| ty.clone()))
            .unwrap_or_else(|| VerificationType::Generic(name.clone())),
        Type::Unit => VerificationType::Unit,
        Type::Bool => VerificationType::Bool,
        Type::Int => VerificationType::Integer,
        Type::Float => VerificationType::Float,
        Type::CodePoint => VerificationType::CodePoint,
        Type::Byte => VerificationType::Byte,
        Type::RawBytes => VerificationType::Bytes,
        Type::RawByteBuffer => VerificationType::ByteBuffer,
        Type::Reference { value, .. } => VerificationType::Reference(Box::new(nested(*value)?)),
        Type::RawList(value) | Type::Sequence(value) => {
            VerificationType::List(Box::new(nested(*value)?))
        }
        Type::Remote(value) => VerificationType::Remote(Box::new(nested(*value)?)),
        Type::Future(value) => VerificationType::Future(Box::new(nested(*value)?)),
        Type::Function(function) => VerificationType::Function {
            parameters: function
                .parameters
                .iter()
                .map(|ty| nested(*ty))
                .collect::<Result<_, _>>()?,
            parameter_modes: function.parameter_modes.clone(),
            result: Box::new(nested(function.result)?),
        },
        Type::Record { record, arguments } => VerificationType::Record {
            record: *record,
            arguments: arguments
                .iter()
                .map(|ty| nested(*ty))
                .collect::<Result<_, _>>()?,
        },
        Type::Variant { variant, arguments } => VerificationType::Variant {
            variant: *variant,
            arguments: arguments
                .iter()
                .map(|ty| nested(*ty))
                .collect::<Result<_, _>>()?,
        },
        Type::Intersection(members) => VerificationType::Union(
            members
                .iter()
                .map(|ty| nested(*ty))
                .collect::<Result<_, _>>()?,
        ),
        Type::Module(_) => VerificationType::Unknown,
    })
}

fn concrete_native_type(
    compilation: &Compilation,
    layouts: &mut LayoutRegistry,
    ty: &crate::vm::VerificationType,
    function: &str,
) -> Result<NativeType, FosterError> {
    use crate::vm::VerificationType;
    match ty {
        VerificationType::Unit => Ok(NativeType::Unit),
        VerificationType::Bool => Ok(NativeType::Bool),
        VerificationType::Integer => Ok(NativeType::Int),
        VerificationType::Float => Ok(NativeType::Float),
        VerificationType::CodePoint => Ok(NativeType::CodePoint),
        VerificationType::Byte => Ok(NativeType::Byte),
        VerificationType::Record { record, .. }
            if compilation
                .types
                .record_names
                .get(record)
                .is_some_and(|name| name == "String") =>
        {
            Ok(NativeType::String)
        }
        VerificationType::Record { record, arguments } => {
            layouts.instantiate_type(ty)?;
            layouts
                .record_instance(*record, arguments)
                .map(NativeType::Object)
                .ok_or_else(|| native_error(format!("record type in `{function}` has no layout")))
        }
        VerificationType::Variant { variant, arguments } => {
            layouts.instantiate_type(ty)?;
            layouts
                .variant_instance(*variant, arguments)
                .map(NativeType::Object)
                .ok_or_else(|| native_error(format!("variant type in `{function}` has no layout")))
        }
        VerificationType::Reference(pointee) => {
            layouts.instantiate_type(ty)?;
            layouts
                .pointer(pointee, crate::codegen::layout::Ownership::Borrowed)
                .map(NativeType::Object)
                .ok_or_else(|| {
                    native_error(format!("reference type in `{function}` has no layout"))
                })
        }
        VerificationType::Bytes
        | VerificationType::ByteBuffer
        | VerificationType::List(_)
        | VerificationType::Remote(_)
        | VerificationType::Future(_)
        | VerificationType::Function { .. } => {
            layouts.instantiate_type(ty)?;
            layouts
                .builtin(ty)
                .map(NativeType::Object)
                .ok_or_else(|| native_error(format!("runtime type in `{function}` has no layout")))
        }
        VerificationType::Unknown | VerificationType::Union(_) => {
            Ok(NativeType::Object(layouts.opaque()))
        }
        unsupported => Err(native_error(format!(
            "native specialization of `{function}` does not yet support `{unsupported:?}`"
        ))),
    }
}

fn validate_program(
    compilation: &Compilation,
    program: &Program,
    instances: &[NativeInstance],
    function_types: &HashMap<FunctionId, ir::Signature>,
    layouts: &LayoutRegistry,
) -> Result<(), FosterError> {
    let main = program.main.expect("validated above");
    let main_function = &program.functions[&main];
    if main_function.parameters != u16::from(program.main_arguments) || main_function.captures != 0
    {
        return Err(native_error(
            "native `main` must take no parameters or one `std.process.Arguments` parameter",
        ));
    }
    for instance in instances {
        let body = &program.functions[&instance.key.function];
        if usize::from(body.captures) + usize::from(body.parameters)
            != function_types[&instance.ir_function].parameters.len()
        {
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
                    | Instruction::CallClosure { .. }
                    | Instruction::MakeClosure { .. }
                    | Instruction::CallValue { .. }
                    | Instruction::MakeRecord { .. }
                    | Instruction::MakeVariant { .. }
                    | Instruction::MakeList { .. }
                    | Instruction::LoadField { .. }
                    | Instruction::StoreField { .. }
                    | Instruction::StoreIndex { .. }
                    | Instruction::MatchPattern { .. }
                    | Instruction::Index { .. }
                    | Instruction::MakeReference { .. }
                    | Instruction::MakeWholeReference { .. }
                    | Instruction::MakeFieldReference { .. }
                    | Instruction::MoveOut { .. }
                    | Instruction::Push { .. }
                    | Instruction::Append { .. }
                    | Instruction::Contains { .. }
                    | Instruction::Builtin { .. }
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
            if let Instruction::Builtin { builtin, .. } = instruction
                && builtin.descriptor().native == crate::intrinsics::NativeIntrinsic::Unavailable
            {
                return Err(native_error(format!(
                    "native compilation of `{}` has no lowering for intrinsic `{builtin:?}`",
                    body.name
                ))
                .with_help("move the algorithm into Foster or register a typed native primitive"));
            }
            if let Instruction::LoadConstant { constant, .. } = instruction
                && matches!(
                    program.constants[usize::from(*constant)],
                    Constant::Symbol(_)
                )
            {
                return Err(native_error(format!(
                    "native compilation of `{}` does not yet support type `Symbol` constants",
                    body.name
                ))
                .with_help("use `foster build` without `--native` for the complete VM language"));
            }
        }
    }
    let _ = compilation;
    Ok(())
}

fn declare_layout_destructors(
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

fn define_layout_destructors(
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
                DropPlan::Trivial | DropPlan::Runtime => {}
            }
            let size = builder.ins().iconst(types::I64, i64::from(layout.size));
            let align = builder.ins().iconst(types::I64, i64::from(layout.align));
            runtime_call(
                &mut builder,
                module,
                "foster_dealloc",
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

fn lower_drop_buffer(
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
        "foster_dealloc",
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

fn lower_drop_fields(
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

fn define_function(
    module: &mut ObjectModule,
    instance: &NativeInstance,
    native_id: FuncId,
    backend: &NativeBackend<'_>,
) -> Result<(), FosterError> {
    let function = &backend.ir.program.functions[&instance.key.function];
    let native_function = lower_shared_to_native_ir(
        &backend.ir.shared_functions[&instance.key.function],
        function,
        &backend.ir.function_types[&instance.ir_function],
        &instance.key,
        backend.ir,
    )?;
    native_function
        .verify(backend.ir.function_types)
        .map_err(|error| {
            native_error(format!(
                "invalid native IR for `{}`: {error}",
                function.name
            ))
        })?;
    let frontend_config = module.target_config();
    let mut context = module.make_context();
    context.func.signature = signature(module, &backend.ir.function_types[&instance.ir_function]);
    let mut builder_context = FunctionBuilderContext::new();
    {
        let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
        lower_native_ir(&mut builder, module, &native_function, backend)?;
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

fn cranelift_type(ty: NativeType, pointer_type: ClifType) -> ClifType {
    match ty.representation() {
        ir::Representation::I8 => types::I8,
        ir::Representation::I32 => types::I32,
        ir::Representation::I64 => types::I64,
        ir::Representation::F64 => types::F64,
        ir::Representation::Pointer => pointer_type,
    }
}

fn infer_register_types(
    function: &BytecodeFunction,
    parameter_types: &[NativeType],
    instance: &SpecializationKey,
    environment: NativeIrEnvironment<'_>,
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
                result[usize::from(destination.0)] = Some(
                    match environment.program.constants[usize::from(*constant)] {
                        Constant::Unit => NativeType::Unit,
                        Constant::Bool(_) => NativeType::Bool,
                        Constant::Integer(_) => NativeType::Int,
                        Constant::Float(_) => NativeType::Float,
                        Constant::CodePoint(_) => NativeType::CodePoint,
                        Constant::String(_) => NativeType::String,
                        Constant::Symbol(_) => unreachable!("validated above"),
                    },
                );
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
                    _ => dereference_native_type(
                        register_type(&result, *left, function)?,
                        environment,
                    )?,
                });
            }
            Instruction::Call {
                destination,
                function: callee,
                specialization,
                ..
            }
            | Instruction::CallMethod {
                destination,
                function: callee,
                specialization,
                ..
            }
            | Instruction::CallClosure {
                destination,
                function: callee,
                specialization,
                ..
            } => {
                let callee = environment.instances[&SpecializationKey {
                    function: *callee,
                    substitutions: resolve_specialization(specialization, &instance.substitutions),
                }];
                result[usize::from(destination.0)] =
                    Some(environment.function_types[&callee].result);
            }
            Instruction::MakeClosure {
                destination,
                function: target,
                specialization,
                ..
            } => {
                let specialization =
                    resolve_specialization(specialization, &instance.substitutions);
                result[usize::from(destination.0)] = Some(NativeType::Object(
                    environment
                        .layouts
                        .closure_instance(*target, &specialization)
                        .ok_or_else(|| {
                            native_error(format!(
                                "closure in `{}` has no native layout",
                                function.name
                            ))
                        })?,
                ));
            }
            Instruction::CallValue {
                destination,
                callee,
                ..
            } => {
                let NativeType::Object(layout) = register_type(&result, *callee, function)? else {
                    return Err(native_error(format!(
                        "dynamic call in `{}` has an erased callable representation",
                        function.name
                    )));
                };
                let LayoutKind::Closure {
                    function: target,
                    specialization,
                    ..
                } = &environment.layouts.get(layout).kind
                else {
                    return Err(native_error(format!(
                        "dynamic call in `{}` does not reference a concrete closure",
                        function.name
                    )));
                };
                let target = environment.instances[&SpecializationKey {
                    function: *target,
                    substitutions: specialization.clone(),
                }];
                result[usize::from(destination.0)] =
                    Some(environment.function_types[&target].result);
            }
            Instruction::MakeRecord {
                destination,
                record,
                type_arguments,
                ..
            } => {
                let arguments = type_arguments
                    .iter()
                    .map(|ty| ty.specialize(&instance.substitutions))
                    .collect::<Vec<_>>();
                result[usize::from(destination.0)] = Some(NativeType::Object(
                    environment
                        .layouts
                        .record_instance(*record, &arguments)
                        .ok_or_else(|| {
                            native_error(format!(
                                "record in `{}` has no native layout",
                                function.name
                            ))
                        })?,
                ));
            }
            Instruction::MakeVariant {
                destination,
                variant,
                type_arguments,
                ..
            } => {
                let parent = environment.program.variants[variant].parent;
                let arguments = type_arguments
                    .iter()
                    .map(|ty| ty.specialize(&instance.substitutions))
                    .collect::<Vec<_>>();
                result[usize::from(destination.0)] = Some(NativeType::Object(
                    environment
                        .layouts
                        .variant_instance(parent, &arguments)
                        .ok_or_else(|| {
                            native_error(format!(
                                "variant in `{}` has no native layout",
                                function.name
                            ))
                        })?,
                ));
            }
            Instruction::MakeList {
                destination,
                element_type,
                ..
            } => {
                let concrete = crate::vm::VerificationType::List(Box::new(
                    element_type.specialize(&instance.substitutions),
                ));
                let layout = environment.layouts.builtin(&concrete).ok_or_else(|| {
                    native_error(format!(
                        "list in `{}` has no concrete native layout for `{concrete:?}`",
                        function.name
                    ))
                })?;
                result[usize::from(destination.0)] = Some(NativeType::Object(layout));
            }
            Instruction::LoadField {
                destination,
                object,
                field,
                ..
            } => {
                let object = register_type(&result, *object, function)?;
                result[usize::from(destination.0)] = Some(field_type(
                    environment.program,
                    environment.layouts,
                    environment.physical_layouts,
                    object,
                    field,
                )?);
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
                    NativeType::Object(layout) => {
                        let PhysicalKind::Buffer { element, .. } =
                            &environment.physical_layouts.get(layout).kind
                        else {
                            return Err(native_error(format!(
                                "native indexing requires a buffer in `{}`",
                                function.name
                            )));
                        };
                        native_type_from_value_layout(*element)
                    }
                    _ => {
                        return Err(native_error(format!(
                            "native indexing does not support `{object:?}` in `{}`",
                            function.name
                        )));
                    }
                });
            }
            Instruction::MakeReference {
                destination,
                pointee_type,
                ..
            }
            | Instruction::MakeWholeReference {
                destination,
                pointee_type,
                ..
            }
            | Instruction::MakeFieldReference {
                destination,
                pointee_type,
                ..
            } => {
                let pointee = pointee_type.specialize(&instance.substitutions);
                let layout = environment
                    .layouts
                    .pointer(&pointee, crate::codegen::layout::Ownership::Borrowed)
                    .ok_or_else(|| native_error("reference has no concrete native layout"))?;
                result[usize::from(destination.0)] = Some(NativeType::Object(layout));
            }
            Instruction::MoveOut {
                destination,
                source,
            } => {
                let source_type = register_type(&result, *source, function)?;
                result[usize::from(destination.0)] =
                    Some(dereference_native_type(source_type, environment)?);
            }
            Instruction::Push { destination, .. } => {
                result[usize::from(destination.0)] = Some(NativeType::Unit);
            }
            Instruction::Append {
                destination,
                object,
                ..
            } => {
                result[usize::from(destination.0)] =
                    Some(register_type(&result, *object, function)?);
            }
            Instruction::Contains { destination, .. } => {
                result[usize::from(destination.0)] = Some(NativeType::Bool);
            }
            Instruction::Builtin {
                destination,
                builtin,
                ..
            } => {
                result[usize::from(destination.0)] = Some(native_intrinsic_type(
                    builtin.descriptor().signature.result,
                    environment.layouts,
                )?);
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
                result[usize::from(destination.0)] = Some(field_type(
                    environment.program,
                    environment.layouts,
                    environment.physical_layouts,
                    receiver,
                    name,
                )?);
            }
            Instruction::MatchPattern {
                destination,
                subject,
                pattern,
                bindings,
            } => {
                result[usize::from(destination.0)] = Some(NativeType::Bool);
                let subject = register_type(&result, *subject, function)?;
                let mut types = Vec::new();
                native_pattern_binding_types(
                    environment.program,
                    environment.layouts,
                    environment.physical_layouts,
                    pattern,
                    subject,
                    &mut types,
                )?;
                if types.len() != bindings.len() {
                    return Err(native_error(format!(
                        "native pattern in `{}` has inconsistent binding metadata",
                        function.name
                    )));
                }
                for (binding, ty) in bindings.iter().zip(types) {
                    result[usize::from(binding.0)] = Some(ty);
                }
            }
            _ => {}
        }
    }
    Ok(result)
}

fn native_pattern_binding_types(
    program: &Program,
    layouts: &LayoutRegistry,
    physical_layouts: &PhysicalRegistry,
    pattern: &Pattern,
    subject: NativeType,
    bindings: &mut Vec<NativeType>,
) -> Result<(), FosterError> {
    match pattern.unspanned() {
        Pattern::Binding(_) => bindings.push(subject),
        Pattern::Variant { variant, fields } => {
            let parent = program.variants[variant].parent;
            let NativeType::Object(layout) = subject else {
                return Err(native_error("pattern subject uses the wrong native layout"));
            };
            let LayoutKind::Variant {
                variant_type,
                alternatives,
            } = &layouts.get(layout).kind
            else {
                unreachable!()
            };
            if *variant_type != parent {
                return Err(native_error(
                    "pattern subject uses the wrong nominal layout",
                ));
            }
            let alternative = alternatives
                .iter()
                .find(|alternative| alternative.variant == *variant)
                .ok_or_else(|| native_error("pattern alternative has no logical layout"))?;
            let physical = physical_layouts
                .variant_alternative(layout, alternative.tag)
                .ok_or_else(|| native_error("pattern alternative has no physical layout"))?;
            for ((pattern, ty), field) in fields
                .iter()
                .zip(&alternative.payload)
                .zip(&physical.fields)
            {
                let ty = native_verification_type(program, layouts, ty, field.value.pointee)?;
                native_pattern_binding_types(
                    program,
                    layouts,
                    physical_layouts,
                    pattern,
                    ty,
                    bindings,
                )?;
            }
        }
        Pattern::Spanned { .. } => unreachable!(),
        Pattern::Wildcard
        | Pattern::Bool(_)
        | Pattern::Integer(_)
        | Pattern::Float(_)
        | Pattern::String(_)
        | Pattern::CodePoint(_)
        | Pattern::Symbol(_) => {}
    }
    Ok(())
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

fn dereference_native_type(
    ty: NativeType,
    environment: NativeIrEnvironment<'_>,
) -> Result<NativeType, FosterError> {
    let NativeType::Object(layout) = ty else {
        return Ok(ty);
    };
    let LayoutKind::Pointer { pointee, .. } = &environment.layouts.get(layout).kind else {
        return Ok(ty);
    };
    native_verification_type(environment.program, environment.layouts, pointee, None)
}

fn field_type(
    program: &Program,
    layouts: &LayoutRegistry,
    physical_layouts: &PhysicalRegistry,
    receiver: NativeType,
    field: &str,
) -> Result<NativeType, FosterError> {
    match (receiver, field) {
        (NativeType::Arguments, "executable") => Ok(NativeType::String),
        (NativeType::Arguments, "values") => Ok(NativeType::StringList),
        (NativeType::StringList, "empty?") | (NativeType::String, "empty?") => Ok(NativeType::Bool),
        (NativeType::StringList, "length") | (NativeType::String, "length") => Ok(NativeType::Int),
        (NativeType::StringList, "head") => Ok(NativeType::String),
        (NativeType::String, "head") => Ok(NativeType::CodePoint),
        (NativeType::Object(layout), field) => match &layouts.get(layout).kind {
            LayoutKind::Record { fields, .. } => {
                let slot = fields
                    .iter()
                    .find(|slot| slot.name == field)
                    .ok_or_else(|| {
                        native_error(format!(
                            "native record l{} has no field `{field}`",
                            layout.0
                        ))
                    })?;
                let physical = physical_layouts
                    .record_field(layout, slot.index)
                    .ok_or_else(|| native_error("logical and physical record fields disagree"))?;
                native_verification_type(program, layouts, &slot.ty, physical.value.pointee)
            }
            LayoutKind::Builtin {
                ty: crate::vm::VerificationType::List(element),
            } => match field {
                "empty?" => Ok(NativeType::Bool),
                "length" => Ok(NativeType::Int),
                "head" => native_verification_type(
                    program,
                    layouts,
                    element,
                    match physical_layouts.get(layout).kind {
                        PhysicalKind::Buffer { element, .. } => element.pointee,
                        _ => None,
                    },
                ),
                "rest" => Ok(NativeType::Object(layout)),
                _ => Err(native_error(format!("native list has no field `{field}`"))),
            },
            LayoutKind::Pointer { pointee, .. } => field_type(
                program,
                layouts,
                physical_layouts,
                native_verification_type(program, layouts, pointee, None)?,
                field,
            ),
            _ => Err(native_error(format!(
                "native field access requires a record or list, found l{}",
                layout.0
            ))),
        },
        _ => Err(native_error(format!(
            "native compilation does not support field `{field}` on `{receiver:?}`"
        ))),
    }
}

fn native_verification_type(
    program: &Program,
    layouts: &LayoutRegistry,
    ty: &crate::vm::VerificationType,
    physical_pointee: Option<LayoutId>,
) -> Result<NativeType, FosterError> {
    use crate::vm::VerificationType;
    match ty {
        VerificationType::Unit => Ok(NativeType::Unit),
        VerificationType::Bool => Ok(NativeType::Bool),
        VerificationType::Integer => Ok(NativeType::Int),
        VerificationType::Float => Ok(NativeType::Float),
        VerificationType::CodePoint => Ok(NativeType::CodePoint),
        VerificationType::Byte => Ok(NativeType::Byte),
        VerificationType::Record { record, .. } if Some(*record) == program.string_record => {
            Ok(NativeType::String)
        }
        VerificationType::Record { record, arguments } => layouts
            .record_instance(*record, arguments)
            .or(physical_pointee)
            .map(NativeType::Object)
            .ok_or_else(|| native_error("record field has no native layout")),
        VerificationType::Variant { variant, arguments } => layouts
            .variant_instance(*variant, arguments)
            .or(physical_pointee)
            .map(NativeType::Object)
            .ok_or_else(|| native_error("variant field has no native layout")),
        VerificationType::List(_)
        | VerificationType::Bytes
        | VerificationType::ByteBuffer
        | VerificationType::Remote(_)
        | VerificationType::Future(_)
        | VerificationType::Function { .. } => layouts
            .builtin(ty)
            .or(physical_pointee)
            .map(NativeType::Object)
            .ok_or_else(|| native_error("builtin value has no native layout")),
        VerificationType::Reference(pointee) => layouts
            .pointer(pointee, crate::codegen::layout::Ownership::Borrowed)
            .or(physical_pointee)
            .map(NativeType::Object)
            .ok_or_else(|| native_error("reference has no native layout")),
        VerificationType::Unknown | VerificationType::Union(_) => {
            Ok(NativeType::Object(layouts.opaque()))
        }
        VerificationType::Generic(name) => Err(native_error(format!(
            "unresolved generic `{name}` has no native representation"
        ))),
    }
}

fn native_intrinsic_type(
    ty: crate::intrinsics::IntrinsicType,
    layouts: &LayoutRegistry,
) -> Result<NativeType, FosterError> {
    use crate::intrinsics::IntrinsicType;
    use crate::vm::VerificationType;
    match ty {
        IntrinsicType::Unit => Ok(NativeType::Unit),
        IntrinsicType::Bool => Ok(NativeType::Bool),
        IntrinsicType::Integer => Ok(NativeType::Int),
        IntrinsicType::Float => Ok(NativeType::Float),
        IntrinsicType::CodePoint => Ok(NativeType::CodePoint),
        IntrinsicType::Byte => Ok(NativeType::Byte),
        IntrinsicType::String => Ok(NativeType::String),
        IntrinsicType::Bytes => layouts
            .builtin(&VerificationType::Bytes)
            .map(NativeType::Object)
            .ok_or_else(|| native_error("Bytes intrinsic type has no native layout")),
        IntrinsicType::ByteBuffer => layouts
            .builtin(&VerificationType::ByteBuffer)
            .map(NativeType::Object)
            .ok_or_else(|| native_error("ByteBuffer intrinsic type has no native layout")),
        IntrinsicType::ListByte => layouts
            .builtin(&VerificationType::List(Box::new(VerificationType::Byte)))
            .map(NativeType::Object)
            .ok_or_else(|| native_error("List<Byte> intrinsic type has no native layout")),
        IntrinsicType::Any => Err(native_error(
            "erased intrinsic type does not define a native ABI",
        )),
    }
}

fn lower_shared_to_native_ir(
    shared: &ir::Function,
    metadata: &BytecodeFunction,
    function_signature: &ir::Signature,
    instance: &SpecializationKey,
    environment: NativeIrEnvironment<'_>,
) -> Result<ir::Function, FosterError> {
    let external_values = shared.captures.iter().chain(&shared.parameters);
    let external_types = metadata
        .capture_types
        .iter()
        .chain(&metadata.parameter_types);
    let reference_homes = external_values
        .zip(external_types)
        .filter_map(|(value, ty)| {
            matches!(ty, crate::vm::VerificationType::Reference(_))
                .then(|| shared.storage_hints[value.0 as usize].map(|home| (home, *value)))
                .flatten()
        })
        .collect::<HashMap<_, _>>();
    let inferred = infer_register_types(
        metadata,
        &function_signature.parameters,
        instance,
        environment,
    )?;
    let mut value_types = shared
        .storage_hints
        .iter()
        .enumerate()
        .map(|(index, register)| {
            register
                .and_then(|register| inferred[usize::from(register)])
                .or_else(|| {
                    shared
                        .value_types
                        .get(index)
                        .copied()
                        .map(native_shared_type)
                })
                .unwrap_or(NativeType::Unit)
        })
        .collect::<Vec<_>>();
    for (home, value) in &reference_homes {
        let Some(crate::vm::VerificationType::Reference(pointee)) = metadata
            .capture_types
            .iter()
            .chain(&metadata.parameter_types)
            .nth(usize::from(*home))
        else {
            continue;
        };
        let pointee = pointee.specialize(&instance.substitutions);
        let layout = environment
            .layouts
            .pointer(&pointee, crate::codegen::layout::Ownership::Borrowed)
            .ok_or_else(|| native_error("captured reference has no native layout"))?;
        value_types[value.0 as usize] = NativeType::Object(layout);
    }
    let mut storage_hints = shared.storage_hints.clone();
    let mut blocks = Vec::with_capacity(shared.blocks.len());

    for block in &shared.blocks {
        let mut state = HashMap::<u16, ir::Value>::new();
        for value in &block.parameters {
            if let Some(home) = shared.storage_hints[value.0 as usize] {
                state.insert(home, *value);
            }
        }
        let mut instructions = Vec::new();
        let mut spans = Vec::new();
        for (instruction, span) in block.instructions.iter().zip(&block.instruction_spans) {
            let lowered = lower_shared_instruction(
                instruction,
                metadata,
                instance,
                environment,
                &mut value_types,
                &mut storage_hints,
                &reference_homes,
            )?;
            for (instruction, consumed) in lowered {
                if let ir::Instruction::Portable(ir::PortableInstruction::Drop { value }) =
                    &instruction
                    && !remove_shared_home(&mut state, &storage_hints, *value)
                {
                    continue;
                }
                for value in consumed {
                    remove_shared_home(&mut state, &storage_hints, value);
                }
                for destination in instruction.destinations() {
                    if let Some(home) = storage_hints[destination.0 as usize] {
                        state.insert(home, destination);
                    }
                }
                instructions.push(instruction);
                spans.push(span.clone());
            }
        }
        if let ir::Terminator::Return(returned) = block.terminator {
            let mut released = HashSet::new();
            for value in state.values().copied() {
                if value != returned
                    && released.insert(value)
                    && matches!(value_types[value.0 as usize], NativeType::Object(_))
                {
                    instructions.push(ir::Instruction::Portable(ir::PortableInstruction::Drop {
                        value,
                    }));
                    spans.push(block.terminator_span.clone());
                }
            }
        }
        blocks.push(ir::BlockData {
            parameters: block.parameters.clone(),
            instructions,
            instruction_spans: spans,
            terminator: block.terminator.clone(),
            terminator_span: block.terminator_span.clone(),
        });
    }

    let mut parameters = shared.captures.clone();
    parameters.extend(&shared.parameters);
    let mut entry = shared.entry;
    let mut entry_arguments = shared.entry_arguments.clone();
    if !reference_homes.is_empty() {
        let mut loaded_captures = HashMap::new();
        let mut prologue_instructions = Vec::new();
        let mut prologue_spans = Vec::new();
        for (input, input_type) in shared.captures.iter().chain(&shared.parameters).zip(
            metadata
                .capture_types
                .iter()
                .chain(&metadata.parameter_types),
        ) {
            let crate::vm::VerificationType::Reference(pointee) = input_type else {
                continue;
            };
            let concrete_pointee = pointee.specialize(&instance.substitutions);
            let reference_type = NativeType::Object(
                environment
                    .layouts
                    .pointer(
                        &concrete_pointee,
                        crate::codegen::layout::Ownership::Borrowed,
                    )
                    .ok_or_else(|| native_error("captured reference has no native layout"))?,
            );
            value_types[input.0 as usize] = reference_type;
            let loaded_type =
                native_verification_type(environment.program, environment.layouts, pointee, None)?;
            let loaded = allocate_shared_value(&mut value_types, &mut storage_hints, loaded_type);
            prologue_instructions.push(ir::Instruction::RuntimeCall {
                destination: loaded,
                helper: reference_load_helper(loaded_type),
                signature: ir::Signature {
                    parameters: vec![reference_type],
                    result: loaded_type,
                },
                arguments: vec![*input],
            });
            prologue_spans.push(Range::default());
            loaded_captures.insert(*input, loaded);
        }
        for block in &mut blocks {
            shift_native_blocks(&mut block.terminator, 1);
        }
        let arguments = shared
            .entry_arguments
            .iter()
            .map(|value| loaded_captures.get(value).copied().unwrap_or(*value))
            .collect();
        blocks.insert(
            0,
            ir::BlockData {
                parameters: Vec::new(),
                instructions: prologue_instructions,
                instruction_spans: prologue_spans,
                terminator: ir::Terminator::Jump {
                    target: ir::Block(shared.entry.0 + 1),
                    arguments,
                },
                terminator_span: Range::default(),
            },
        );
        entry = ir::Block(0);
        entry_arguments = Vec::new();
    }
    Ok(ir::Function {
        name: shared.name.clone(),
        signature: function_signature.clone(),
        parameters,
        captures: Vec::new(),
        capture_types: Vec::new(),
        entry_seeds: shared.entry_seeds.clone(),
        entry,
        entry_arguments,
        value_types,
        storage_hints,
        blocks,
    })
}

fn shift_native_blocks(terminator: &mut ir::Terminator, offset: u32) {
    match terminator {
        ir::Terminator::Jump { target, .. } => target.0 += offset,
        ir::Terminator::Branch {
            then_target,
            else_target,
            ..
        } => {
            then_target.0 += offset;
            else_target.0 += offset;
        }
        ir::Terminator::Return(_) => {}
    }
}

fn native_shared_type(ty: ir::Type) -> NativeType {
    match ty {
        ir::Type::Opaque => NativeType::Opaque,
        ir::Type::Unit => NativeType::Unit,
        ir::Type::Bool => NativeType::Bool,
        ir::Type::Int => NativeType::Int,
        ir::Type::Float => NativeType::Float,
        ir::Type::CodePoint => NativeType::CodePoint,
        ir::Type::Byte => NativeType::Byte,
        ir::Type::String => NativeType::String,
        ir::Type::Arguments => NativeType::Arguments,
        ir::Type::StringList => NativeType::StringList,
        ir::Type::Object(layout) => NativeType::Object(layout),
    }
}

fn remove_shared_home(
    state: &mut HashMap<u16, ir::Value>,
    storage_hints: &[Option<u16>],
    value: ir::Value,
) -> bool {
    let Some(home) = storage_hints[value.0 as usize] else {
        return true;
    };
    if state.get(&home) == Some(&value) {
        state.remove(&home);
        true
    } else {
        false
    }
}

fn allocate_shared_value(
    value_types: &mut Vec<NativeType>,
    storage_hints: &mut Vec<Option<u16>>,
    ty: NativeType,
) -> ir::Value {
    let value = ir::Value(value_types.len() as u32);
    value_types.push(ty);
    storage_hints.push(None);
    value
}

fn lower_shared_instruction(
    instruction: &ir::Instruction,
    metadata: &BytecodeFunction,
    instance: &SpecializationKey,
    environment: NativeIrEnvironment<'_>,
    value_types: &mut Vec<NativeType>,
    storage_hints: &mut Vec<Option<u16>>,
    reference_homes: &HashMap<u16, ir::Value>,
) -> Result<Vec<(ir::Instruction, Vec<ir::Value>)>, FosterError> {
    let ty = |value: ir::Value| value_types[value.0 as usize];
    let one = |instruction| vec![(instruction, Vec::new())];
    let ir::Instruction::Portable(portable) = instruction else {
        return Ok(one(instruction.clone()));
    };
    match portable {
        ir::PortableInstruction::LoadConstant {
            destination,
            constant,
        } => {
            let value = match environment.program.constants[usize::from(*constant)] {
                Constant::Unit => ir::Constant::Unit,
                Constant::Bool(value) => ir::Constant::Bool(value),
                Constant::Integer(value) => ir::Constant::Integer(value),
                Constant::Float(value) => ir::Constant::Float(value),
                Constant::CodePoint(value) => ir::Constant::CodePoint(value),
                Constant::String(_) => {
                    ir::Constant::RuntimeString(environment.runtime_string_indices[constant])
                }
                Constant::Symbol(_) => {
                    return Err(native_error(format!(
                        "native compilation of `{}` does not yet support symbol constants",
                        metadata.name
                    )));
                }
            };
            Ok(one(ir::Instruction::Constant {
                destination: *destination,
                value,
            }))
        }
        ir::PortableInstruction::Move {
            destination,
            source,
        } => {
            let Some(reference) = storage_hints[destination.0 as usize]
                .and_then(|home| reference_homes.get(&home).copied())
            else {
                return Ok(one(instruction.clone()));
            };
            let stored_type = ty(*source);
            let reference_type = ty(reference);
            let stored = allocate_shared_value(value_types, storage_hints, NativeType::Unit);
            Ok(vec![
                (
                    ir::Instruction::RuntimeCall {
                        destination: stored,
                        helper: reference_store_helper(stored_type),
                        signature: ir::Signature {
                            parameters: vec![reference_type, stored_type],
                            result: NativeType::Unit,
                        },
                        arguments: vec![reference, *source],
                    },
                    Vec::new(),
                ),
                (instruction.clone(), Vec::new()),
            ])
        }
        ir::PortableInstruction::Unary {
            destination,
            operator,
            operand,
        } => Ok(one(ir::Instruction::Unary {
            destination: *destination,
            operator: *operator,
            operand: *operand,
        })),
        ir::PortableInstruction::Binary {
            destination,
            operator,
            left,
            right,
        } => {
            let mut result = Vec::new();
            let mut left = *left;
            let mut right = *right;
            for operand in [&mut left, &mut right] {
                let operand_type = value_types[operand.0 as usize];
                if let NativeType::Object(layout) = operand_type
                    && let LayoutKind::Pointer { pointee, .. } =
                        &environment.layouts.get(layout).kind
                {
                    let loaded_type = native_verification_type(
                        environment.program,
                        environment.layouts,
                        pointee,
                        None,
                    )?;
                    let loaded = allocate_shared_value(value_types, storage_hints, loaded_type);
                    result.push((
                        ir::Instruction::RuntimeCall {
                            destination: loaded,
                            helper: reference_load_helper(loaded_type),
                            signature: ir::Signature {
                                parameters: vec![NativeType::Object(layout)],
                                result: loaded_type,
                            },
                            arguments: vec![*operand],
                        },
                        Vec::new(),
                    ));
                    *operand = loaded;
                }
            }
            if value_types[left.0 as usize] == NativeType::Int
                && matches!(
                    value_types[right.0 as usize],
                    NativeType::Byte | NativeType::CodePoint
                )
            {
                let extended = allocate_shared_value(value_types, storage_hints, NativeType::Int);
                result.push((
                    ir::Instruction::IntegerExtend {
                        destination: extended,
                        operand: right,
                    },
                    Vec::new(),
                ));
                right = extended;
            } else if value_types[right.0 as usize] == NativeType::Int
                && matches!(
                    value_types[left.0 as usize],
                    NativeType::Byte | NativeType::CodePoint
                )
            {
                let extended = allocate_shared_value(value_types, storage_hints, NativeType::Int);
                result.push((
                    ir::Instruction::IntegerExtend {
                        destination: extended,
                        operand: left,
                    },
                    Vec::new(),
                ));
                left = extended;
            }
            result.push((
                ir::Instruction::Binary {
                    destination: *destination,
                    operator: *operator,
                    left,
                    right,
                },
                Vec::new(),
            ));
            Ok(result)
        }
        ir::PortableInstruction::Call {
            destination,
            function,
            specialization,
            arguments,
        } => {
            let mut result = Vec::new();
            let (arguments, consumed) = shared_call_arguments(
                arguments,
                &environment.program.functions[function].parameter_modes,
                value_types,
                storage_hints,
                &mut result,
            )?;
            Ok({
                result.push((
                    ir::Instruction::Call {
                        destination: *destination,
                        function: environment.instances[&SpecializationKey {
                            function: *function,
                            substitutions: resolve_specialization(
                                specialization,
                                &instance.substitutions,
                            ),
                        }],
                        specialization: Vec::new(),
                        arguments,
                    },
                    consumed,
                ));
                result
            })
        }
        ir::PortableInstruction::CallMethod {
            destination,
            receiver,
            function,
            specialization,
            arguments,
        } => {
            let mut sources = vec![*receiver];
            sources.extend(arguments);
            let mut result = Vec::new();
            let (arguments, consumed) = shared_call_arguments(
                &sources,
                &environment.program.functions[function].parameter_modes,
                value_types,
                storage_hints,
                &mut result,
            )?;
            result.push((
                ir::Instruction::Call {
                    destination: *destination,
                    function: environment.instances[&SpecializationKey {
                        function: *function,
                        substitutions: resolve_specialization(
                            specialization,
                            &instance.substitutions,
                        ),
                    }],
                    specialization: Vec::new(),
                    arguments,
                },
                consumed,
            ));
            Ok(result)
        }
        ir::PortableInstruction::CallClosure {
            destination,
            function,
            specialization,
            captures,
            arguments,
        } => {
            let mut result = Vec::new();
            let target = environment.instances[&SpecializationKey {
                function: *function,
                substitutions: resolve_specialization(specialization, &instance.substitutions),
            }];
            let expected_captures =
                &environment.function_types[&target].parameters[..captures.len()];
            let (mut lowered, mut consumed) = shared_capture_arguments(
                captures,
                expected_captures,
                value_types,
                storage_hints,
                &mut result,
                &metadata.name,
            )?;
            let (ordinary, ordinary_consumed) = shared_call_arguments(
                arguments,
                &environment.program.functions[function].parameter_modes,
                value_types,
                storage_hints,
                &mut result,
            )?;
            lowered.extend(ordinary);
            consumed.extend(ordinary_consumed);
            result.push((
                ir::Instruction::Call {
                    destination: *destination,
                    function: target,
                    specialization: Vec::new(),
                    arguments: lowered,
                },
                consumed,
            ));
            Ok(result)
        }
        ir::PortableInstruction::MakeClosure {
            destination,
            function,
            specialization,
            captures,
        } => {
            let mut result = Vec::new();
            let specialization = resolve_specialization(specialization, &instance.substitutions);
            let target = environment.instances[&SpecializationKey {
                function: *function,
                substitutions: specialization.clone(),
            }];
            let expected_captures =
                &environment.function_types[&target].parameters[..captures.len()];
            let (captures, consumed) = shared_capture_arguments(
                captures,
                expected_captures,
                value_types,
                storage_hints,
                &mut result,
                &metadata.name,
            )?;
            result.push((
                ir::Instruction::Portable(ir::PortableInstruction::MakeClosure {
                    destination: *destination,
                    function: target,
                    specialization: Vec::new(),
                    captures: captures
                        .into_iter()
                        .map(|value| (crate::hir::CaptureMode::Move, value))
                        .collect(),
                }),
                consumed,
            ));
            Ok(result)
        }
        ir::PortableInstruction::CallValue {
            destination,
            callee,
            arguments,
        } => {
            let NativeType::Object(layout) = ty(*callee) else {
                return Err(native_error(format!(
                    "dynamic call in `{}` crosses an erased callable boundary",
                    metadata.name
                )));
            };
            let LayoutKind::Closure {
                function: target, ..
            } = &environment.layouts.get(layout).kind
            else {
                return Err(native_error(format!(
                    "dynamic call in `{}` requires a concrete closure",
                    metadata.name
                )));
            };
            let mut result = Vec::new();
            let (arguments, consumed) = shared_call_arguments(
                arguments,
                &environment.program.functions[target].parameter_modes,
                value_types,
                storage_hints,
                &mut result,
            )?;
            result.push((
                ir::Instruction::Portable(ir::PortableInstruction::CallValue {
                    destination: *destination,
                    callee: *callee,
                    arguments,
                }),
                consumed,
            ));
            Ok(result)
        }
        ir::PortableInstruction::MakeRecord {
            destination,
            record,
            type_arguments,
            fields,
        } => Ok(one(ir::Instruction::Portable(
            ir::PortableInstruction::MakeRecord {
                destination: *destination,
                record: *record,
                type_arguments: type_arguments
                    .iter()
                    .map(|ty| ty.specialize(&instance.substitutions))
                    .collect(),
                fields: fields.clone(),
            },
        ))),
        ir::PortableInstruction::MakeVariant {
            destination,
            variant,
            type_arguments,
            payload,
        } => Ok(one(ir::Instruction::Portable(
            ir::PortableInstruction::MakeVariant {
                destination: *destination,
                variant: *variant,
                type_arguments: type_arguments
                    .iter()
                    .map(|ty| ty.specialize(&instance.substitutions))
                    .collect(),
                payload: payload.clone(),
            },
        ))),
        ir::PortableInstruction::MoveOut { source, .. } => {
            Ok(vec![(instruction.clone(), vec![*source])])
        }
        ir::PortableInstruction::Index {
            destination,
            object,
            index,
        } => {
            let helper = match ty(*object) {
                NativeType::StringList => Some("foster_string_list_get"),
                NativeType::String => Some("foster_string_get"),
                NativeType::Object(layout)
                    if matches!(
                        environment.physical_layouts.get(layout).kind,
                        PhysicalKind::Buffer { .. }
                    ) =>
                {
                    None
                }
                receiver => {
                    return Err(native_error(format!(
                        "native indexing does not support `{receiver:?}`"
                    )));
                }
            };
            let Some(helper) = helper else {
                return Ok(one(instruction.clone()));
            };
            let arguments = vec![*object, *index];
            Ok(one(ir::Instruction::RuntimeCall {
                destination: *destination,
                helper,
                signature: runtime_signature(*destination, &arguments, value_types),
                arguments,
            }))
        }
        ir::PortableInstruction::LoadField {
            destination,
            object,
            field,
            by_reference,
        } if !matches!(ty(*object), NativeType::Object(_)) => {
            if *by_reference {
                return Err(native_error(format!(
                    "native compilation does not support reference field `{field}`"
                )));
            }
            let helper = native_field_helper(ty(*object), field)?;
            let arguments = vec![*object];
            Ok(one(ir::Instruction::RuntimeCall {
                destination: *destination,
                helper,
                signature: runtime_signature(*destination, &arguments, value_types),
                arguments,
            }))
        }
        ir::PortableInstruction::CallContractMethod {
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
            if matches!(ty(*receiver), NativeType::Object(_)) {
                return Ok(one(ir::Instruction::Portable(
                    ir::PortableInstruction::LoadField {
                        destination: *destination,
                        object: *receiver,
                        field: name.clone(),
                        by_reference: false,
                    },
                )));
            }
            let helper = native_field_helper(ty(*receiver), name)?;
            let arguments = vec![*receiver];
            Ok(one(ir::Instruction::RuntimeCall {
                destination: *destination,
                helper,
                signature: runtime_signature(*destination, &arguments, value_types),
                arguments,
            }))
        }
        ir::PortableInstruction::Assert { condition, message } => {
            Ok(one(ir::Instruction::Assert {
                condition: *condition,
                message: *message,
            }))
        }
        _ => Ok(one(instruction.clone())),
    }
}

fn shared_call_arguments(
    arguments: &[ir::Value],
    modes: &[ParameterMode],
    value_types: &mut Vec<NativeType>,
    storage_hints: &mut Vec<Option<u16>>,
    instructions: &mut Vec<(ir::Instruction, Vec<ir::Value>)>,
) -> Result<(Vec<ir::Value>, Vec<ir::Value>), FosterError> {
    if arguments.len() != modes.len() {
        return Err(native_error(
            "shared call ownership metadata has the wrong arity",
        ));
    }
    let mut lowered = Vec::with_capacity(arguments.len());
    let mut consumed = Vec::new();
    for (argument, mode) in arguments.iter().zip(modes) {
        let ty = value_types[argument.0 as usize];
        if *mode == ParameterMode::Borrow && matches!(ty, NativeType::Object(_)) {
            let retained = allocate_shared_value(value_types, storage_hints, ty);
            instructions.push((
                ir::Instruction::Portable(ir::PortableInstruction::Move {
                    destination: retained,
                    source: *argument,
                }),
                Vec::new(),
            ));
            lowered.push(retained);
        } else {
            lowered.push(*argument);
            if *mode == ParameterMode::Consume {
                consumed.push(*argument);
            }
        }
    }
    Ok((lowered, consumed))
}

fn shared_capture_arguments(
    captures: &[(crate::hir::CaptureMode, ir::Value)],
    expected_types: &[NativeType],
    value_types: &mut Vec<NativeType>,
    storage_hints: &mut Vec<Option<u16>>,
    instructions: &mut Vec<(ir::Instruction, Vec<ir::Value>)>,
    function: &str,
) -> Result<(Vec<ir::Value>, Vec<ir::Value>), FosterError> {
    if captures.len() != expected_types.len() {
        return Err(native_error("closure capture ABI has the wrong arity"));
    }
    let mut lowered = Vec::with_capacity(captures.len());
    let mut consumed = Vec::new();
    for ((mode, value), expected) in captures.iter().zip(expected_types) {
        let ty = value_types[value.0 as usize];
        match mode {
            crate::hir::CaptureMode::Move => {
                lowered.push(*value);
                consumed.push(*value);
            }
            crate::hir::CaptureMode::Copy => {
                if matches!(ty, NativeType::Object(_)) {
                    let retained = allocate_shared_value(value_types, storage_hints, ty);
                    instructions.push((
                        ir::Instruction::Portable(ir::PortableInstruction::Move {
                            destination: retained,
                            source: *value,
                        }),
                        Vec::new(),
                    ));
                    lowered.push(retained);
                } else {
                    lowered.push(*value);
                }
            }
            crate::hir::CaptureMode::Ref => {
                let NativeType::Object(layout) = expected else {
                    return Err(native_error(format!(
                        "native closure `{function}` has a non-reference capture ABI"
                    )));
                };
                let reference =
                    allocate_shared_value(value_types, storage_hints, NativeType::Object(*layout));
                instructions.push((
                    ir::Instruction::Portable(ir::PortableInstruction::MakeWholeReference {
                        destination: reference,
                        pointee_type: crate::vm::VerificationType::Unknown,
                        object: *value,
                    }),
                    Vec::new(),
                ));
                lowered.push(reference);
            }
            crate::hir::CaptureMode::Pending => {
                return Err(native_error(format!(
                    "native closure `{function}` has an unresolved capture mode"
                )));
            }
        }
    }
    Ok((lowered, consumed))
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

fn native_field_helper(receiver: NativeType, field: &str) -> Result<&'static str, FosterError> {
    let receiver_kind = match receiver {
        NativeType::Arguments => Some(crate::intrinsics::NativeReceiverKind::Arguments),
        NativeType::String => Some(crate::intrinsics::NativeReceiverKind::String),
        NativeType::StringList => Some(crate::intrinsics::NativeReceiverKind::StringList),
        _ => None,
    };
    receiver_kind
        .and_then(|receiver| crate::intrinsics::native_member_runtime(receiver, field))
        .ok_or_else(|| {
            native_error(format!(
                "native compilation does not support field `{field}` on `{receiver:?}`"
            ))
        })
}

fn reference_load_helper(ty: NativeType) -> &'static str {
    match ty {
        NativeType::Unit | NativeType::Bool | NativeType::Byte => "foster_ref_load_i8",
        NativeType::CodePoint => "foster_ref_load_i32",
        NativeType::Int => "foster_ref_load_i64",
        NativeType::Float => "foster_ref_load_f64",
        NativeType::String
        | NativeType::Arguments
        | NativeType::StringList
        | NativeType::Object(_)
        | NativeType::Opaque => "foster_ref_load_ptr",
    }
}

fn reference_store_helper(ty: NativeType) -> &'static str {
    match ty {
        NativeType::Unit | NativeType::Bool | NativeType::Byte => "foster_ref_store_i8",
        NativeType::CodePoint => "foster_ref_store_i32",
        NativeType::Int => "foster_ref_store_i64",
        NativeType::Float => "foster_ref_store_f64",
        NativeType::String
        | NativeType::Arguments
        | NativeType::StringList
        | NativeType::Object(_)
        | NativeType::Opaque => "foster_ref_store_ptr",
    }
}

fn lower_native_ir(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    function: &ir::Function,
    backend: &NativeBackend<'_>,
) -> Result<(), FosterError> {
    let pointer_type = module.target_config().pointer_type();
    let mut home_types = HashMap::<u16, NativeType>::new();
    for (value, home) in function.storage_hints.iter().enumerate() {
        if let Some(home) = home {
            home_types
                .entry(*home)
                .or_insert(function.value_types[value]);
        }
    }
    let homes = home_types
        .into_iter()
        .map(|(home, ty)| {
            let lowered = cranelift_type(ty, pointer_type);
            let size = lowered.bytes();
            let align_shift = u8::try_from(size.trailing_zeros()).unwrap_or(0);
            let slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                size,
                align_shift,
            ));
            (home, slot)
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
    for (value, lowered) in &values {
        if let Some(home) = function.storage_hints[value.0 as usize] {
            builder
                .ins()
                .stack_store(pointer_type, *lowered, homes[&home], 0);
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
        for instruction in &block.instructions {
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
                    backend.objects.layouts,
                )?;
                if lowered_bindings.len() != bindings.len() {
                    return Err(native_error(
                        "native pattern binding arity changed during lowering",
                    ));
                }
                values.insert(*destination, matched);
                for (binding, value) in bindings.iter().zip(lowered_bindings) {
                    if let NativeType::Object(layout) = function.value_type(*binding)
                        && backend.objects.layouts.is_managed(layout)
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
                function,
                instruction,
                &values,
                &homes,
                backend,
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
    layouts: NativeLayouts<'_>,
) -> Result<(ClifValue, Vec<ClifValue>), FosterError> {
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
        Pattern::String(_) | Pattern::Symbol(_) => Err(native_error(
            "native string and symbol patterns are not yet lowered",
        )),
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
                    layouts,
                )?;
                matched = builder.ins().band(matched, field_matched);
                bindings.append(&mut field_bindings);
            }
            builder.append_block_param(join_block, types::I8);
            for binding in &bindings {
                builder.append_block_param(join_block, builder.func.dfg.value_type(*binding));
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

fn native_type_from_value_layout(value: ValueLayout) -> NativeType {
    match (value.kind, value.pointee) {
        (ScalarKind::I8, _) => NativeType::Byte,
        (ScalarKind::I32, _) => NativeType::CodePoint,
        (ScalarKind::I64, _) => NativeType::Int,
        (ScalarKind::F64, _) => NativeType::Float,
        (ScalarKind::Pointer, Some(layout)) => NativeType::Object(layout),
        (ScalarKind::Pointer, None) => NativeType::Opaque,
    }
}

fn lower_native_instruction(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    function: &ir::Function,
    instruction: &ir::Instruction,
    values: &HashMap<ir::Value, ClifValue>,
    homes: &HashMap<u16, StackSlot>,
    backend: &NativeBackend<'_>,
) -> Result<Option<ClifValue>, FosterError> {
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
            let reference = module.declare_func_in_func(backend.functions[function], builder.func);
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
        ir::Instruction::Portable(instruction) => {
            return lower_portable_native(
                builder,
                module,
                function,
                instruction,
                values,
                homes,
                backend,
            );
        }
    };
    Ok(Some(result))
}

fn lower_portable_native(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    function: &ir::Function,
    instruction: &ir::PortableInstruction,
    values: &HashMap<ir::Value, ClifValue>,
    homes: &HashMap<u16, StackSlot>,
    backend: &NativeBackend<'_>,
) -> Result<Option<ClifValue>, FosterError> {
    let objects = backend.objects;
    let get = |value: &ir::Value| values[value];
    match instruction {
        ir::PortableInstruction::Drop { value } => {
            if let NativeType::Object(layout) = function.value_type(*value)
                && objects.layouts.is_managed(layout)
            {
                objects.release(builder, module, get(value), layout)?;
            }
            Ok(None)
        }
        ir::PortableInstruction::Move {
            destination,
            source,
        } => {
            let value = get(source);
            if let NativeType::Object(layout) = function.value_type(*destination)
                && objects.layouts.is_managed(layout)
            {
                objects.retain(builder, value, layout);
            }
            Ok(Some(value))
        }
        ir::PortableInstruction::CopyOnWrite {
            destination,
            source,
        } => {
            let NativeType::Object(layout) = function.value_type(*destination) else {
                return Err(native_error("copy-on-write requires a native object"));
            };
            let physical = objects.layouts.physical.get(layout);
            let source = get(source);
            let copied = match &physical.kind {
                PhysicalKind::Record { fields } => {
                    let copied = objects.allocate(builder, module, layout)?;
                    for field in fields {
                        let value =
                            load_physical_value(builder, module, source, field.offset, field.value);
                        if let Some(pointee) = field.value.pointee
                            && objects.layouts.is_managed(pointee)
                        {
                            objects.retain(builder, value, pointee);
                        }
                        store_physical_value(builder, copied, field.offset, value);
                    }
                    copied
                }
                PhysicalKind::Buffer { .. } => {
                    clone_native_buffer(builder, module, source, layout, objects)?
                }
                _ => {
                    return Err(native_error(
                        "native copy-on-write requires a record or buffer layout",
                    ));
                }
            };
            objects.release(builder, module, source, layout)?;
            Ok(Some(copied))
        }
        ir::PortableInstruction::MakeList {
            destination,
            elements,
            ..
        } => {
            let NativeType::Object(layout) = function.value_type(*destination) else {
                return Err(native_error("list result uses the wrong native layout"));
            };
            let object = allocate_native_buffer(builder, module, layout, elements.len(), objects)?;
            let PhysicalKind::Buffer {
                data_offset,
                element,
                ..
            } = objects.layouts.physical.get(layout).kind
            else {
                return Err(native_error("list result has a non-buffer layout"));
            };
            let data = load_physical_value(
                builder,
                module,
                object,
                data_offset,
                ValueLayout {
                    size: module.target_config().pointer_type().bytes(),
                    align: u16::try_from(module.target_config().pointer_type().bytes())
                        .expect("pointer alignment fits in u16"),
                    kind: ScalarKind::Pointer,
                    pointee: None,
                },
            );
            for (index, source) in elements.iter().enumerate() {
                let value = get(source);
                if let Some(pointee) = element.pointee
                    && objects.layouts.is_managed(pointee)
                {
                    objects.retain(builder, value, pointee);
                }
                store_physical_value(
                    builder,
                    data,
                    u32::try_from(index).unwrap_or(u32::MAX) * element.size,
                    value,
                );
            }
            Ok(Some(object))
        }
        ir::PortableInstruction::Index {
            destination,
            object,
            index,
        } if matches!(function.value_type(*object), NativeType::Object(_)) => {
            let NativeType::Object(layout) = function.value_type(*object) else {
                unreachable!()
            };
            let (address, element) = native_buffer_element_address(
                builder,
                module,
                get(object),
                get(index),
                layout,
                objects,
            )?;
            let value = builder.ins().load(
                physical_cranelift_type(element.kind, module.target_config().pointer_type()),
                MemFlagsData::trusted(),
                address,
                0,
            );
            if let NativeType::Object(pointee) = function.value_type(*destination)
                && objects.layouts.is_managed(pointee)
            {
                objects.retain(builder, value, pointee);
            }
            Ok(Some(value))
        }
        ir::PortableInstruction::StoreIndex {
            object,
            index,
            source,
        } => {
            let NativeType::Object(layout) = function.value_type(*object) else {
                return Err(native_error("native indexed store requires a buffer"));
            };
            let (address, element) = native_buffer_element_address(
                builder,
                module,
                get(object),
                get(index),
                layout,
                objects,
            )?;
            if let Some(pointee) = element.pointee
                && objects.layouts.is_managed(pointee)
            {
                let old = builder.ins().load(
                    module.target_config().pointer_type(),
                    MemFlagsData::trusted(),
                    address,
                    0,
                );
                objects.release(builder, module, old, pointee)?;
                objects.retain(builder, get(source), pointee);
            }
            store_physical_value(builder, address, 0, get(source));
            Ok(None)
        }
        ir::PortableInstruction::Append {
            destination,
            object,
            value,
        } => {
            let NativeType::Object(layout) = function.value_type(*destination) else {
                return Err(native_error("native append requires a buffer"));
            };
            let appended =
                append_native_buffer(builder, module, get(object), get(value), layout, objects)?;
            Ok(Some(appended))
        }
        ir::PortableInstruction::Push { object, value, .. } => {
            let NativeType::Object(layout) = function.value_type(*object) else {
                return Err(native_error("native push requires a buffer"));
            };
            push_native_buffer(builder, module, get(object), get(value), layout, objects)?;
            Ok(Some(builder.ins().iconst(types::I8, 0)))
        }
        ir::PortableInstruction::Contains {
            value, candidates, ..
        } => {
            let mut matched = builder.ins().iconst(types::I8, 0);
            for candidate in candidates {
                let equal = lower_binary(
                    builder,
                    module,
                    BinaryOp::Equal,
                    function.value_type(*value),
                    get(value),
                    get(candidate),
                )?;
                matched = builder.ins().bor(matched, equal);
            }
            Ok(Some(matched))
        }
        ir::PortableInstruction::MakeWholeReference { object, .. } => {
            if let NativeType::Object(layout) = function.value_type(*object)
                && matches!(
                    objects.layouts.logical.get(layout).kind,
                    LayoutKind::Pointer { .. }
                )
            {
                return Ok(Some(get(object)));
            }
            let home = function.storage_hints[object.0 as usize]
                .and_then(|home| homes.get(&home).copied())
                .ok_or_else(|| native_error("referenced value has no native storage home"))?;
            Ok(Some(builder.ins().stack_addr(
                module.target_config().pointer_type(),
                home,
                0,
            )))
        }
        ir::PortableInstruction::MakeReference { object, index, .. } => {
            let (object, object_type) = native_reference_receiver(
                builder,
                module,
                get(object),
                function.value_type(*object),
                backend,
            )?;
            let NativeType::Object(layout) = object_type else {
                return Err(native_error("indexed reference requires a native buffer"));
            };
            let (address, _) = native_buffer_element_address(
                builder,
                module,
                object,
                get(index),
                layout,
                objects,
            )?;
            Ok(Some(address))
        }
        ir::PortableInstruction::MakeFieldReference { object, field, .. }
        | ir::PortableInstruction::LoadField {
            object,
            field,
            by_reference: true,
            ..
        } => {
            let (object, object_type) = native_reference_receiver(
                builder,
                module,
                get(object),
                function.value_type(*object),
                backend,
            )?;
            let NativeType::Object(layout) = object_type else {
                return Err(native_error("field reference requires a native record"));
            };
            let LayoutKind::Record { fields, .. } = &objects.layouts.logical.get(layout).kind
            else {
                return Err(native_error("field reference requires a native record"));
            };
            let slot = fields
                .iter()
                .find(|slot| slot.name == *field)
                .ok_or_else(|| native_error(format!("record has no field `{field}`")))?;
            let physical = objects
                .layouts
                .physical
                .record_field(layout, slot.index)
                .ok_or_else(|| native_error("record field has no physical slot"))?;
            Ok(Some(
                builder.ins().iadd_imm_s(object, i64::from(physical.offset)),
            ))
        }
        ir::PortableInstruction::MoveOut {
            destination,
            source,
        } => {
            let source_type = function.value_type(*source);
            let is_reference = matches!(
                source_type,
                NativeType::Object(layout)
                    if matches!(objects.layouts.logical.get(layout).kind, LayoutKind::Pointer { .. })
            );
            if !is_reference {
                return Ok(Some(get(source)));
            }
            let ty = function.value_type(*destination);
            let lowered = cranelift_type(ty, module.target_config().pointer_type());
            let value = builder
                .ins()
                .load(lowered, MemFlagsData::trusted(), get(source), 0);
            let zero = match ty {
                NativeType::Float => builder.ins().f64const(0.0),
                _ => builder.ins().iconst(lowered, 0),
            };
            builder
                .ins()
                .store(MemFlagsData::trusted(), zero, get(source), 0);
            Ok(Some(value))
        }
        ir::PortableInstruction::MakeRecord {
            destination,
            record,
            type_arguments: _,
            fields: values_to_store,
        } => {
            let NativeType::Object(layout) = function.value_type(*destination) else {
                return Err(native_error("record result uses the wrong native layout"));
            };
            let LayoutKind::Record {
                record: layout_record,
                ..
            } = objects.layouts.logical.get(layout).kind
            else {
                return Err(native_error("record has a non-record logical layout"));
            };
            if layout_record != *record {
                return Err(native_error("record result uses the wrong nominal layout"));
            }
            let physical = objects.layouts.physical.get(layout);
            let PhysicalKind::Record { fields } = &physical.kind else {
                return Err(native_error("record has a non-record physical layout"));
            };
            let object = objects.allocate(builder, module, layout)?;
            for ((name, source), field) in values_to_store.iter().zip(fields) {
                if name != &field.name {
                    return Err(native_error(
                        "logical and physical record field order disagree",
                    ));
                }
                let value = get(source);
                if let NativeType::Object(pointee) = function.value_type(*source) {
                    objects.retain(builder, value, pointee);
                }
                store_physical_value(builder, object, field.offset, value);
            }
            Ok(Some(object))
        }
        ir::PortableInstruction::MakeVariant {
            destination,
            variant,
            type_arguments: _,
            payload,
        } => {
            let NativeType::Object(layout) = function.value_type(*destination) else {
                return Err(native_error("variant result uses the wrong native layout"));
            };
            let LayoutKind::Variant { alternatives, .. } =
                &objects.layouts.logical.get(layout).kind
            else {
                return Err(native_error("variant has a non-variant logical layout"));
            };
            let tag = alternatives
                .iter()
                .find(|alternative| alternative.variant == *variant)
                .map(|alternative| alternative.tag)
                .ok_or_else(|| native_error("variant construction has no native alternative"))?;
            let physical = objects.layouts.physical.get(layout);
            let PhysicalKind::Variant {
                tag_offset,
                alternatives,
                ..
            } = &physical.kind
            else {
                return Err(native_error("variant has a non-variant physical layout"));
            };
            let alternative = alternatives
                .iter()
                .find(|alternative| alternative.tag == tag)
                .ok_or_else(|| native_error("variant alternative has no physical layout"))?;
            if alternative.fields.len() != payload.len() {
                return Err(native_error(
                    "variant payload arity disagrees with its layout",
                ));
            }
            let object = objects.allocate(builder, module, layout)?;
            let tag = builder.ins().iconst(types::I32, i64::from(tag));
            builder
                .ins()
                .store(MemFlagsData::trusted(), tag, object, *tag_offset as i32);
            for (source, field) in payload.iter().zip(&alternative.fields) {
                let value = get(source);
                if let NativeType::Object(pointee) = function.value_type(*source) {
                    objects.retain(builder, value, pointee);
                }
                store_physical_value(builder, object, field.offset, value);
            }
            Ok(Some(object))
        }
        ir::PortableInstruction::MakeClosure {
            destination,
            function: target,
            captures,
            ..
        } => {
            let NativeType::Object(layout) = function.value_type(*destination) else {
                return Err(native_error("closure result uses the wrong native layout"));
            };
            let physical = objects.layouts.physical.get(layout);
            let PhysicalKind::Closure {
                code_offset,
                signature_offset,
                captures: fields,
            } = &physical.kind
            else {
                return Err(native_error("closure has a non-closure physical layout"));
            };
            if captures.len() != fields.len() {
                return Err(native_error("closure capture layout has the wrong arity"));
            }
            let object = objects.allocate(builder, module, layout)?;
            let reference = module.declare_func_in_func(backend.functions[target], builder.func);
            let code = builder
                .ins()
                .func_addr(module.target_config().pointer_type(), reference);
            store_physical_value(builder, object, *code_offset, code);
            let no_signature = builder
                .ins()
                .iconst(module.target_config().pointer_type(), 0);
            store_physical_value(builder, object, *signature_offset, no_signature);
            for ((_, source), field) in captures.iter().zip(fields) {
                store_physical_value(builder, object, field.offset, get(source));
            }
            Ok(Some(object))
        }
        ir::PortableInstruction::CallValue {
            destination: _,
            callee,
            arguments,
        } => {
            let NativeType::Object(layout) = function.value_type(*callee) else {
                return Err(native_error(
                    "dynamic call requires a concrete closure layout",
                ));
            };
            let LayoutKind::Closure {
                function: target,
                specialization,
                ..
            } = &objects.layouts.logical.get(layout).kind
            else {
                return Err(native_error("dynamic call target is not a closure layout"));
            };
            let target = backend.ir.instances[&SpecializationKey {
                function: *target,
                substitutions: specialization.clone(),
            }];
            let physical = objects.layouts.physical.get(layout);
            let PhysicalKind::Closure {
                code_offset,
                captures,
                ..
            } = &physical.kind
            else {
                return Err(native_error(
                    "dynamic call target is not a physical closure",
                ));
            };
            let closure = get(callee);
            let mut lowered = Vec::with_capacity(captures.len() + arguments.len());
            for field in captures {
                let value =
                    load_physical_value(builder, module, closure, field.offset, field.value);
                if let Some(pointee) = field.value.pointee
                    && objects.layouts.is_managed(pointee)
                {
                    objects.retain(builder, value, pointee);
                }
                lowered.push(value);
            }
            lowered.extend(arguments.iter().map(get));
            let code = builder.ins().load(
                module.target_config().pointer_type(),
                MemFlagsData::trusted(),
                closure,
                *code_offset as i32,
            );
            let signature = signature(module, &backend.ir.function_types[&target]);
            let signature = builder.func.import_signature(signature);
            let call = builder.ins().call_indirect(signature, code, &lowered);
            Ok(Some(builder.inst_results(call)[0]))
        }
        ir::PortableInstruction::LoadField {
            destination,
            object,
            field,
            by_reference: false,
        } => {
            let NativeType::Object(layout) = function.value_type(*object) else {
                return Err(native_error("native field load requires a Foster object"));
            };
            if matches!(
                objects.layouts.logical.get(layout).kind,
                LayoutKind::Builtin {
                    ty: crate::vm::VerificationType::List(_)
                }
            ) {
                let (data_offset, length_offset, _, element) =
                    native_buffer_layout(layout, objects)?;
                let word = module.target_config().pointer_type();
                let length = builder.ins().load(
                    word,
                    MemFlagsData::trusted(),
                    get(object),
                    length_offset as i32,
                );
                let result = match field.as_str() {
                    "empty?" => builder.ins().icmp_imm_s(IntCC::Equal, length, 0),
                    "length" => length,
                    "head" => {
                        let empty = builder.ins().icmp_imm_s(IntCC::Equal, length, 0);
                        builder.ins().trapnz(empty, TrapCode::HEAP_OUT_OF_BOUNDS);
                        let data = builder.ins().load(
                            word,
                            MemFlagsData::trusted(),
                            get(object),
                            data_offset as i32,
                        );
                        let value = builder.ins().load(
                            physical_cranelift_type(element.kind, word),
                            MemFlagsData::trusted(),
                            data,
                            0,
                        );
                        if let Some(pointee) = element.pointee
                            && objects.layouts.is_managed(pointee)
                        {
                            objects.retain(builder, value, pointee);
                        }
                        value
                    }
                    "rest" => native_buffer_tail(builder, module, get(object), layout, objects)?,
                    _ => return Err(native_error(format!("native list has no field `{field}`"))),
                };
                return Ok(Some(result));
            }
            let LayoutKind::Record { fields, .. } = &objects.layouts.logical.get(layout).kind
            else {
                return Err(native_error("native field load requires a record or list"));
            };
            let slot = fields
                .iter()
                .find(|slot| slot.name == *field)
                .ok_or_else(|| native_error(format!("record has no field `{field}`")))?;
            let physical = objects
                .layouts
                .physical
                .record_field(layout, slot.index)
                .ok_or_else(|| native_error("record field has no physical slot"))?;
            let result = load_physical_value(
                builder,
                module,
                get(object),
                physical.offset,
                physical.value,
            );
            if let NativeType::Object(pointee) = function.value_type(*destination) {
                objects.retain(builder, result, pointee);
            }
            Ok(Some(result))
        }
        ir::PortableInstruction::StoreField {
            object,
            field,
            source,
        } => {
            let NativeType::Object(layout) = function.value_type(*object) else {
                return Err(native_error("native field store requires a Foster object"));
            };
            let LayoutKind::Record { fields, .. } = &objects.layouts.logical.get(layout).kind
            else {
                return Err(native_error("native field store requires a record"));
            };
            let slot = fields
                .iter()
                .find(|slot| slot.name == *field)
                .ok_or_else(|| native_error(format!("record has no field `{field}`")))?;
            let physical = objects
                .layouts
                .physical
                .record_field(layout, slot.index)
                .ok_or_else(|| native_error("record field has no physical slot"))?;
            if let Some(pointee) = physical.value.pointee
                && objects.layouts.is_managed(pointee)
            {
                let old = load_physical_value(
                    builder,
                    module,
                    get(object),
                    physical.offset,
                    physical.value,
                );
                objects.release(builder, module, old, pointee)?;
            }
            let source_value = get(source);
            if let NativeType::Object(pointee) = function.value_type(*source) {
                objects.retain(builder, source_value, pointee);
            }
            store_physical_value(builder, get(object), physical.offset, source_value);
            Ok(None)
        }
        ir::PortableInstruction::Builtin {
            destination,
            builtin,
            arguments,
        } => {
            use crate::intrinsics::{NativeInlineIntrinsic, NativeIntrinsic};
            let lowered = arguments.iter().map(get).collect::<Vec<_>>();
            let result = match builtin.descriptor().native {
                NativeIntrinsic::Inline(NativeInlineIntrinsic::IntegerToCodePoint) => {
                    let value = lowered[0];
                    let above =
                        builder
                            .ins()
                            .icmp_imm_u(IntCC::UnsignedGreaterThan, value, 0x10_ffff);
                    let below_surrogate =
                        builder
                            .ins()
                            .icmp_imm_s(IntCC::SignedLessThan, value, 0xd800);
                    let above_surrogate =
                        builder
                            .ins()
                            .icmp_imm_s(IntCC::SignedGreaterThan, value, 0xdfff);
                    let valid_surrogate = builder.ins().bor(below_surrogate, above_surrogate);
                    let invalid_surrogate = builder.ins().bxor_imm_u(valid_surrogate, 1);
                    let invalid = builder.ins().bor(above, invalid_surrogate);
                    builder
                        .ins()
                        .trapnz(invalid, TrapCode::BAD_CONVERSION_TO_INTEGER);
                    builder.ins().ireduce(types::I32, value)
                }
                NativeIntrinsic::Inline(NativeInlineIntrinsic::ByteIsValid) => builder
                    .ins()
                    .icmp_imm_u(IntCC::UnsignedLessThanOrEqual, lowered[0], 255),
                NativeIntrinsic::Inline(NativeInlineIntrinsic::IntegerToByte) => {
                    let invalid =
                        builder
                            .ins()
                            .icmp_imm_u(IntCC::UnsignedGreaterThan, lowered[0], 255);
                    builder
                        .ins()
                        .trapnz(invalid, TrapCode::BAD_CONVERSION_TO_INTEGER);
                    builder.ins().ireduce(types::I8, lowered[0])
                }
                NativeIntrinsic::Runtime(helper) => runtime_call(
                    builder,
                    module,
                    helper,
                    &runtime_signature(*destination, arguments, &function.value_types),
                    &lowered,
                )?,
                NativeIntrinsic::Unavailable => {
                    return Err(native_error(format!(
                        "intrinsic `{builtin:?}` reached Cranelift without a native lowering"
                    )));
                }
            };
            Ok(Some(result))
        }
        unsupported => Err(native_error(format!(
            "portable operation reached Cranelift without native legalization: {unsupported:?}"
        ))),
    }
}

fn native_reference_receiver(
    builder: &mut FunctionBuilder<'_>,
    module: &ObjectModule,
    value: ClifValue,
    mut ty: NativeType,
    backend: &NativeBackend<'_>,
) -> Result<(ClifValue, NativeType), FosterError> {
    let mut value = value;
    while let NativeType::Object(layout) = ty {
        let LayoutKind::Pointer { pointee, .. } = &backend.ir.layouts.get(layout).kind else {
            break;
        };
        ty = native_verification_type(backend.ir.program, backend.ir.layouts, pointee, None)?;
        value = builder.ins().load(
            cranelift_type(ty, module.target_config().pointer_type()),
            MemFlagsData::trusted(),
            value,
            0,
        );
    }
    Ok((value, ty))
}

fn native_buffer_layout(
    layout: LayoutId,
    objects: ObjectRuntime<'_>,
) -> Result<(u32, u32, u32, ValueLayout), FosterError> {
    match objects.layouts.physical.get(layout).kind {
        PhysicalKind::Buffer {
            data_offset,
            length_offset,
            capacity_offset,
            element,
        } => Ok((data_offset, length_offset, capacity_offset, element)),
        _ => Err(native_error(
            "native list operation requires a buffer layout",
        )),
    }
}

fn allocate_native_buffer(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    layout: LayoutId,
    length: usize,
    objects: ObjectRuntime<'_>,
) -> Result<ClifValue, FosterError> {
    let (data_offset, length_offset, capacity_offset, element) =
        native_buffer_layout(layout, objects)?;
    let object = objects.allocate(builder, module, layout)?;
    let capacity = length.max(1);
    let size = builder.ins().iconst(
        types::I64,
        i64::try_from(capacity).unwrap_or(i64::MAX) * i64::from(element.size),
    );
    let align = builder.ins().iconst(types::I64, i64::from(element.align));
    let data = runtime_call(
        builder,
        module,
        "foster_alloc",
        &ir::Signature {
            parameters: vec![NativeType::Int, NativeType::Int],
            result: NativeType::Opaque,
        },
        &[size, align],
    )?;
    let word = module.target_config().pointer_type();
    store_physical_value(builder, object, data_offset, data);
    let length = builder
        .ins()
        .iconst(word, i64::try_from(length).unwrap_or(i64::MAX));
    store_physical_value(builder, object, length_offset, length);
    let capacity = builder
        .ins()
        .iconst(word, i64::try_from(capacity).unwrap_or(i64::MAX));
    store_physical_value(builder, object, capacity_offset, capacity);
    Ok(object)
}

fn native_buffer_element_address(
    builder: &mut FunctionBuilder<'_>,
    module: &ObjectModule,
    object: ClifValue,
    index: ClifValue,
    layout: LayoutId,
    objects: ObjectRuntime<'_>,
) -> Result<(ClifValue, ValueLayout), FosterError> {
    let (data_offset, length_offset, _, element) = native_buffer_layout(layout, objects)?;
    let word = module.target_config().pointer_type();
    let length = builder
        .ins()
        .load(word, MemFlagsData::trusted(), object, length_offset as i32);
    let outside = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, index, length);
    builder.ins().trapnz(outside, TrapCode::HEAP_OUT_OF_BOUNDS);
    let data = builder
        .ins()
        .load(word, MemFlagsData::trusted(), object, data_offset as i32);
    let offset = builder.ins().imul_imm_u(index, i64::from(element.size));
    Ok((builder.ins().iadd(data, offset), element))
}

fn copy_native_buffer_elements(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    source: ClifValue,
    target: ClifValue,
    layout: LayoutId,
    retain: bool,
    objects: ObjectRuntime<'_>,
) -> Result<(), FosterError> {
    let (data_offset, length_offset, _, element) = native_buffer_layout(layout, objects)?;
    let word = module.target_config().pointer_type();
    let source_data = builder
        .ins()
        .load(word, MemFlagsData::trusted(), source, data_offset as i32);
    let target_data = builder
        .ins()
        .load(word, MemFlagsData::trusted(), target, data_offset as i32);
    let length = builder
        .ins()
        .load(word, MemFlagsData::trusted(), source, length_offset as i32);
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
    let offset = builder.ins().imul_imm_u(index, i64::from(element.size));
    let source_address = builder.ins().iadd(source_data, offset);
    let value = builder.ins().load(
        physical_cranelift_type(element.kind, word),
        MemFlagsData::trusted(),
        source_address,
        0,
    );
    if retain
        && let Some(pointee) = element.pointee
        && objects.layouts.is_managed(pointee)
    {
        objects.retain(builder, value, pointee);
    }
    let target_address = builder.ins().iadd(target_data, offset);
    store_physical_value(builder, target_address, 0, value);
    let next = builder.ins().iadd_imm_s(index, 1);
    builder.ins().jump(loop_block, &[next.into()]);
    builder.switch_to_block(finish);
    Ok(())
}

fn clone_native_buffer(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    source: ClifValue,
    layout: LayoutId,
    objects: ObjectRuntime<'_>,
) -> Result<ClifValue, FosterError> {
    let (_, length_offset, _, _) = native_buffer_layout(layout, objects)?;
    let length = builder.ins().load(
        module.target_config().pointer_type(),
        MemFlagsData::trusted(),
        source,
        length_offset as i32,
    );
    let target = allocate_native_buffer_dynamic(builder, module, layout, length, objects)?;
    copy_native_buffer_elements(builder, module, source, target, layout, true, objects)?;
    Ok(target)
}

fn native_buffer_tail(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    source: ClifValue,
    layout: LayoutId,
    objects: ObjectRuntime<'_>,
) -> Result<ClifValue, FosterError> {
    let (data_offset, length_offset, _, element) = native_buffer_layout(layout, objects)?;
    let word = module.target_config().pointer_type();
    let source_data = builder
        .ins()
        .load(word, MemFlagsData::trusted(), source, data_offset as i32);
    let length = builder
        .ins()
        .load(word, MemFlagsData::trusted(), source, length_offset as i32);
    let empty = builder.ins().icmp_imm_s(IntCC::Equal, length, 0);
    let zero = builder.ins().iconst(word, 0);
    let decremented = builder.ins().iadd_imm_s(length, -1);
    let tail_length = builder.ins().select(empty, zero, decremented);
    let target = allocate_native_buffer_dynamic(builder, module, layout, tail_length, objects)?;
    let target_data = builder
        .ins()
        .load(word, MemFlagsData::trusted(), target, data_offset as i32);
    let loop_block = builder.create_block();
    let copy = builder.create_block();
    let finish = builder.create_block();
    builder.append_block_param(loop_block, word);
    builder.ins().jump(loop_block, &[zero.into()]);
    builder.switch_to_block(loop_block);
    let index = builder.block_params(loop_block)[0];
    let done = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, index, tail_length);
    builder.ins().brif(done, finish, &[], copy, &[]);
    builder.switch_to_block(copy);
    let target_offset = builder.ins().imul_imm_u(index, i64::from(element.size));
    let source_offset = builder
        .ins()
        .iadd_imm_s(target_offset, i64::from(element.size));
    let source_address = builder.ins().iadd(source_data, source_offset);
    let value = builder.ins().load(
        physical_cranelift_type(element.kind, word),
        MemFlagsData::trusted(),
        source_address,
        0,
    );
    if let Some(pointee) = element.pointee
        && objects.layouts.is_managed(pointee)
    {
        objects.retain(builder, value, pointee);
    }
    let target_address = builder.ins().iadd(target_data, target_offset);
    store_physical_value(builder, target_address, 0, value);
    let next = builder.ins().iadd_imm_s(index, 1);
    builder.ins().jump(loop_block, &[next.into()]);
    builder.switch_to_block(finish);
    Ok(target)
}

fn copy_native_buffer_data(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    source_data: ClifValue,
    target_data: ClifValue,
    length: ClifValue,
    element: ValueLayout,
) {
    let word = module.target_config().pointer_type();
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
    let offset = builder.ins().imul_imm_u(index, i64::from(element.size));
    let source = builder.ins().iadd(source_data, offset);
    let value = builder.ins().load(
        physical_cranelift_type(element.kind, word),
        MemFlagsData::trusted(),
        source,
        0,
    );
    let target = builder.ins().iadd(target_data, offset);
    store_physical_value(builder, target, 0, value);
    let next = builder.ins().iadd_imm_s(index, 1);
    builder.ins().jump(loop_block, &[next.into()]);
    builder.switch_to_block(finish);
}

fn allocate_native_buffer_dynamic(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    layout: LayoutId,
    length: ClifValue,
    objects: ObjectRuntime<'_>,
) -> Result<ClifValue, FosterError> {
    let (data_offset, length_offset, capacity_offset, element) =
        native_buffer_layout(layout, objects)?;
    let object = objects.allocate(builder, module, layout)?;
    let word = module.target_config().pointer_type();
    let is_empty = builder.ins().icmp_imm_s(IntCC::Equal, length, 0);
    let one = builder.ins().iconst(word, 1);
    let capacity = builder.ins().select(is_empty, one, length);
    let size = builder.ins().imul_imm_u(capacity, i64::from(element.size));
    let align = builder.ins().iconst(types::I64, i64::from(element.align));
    let data = runtime_call(
        builder,
        module,
        "foster_alloc",
        &ir::Signature {
            parameters: vec![NativeType::Int, NativeType::Int],
            result: NativeType::Opaque,
        },
        &[size, align],
    )?;
    store_physical_value(builder, object, data_offset, data);
    store_physical_value(builder, object, length_offset, length);
    store_physical_value(builder, object, capacity_offset, capacity);
    Ok(object)
}

fn append_native_buffer(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    source: ClifValue,
    value: ClifValue,
    layout: LayoutId,
    objects: ObjectRuntime<'_>,
) -> Result<ClifValue, FosterError> {
    let target = clone_native_buffer(builder, module, source, layout, objects)?;
    push_native_buffer(builder, module, target, value, layout, objects)?;
    Ok(target)
}

fn push_native_buffer(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    object: ClifValue,
    value: ClifValue,
    layout: LayoutId,
    objects: ObjectRuntime<'_>,
) -> Result<(), FosterError> {
    let (data_offset, length_offset, capacity_offset, element) =
        native_buffer_layout(layout, objects)?;
    let word = module.target_config().pointer_type();
    let old_data = builder
        .ins()
        .load(word, MemFlagsData::trusted(), object, data_offset as i32);
    let length = builder
        .ins()
        .load(word, MemFlagsData::trusted(), object, length_offset as i32);
    let old_capacity = builder.ins().load(
        word,
        MemFlagsData::trusted(),
        object,
        capacity_offset as i32,
    );
    let new_length = builder.ins().iadd_imm_s(length, 1);
    let new_capacity = new_length;
    let size = builder
        .ins()
        .imul_imm_u(new_capacity, i64::from(element.size));
    let align = builder.ins().iconst(types::I64, i64::from(element.align));
    let new_data = runtime_call(
        builder,
        module,
        "foster_alloc",
        &ir::Signature {
            parameters: vec![NativeType::Int, NativeType::Int],
            result: NativeType::Opaque,
        },
        &[size, align],
    )?;
    copy_native_buffer_data(builder, module, old_data, new_data, length, element);
    store_physical_value(builder, object, data_offset, new_data);
    store_physical_value(builder, object, capacity_offset, new_capacity);
    let offset = builder.ins().imul_imm_u(length, i64::from(element.size));
    let address = builder.ins().iadd(new_data, offset);
    if let Some(pointee) = element.pointee
        && objects.layouts.is_managed(pointee)
    {
        objects.retain(builder, value, pointee);
    }
    store_physical_value(builder, address, 0, value);
    store_physical_value(builder, object, length_offset, new_length);
    let old_size = builder
        .ins()
        .imul_imm_u(old_capacity, i64::from(element.size));
    runtime_call(
        builder,
        module,
        "foster_dealloc",
        &ir::Signature {
            parameters: vec![NativeType::Opaque, NativeType::Int, NativeType::Int],
            result: NativeType::Unit,
        },
        &[old_data, old_size, align],
    )?;
    Ok(())
}

fn allocate_object(
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
        "foster_alloc",
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

fn retain_object(
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

fn release_object(
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

fn load_physical_value(
    builder: &mut FunctionBuilder<'_>,
    module: &ObjectModule,
    object: ClifValue,
    offset: u32,
    value: ValueLayout,
) -> ClifValue {
    let ty = physical_cranelift_type(value.kind, module.target_config().pointer_type());
    builder
        .ins()
        .load(ty, MemFlagsData::trusted(), object, offset as i32)
}

fn store_physical_value(
    builder: &mut FunctionBuilder<'_>,
    object: ClifValue,
    offset: u32,
    value: ClifValue,
) {
    builder
        .ins()
        .store(MemFlagsData::trusted(), value, object, offset as i32);
}

fn physical_cranelift_type(kind: ScalarKind, pointer_type: ClifType) -> ClifType {
    match kind {
        ScalarKind::I8 => types::I8,
        ScalarKind::I32 => types::I32,
        ScalarKind::I64 => types::I64,
        ScalarKind::F64 => types::F64,
        ScalarKind::Pointer => pointer_type,
    }
}

fn lower_native_terminator(
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

fn lower_binary(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    operator: BinaryOp,
    operand_type: NativeType,
    left: ClifValue,
    right: ClifValue,
) -> Result<ClifValue, FosterError> {
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
    arguments: &[ClifValue],
) -> Result<ClifValue, FosterError> {
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
        NativeType::Opaque
        | NativeType::Arguments
        | NativeType::StringList
        | NativeType::Object(_) => {
            unreachable!("rejected above")
        }
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
        NativeType::Opaque
        | NativeType::Arguments
        | NativeType::StringList
        | NativeType::Object(_) => {
            unreachable!("rejected above")
        }
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
        r#"use std::alloc::{{Layout, alloc_zeroed, dealloc, handle_alloc_error}};
use std::ffi::OsString;
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
extern "C" fn foster_alloc(size: i64, align: i64) -> usize {{
    let layout = Layout::from_size_align(size as usize, align as usize)
        .unwrap_or_else(|_| std::process::abort());
    let pointer = unsafe {{ alloc_zeroed(layout) }};
    if pointer.is_null() {{
        handle_alloc_error(layout);
    }}
    pointer as usize
}}

#[unsafe(no_mangle)]
extern "C" fn foster_dealloc(pointer: usize, size: i64, align: i64) -> u8 {{
    let layout = Layout::from_size_align(size as usize, align as usize)
        .unwrap_or_else(|_| std::process::abort());
    unsafe {{ dealloc(pointer as *mut u8, layout) }};
    0
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

#[unsafe(no_mangle)]
extern "C" fn foster_parse_float(value: usize) -> f64 {{
    unsafe {{ string_value(value) }}.parse::<f64>().unwrap_or_else(|_| {{
        eprintln!("error: invalid Float text");
        std::process::exit(2);
    }})
}}

#[unsafe(no_mangle)]
extern "C" fn foster_format_float(value: f64) -> usize {{
    Box::into_raw(Box::new(value.to_string())) as usize
}}

#[unsafe(no_mangle)]
extern "C" fn foster_ref_load_i8(reference: usize) -> u8 {{ unsafe {{ *(reference as *const u8) }} }}
#[unsafe(no_mangle)]
extern "C" fn foster_ref_load_i32(reference: usize) -> u32 {{ unsafe {{ *(reference as *const u32) }} }}
#[unsafe(no_mangle)]
extern "C" fn foster_ref_load_i64(reference: usize) -> i64 {{ unsafe {{ *(reference as *const i64) }} }}
#[unsafe(no_mangle)]
extern "C" fn foster_ref_load_f64(reference: usize) -> f64 {{ unsafe {{ *(reference as *const f64) }} }}
#[unsafe(no_mangle)]
extern "C" fn foster_ref_load_ptr(reference: usize) -> usize {{ unsafe {{ *(reference as *const usize) }} }}

#[unsafe(no_mangle)]
extern "C" fn foster_ref_store_i8(reference: usize, value: u8) -> u8 {{
    unsafe {{ *(reference as *mut u8) = value }};
    0
}}
#[unsafe(no_mangle)]
extern "C" fn foster_ref_store_i32(reference: usize, value: u32) -> u8 {{
    unsafe {{ *(reference as *mut u32) = value }};
    0
}}
#[unsafe(no_mangle)]
extern "C" fn foster_ref_store_i64(reference: usize, value: i64) -> u8 {{
    unsafe {{ *(reference as *mut i64) = value }};
    0
}}
#[unsafe(no_mangle)]
extern "C" fn foster_ref_store_f64(reference: usize, value: f64) -> u8 {{
    unsafe {{ *(reference as *mut f64) = value }};
    0
}}
#[unsafe(no_mangle)]
extern "C" fn foster_ref_store_ptr(reference: usize, value: usize) -> u8 {{
    unsafe {{ *(reference as *mut usize) = value }};
    0
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
        let shared = vm::compile_shared(&compilation).unwrap();
        let shared_functions = shared.functions;
        let mut program = shared.metadata;
        let mut layouts = crate::codegen::layout::legalize(&mut program).unwrap();
        let main = program.main.unwrap();
        let instances = reachable_instances(&program, &shared_functions, main).unwrap();
        let instance_ids = instances
            .iter()
            .map(|instance| (instance.key.clone(), instance.ir_function))
            .collect::<HashMap<_, _>>();
        let function_types =
            collect_function_types(&compilation, &program, &instances, &mut layouts).unwrap();
        let physical_layouts = crate::codegen::layout::physical::PhysicalRegistry::build(
            &layouts,
            crate::codegen::layout::physical::TargetLayout::host(),
        )
        .unwrap();
        let instance = instances
            .iter()
            .find(|instance| instance.key.function == main)
            .unwrap();
        let (_, runtime_string_indices) = runtime_strings(&program);
        let function = lower_shared_to_native_ir(
            &shared_functions[&main],
            &program.functions[&main],
            &function_types[&instance.ir_function],
            &instance.key,
            NativeIrEnvironment {
                program: &program,
                shared_functions: &shared_functions,
                function_types: &function_types,
                runtime_string_indices: &runtime_string_indices,
                layouts: &layouts,
                physical_layouts: &physical_layouts,
                instances: &instance_ids,
            },
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
                for destination in instruction.destinations() {
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
