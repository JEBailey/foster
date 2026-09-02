//! Ahead-of-time native compilation through Cranelift.
//!
//! The native backend deliberately accepts a smaller language surface than the VM. Unsupported
//! operations are diagnosed before an object is emitted, which keeps the portable bytecode VM as
//! the reference implementation while native support grows.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use cranelift_codegen::ir::{
    AbiParam, Block as ClifBlock, InstBuilder, MemFlagsData, Signature as ClifSignature, TrapCode,
    Type as ClifType, Value as ClifValue, types,
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
        !matches!(
            self.logical.get(layout).kind,
            LayoutKind::Builtin { .. } | LayoutKind::Pointer { .. } | LayoutKind::Opaque
        ) && !matches!(
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
    may_bind_object: bool,
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
    let mut program =
        vm::compile_with_options(compilation, vm::CompileOptions { optimize: false })?;
    let mut layouts = crate::codegen::layout::legalize(&mut program)?;
    let main = program.main.ok_or_else(|| {
        FosterError::runtime("native compilation requires a `main` function").with_code("E0900")
    })?;
    let instances = reachable_instances(&program, main)?;
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
    if matches!(
        function_types[&main_instance.ir_function].result,
        NativeType::Arguments | NativeType::StringList | NativeType::Object(_)
    ) {
        return Err(native_error(
            "native `main` cannot return Arguments or List<String>",
        ));
    }
    validate_program(compilation, &program, &instances, &function_types, &layouts)?;
    let (_, runtime_string_indices) = runtime_strings(&program);
    let environment = NativeIrEnvironment {
        program: &program,
        function_types: &function_types,
        runtime_string_indices: &runtime_string_indices,
        layouts: &layouts,
        physical_layouts: &physical_layouts,
        instances: &instance_ids,
    };
    let mut output = String::from("foster-codegen-ir 1\n\n");
    for instance in &instances {
        let function = &program.functions[&instance.key.function];
        let lowered = lower_to_native_ir(
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
    // Request verified bytecode without bytecode optimization. Its storage homes retain the
    // stable types needed while the native subset is rebuilt as shared SSA below; Cranelift still
    // performs the requested machine-level optimization.
    let mut program =
        vm::compile_with_options(compilation, vm::CompileOptions { optimize: false })?;
    let mut layouts = crate::codegen::layout::legalize(&mut program)?;
    let main = program.main.ok_or_else(|| {
        FosterError::runtime("native compilation requires a `main` function").with_code("E0900")
    })?;
    let instances = reachable_instances(&program, main)?;
    let instance_ids = instances
        .iter()
        .map(|instance| (instance.key.clone(), instance.ir_function))
        .collect::<HashMap<_, _>>();
    let function_types = collect_function_types(compilation, &program, &instances, &mut layouts)?;
    let main_instance = instances
        .iter()
        .find(|instance| instance.key.function == main && instance.key.substitutions.is_empty())
        .expect("main specialization is reachable");
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
    validate_program(compilation, &program, &instances, &function_types, &layouts)?;
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
    main: FunctionId,
) -> Result<Vec<NativeInstance>, FosterError> {
    let mut reachable = BTreeSet::new();
    let mut pending = vec![SpecializationKey {
        function: main,
        substitutions: Vec::new(),
    }];
    while let Some(instance) = pending.pop() {
        if instance
            .substitutions
            .iter()
            .any(|(_, ty)| verification_type_depth(ty) > 64)
        {
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
        for instruction in &body.instructions {
            match instruction {
                Instruction::Call {
                    function,
                    specialization,
                    ..
                }
                | Instruction::CallMethod {
                    function,
                    specialization,
                    ..
                }
                | Instruction::MakeClosure {
                    function,
                    specialization,
                    ..
                }
                | Instruction::CallClosure {
                    function,
                    specialization,
                    ..
                } => pending.push(SpecializationKey {
                    function: *function,
                    substitutions: resolve_specialization(specialization, &instance.substitutions),
                }),
                _ => {}
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

fn verification_type_depth(ty: &crate::vm::VerificationType) -> usize {
    use crate::vm::VerificationType;
    match ty {
        VerificationType::List(value)
        | VerificationType::Reference(value)
        | VerificationType::Remote(value)
        | VerificationType::Future(value) => 1 + verification_type_depth(value),
        VerificationType::Function {
            parameters, result, ..
        } => {
            1 + parameters
                .iter()
                .chain(std::iter::once(result.as_ref()))
                .map(verification_type_depth)
                .max()
                .unwrap_or(0)
        }
        VerificationType::Record { arguments, .. }
        | VerificationType::Variant { arguments, .. }
        | VerificationType::Union(arguments) => {
            1 + arguments
                .iter()
                .map(verification_type_depth)
                .max()
                .unwrap_or(0)
        }
        _ => 1,
    }
}

fn resolve_specialization(
    specialization: &crate::vm::Specialization,
    outer: &crate::vm::Specialization,
) -> crate::vm::Specialization {
    specialization
        .iter()
        .map(|(name, ty)| (name.clone(), substitute_verification_type(ty, outer)))
        .collect()
}

fn substitute_verification_type(
    ty: &crate::vm::VerificationType,
    substitutions: &crate::vm::Specialization,
) -> crate::vm::VerificationType {
    use crate::vm::VerificationType;
    match ty {
        VerificationType::Generic(name) => substitutions
            .iter()
            .find_map(|(candidate, ty)| (candidate == name).then(|| ty.clone()))
            .unwrap_or_else(|| ty.clone()),
        VerificationType::List(value) => {
            VerificationType::List(Box::new(substitute_verification_type(value, substitutions)))
        }
        VerificationType::Reference(value) => VerificationType::Reference(Box::new(
            substitute_verification_type(value, substitutions),
        )),
        VerificationType::Remote(value) => {
            VerificationType::Remote(Box::new(substitute_verification_type(value, substitutions)))
        }
        VerificationType::Future(value) => {
            VerificationType::Future(Box::new(substitute_verification_type(value, substitutions)))
        }
        VerificationType::Function {
            parameters,
            parameter_modes,
            result,
        } => VerificationType::Function {
            parameters: parameters
                .iter()
                .map(|ty| substitute_verification_type(ty, substitutions))
                .collect(),
            parameter_modes: parameter_modes.clone(),
            result: Box::new(substitute_verification_type(result, substitutions)),
        },
        VerificationType::Record { record, arguments } => VerificationType::Record {
            record: *record,
            arguments: arguments
                .iter()
                .map(|ty| substitute_verification_type(ty, substitutions))
                .collect(),
        },
        VerificationType::Variant { variant, arguments } => VerificationType::Variant {
            variant: *variant,
            arguments: arguments
                .iter()
                .map(|ty| substitute_verification_type(ty, substitutions))
                .collect(),
        },
        VerificationType::Union(members) => VerificationType::Union(
            members
                .iter()
                .map(|ty| substitute_verification_type(ty, substitutions))
                .collect(),
        ),
        _ => ty.clone(),
    }
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
                    let concrete = substitute_verification_type(ty, &instance.key.substitutions);
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
            && arguments.len() == 1
            && native_type(compilation, layouts, arguments[0], substitutions, function)?
                == NativeType::String =>
        {
            Ok(NativeType::StringList)
        }
        Type::Record { record, .. } if compilation.hir.records[record].name == "Symbol" => {
            Err(native_error(format!(
                "native compilation of `{function}` does not yet support type `{}`",
                compilation.types.display(ty)
            ))
            .with_help("use `foster build` without `--native` for the complete VM language"))
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
                    | Instruction::LoadField { .. }
                    | Instruction::StoreField { .. }
                    | Instruction::MatchPattern { .. }
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
                DropPlan::Trivial | DropPlan::Buffer { .. } | DropPlan::Runtime => {}
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
    let native_function = lower_to_native_ir(
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
                    _ => register_type(&result, *left, function)?,
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
                    .map(|ty| substitute_verification_type(ty, &instance.substitutions))
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
                    .map(|ty| substitute_verification_type(ty, &instance.substitutions))
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
        (NativeType::Object(layout), field) => {
            let LayoutKind::Record { fields, .. } = &layouts.get(layout).kind else {
                return Err(native_error(format!(
                    "native field access requires a record, found l{}",
                    layout.0
                )));
            };
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
        VerificationType::Record { record, .. } => layouts
            .record(*record)
            .or(physical_pointee)
            .map(NativeType::Object)
            .ok_or_else(|| native_error("record field has no native layout")),
        VerificationType::Variant { variant, .. } => layouts
            .variant(*variant)
            .or(physical_pointee)
            .map(NativeType::Object)
            .ok_or_else(|| native_error("variant field has no native layout")),
        unsupported => Err(native_error(format!(
            "native aggregate field type `{unsupported:?}` is not yet representable"
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
        | Instruction::MakeRecord { destination, .. }
        | Instruction::MakeVariant { destination, .. }
        | Instruction::LoadField { destination, .. }
        | Instruction::Index { destination, .. }
        | Instruction::Call { destination, .. }
        | Instruction::CallMethod { destination, .. }
        | Instruction::CallClosure { destination, .. }
        | Instruction::MakeClosure { destination, .. }
        | Instruction::CallValue { destination, .. }
        | Instruction::CallContractMethod { destination, .. } => vec![*destination],
        Instruction::MatchPattern {
            destination,
            bindings,
            ..
        } => {
            let mut definitions = vec![*destination];
            definitions.extend(bindings);
            definitions
        }
        Instruction::Jump { .. }
        | Instruction::JumpIfFalse { .. }
        | Instruction::Assert { .. }
        | Instruction::StoreField { .. }
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
        Instruction::MakeRecord { fields, .. } => {
            fields.iter().map(|(_, register)| *register).collect()
        }
        Instruction::MakeVariant { payload, .. } => payload.clone(),
        Instruction::MakeClosure { captures, .. } => {
            captures.iter().map(|(_, register)| *register).collect()
        }
        Instruction::CallValue {
            callee, arguments, ..
        } => {
            let mut uses = vec![*callee];
            uses.extend(arguments);
            uses
        }
        Instruction::LoadField { object, .. } => vec![*object],
        Instruction::StoreField { object, source, .. } => vec![*object, *source],
        Instruction::MatchPattern { subject, .. } => vec![*subject],
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
        Instruction::CallClosure {
            captures,
            arguments,
            ..
        } => {
            let mut uses = captures
                .iter()
                .map(|(_, register)| *register)
                .collect::<Vec<_>>();
            uses.extend(arguments);
            uses
        }
        Instruction::Return { source } => vec![*source],
        _ => unreachable!("validated native instruction subset"),
    }
}

fn lower_to_native_ir(
    function: &BytecodeFunction,
    function_signature: &ir::Signature,
    instance: &SpecializationKey,
    environment: NativeIrEnvironment<'_>,
) -> Result<ir::Function, FosterError> {
    let inferred = infer_register_types(
        function,
        &function_signature.parameters,
        instance,
        environment,
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
    let mut entry_seeds = Vec::new();
    let entry_arguments = parameter_registers[0]
        .iter()
        .map(|register| {
            function_parameters
                .get(usize::from(register.0))
                .copied()
                .unwrap_or_else(|| {
                    let seed = allocate_native_value(
                        &mut value_types,
                        register_types[usize::from(register.0)],
                    );
                    entry_seeds.push(seed);
                    seed
                })
        })
        .collect::<Vec<_>>();

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
        let mut instruction_spans = Vec::new();
        let mut terminator = None;
        let mut terminator_span = std::ops::Range::default();

        for (index, instruction) in function.instructions[start..end].iter().enumerate() {
            let source_index = start + index;
            let source_span = function
                .instruction_spans
                .get(source_index)
                .cloned()
                .unwrap_or_default();
            match instruction {
                Instruction::Drop { register } => {
                    if let Some(value) = state[usize::from(register.0)].take()
                        && matches!(value_types[value.0 as usize], NativeType::Object(_))
                    {
                        instructions.push(ir::Instruction::Portable(
                            ir::PortableInstruction::Drop { value },
                        ));
                    }
                }
                Instruction::LoadConstant {
                    destination,
                    constant,
                } => {
                    let constant = match environment.program.constants[usize::from(*constant)] {
                        Constant::Unit => ir::Constant::Unit,
                        Constant::Bool(value) => ir::Constant::Bool(value),
                        Constant::Integer(value) => ir::Constant::Integer(value),
                        Constant::Float(value) => ir::Constant::Float(value),
                        Constant::CodePoint(value) => ir::Constant::CodePoint(value),
                        Constant::String(_) => ir::Constant::RuntimeString(
                            environment.runtime_string_indices[constant],
                        ),
                        Constant::Symbol(_) => unreachable!("validated above"),
                    };
                    let destination = define_native_register(
                        *destination,
                        &register_types,
                        &mut state,
                        &mut value_types,
                        &mut instructions,
                    );
                    instructions.push(ir::Instruction::Constant {
                        destination,
                        value: constant,
                    });
                    instruction_spans.push(source_span);
                }
                Instruction::Move {
                    destination,
                    source,
                } => {
                    let source = native_register(&state, *source, function)?;
                    let destination = define_native_register(
                        *destination,
                        &register_types,
                        &mut state,
                        &mut value_types,
                        &mut instructions,
                    );
                    instructions.push(ir::Instruction::Portable(ir::PortableInstruction::Move {
                        destination,
                        source,
                    }));
                    instruction_spans.push(source_span);
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
                        &mut instructions,
                    );
                    instructions.push(ir::Instruction::Unary {
                        destination,
                        operator: *operator,
                        operand,
                    });
                    instruction_spans.push(source_span);
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
                        &mut instructions,
                    );
                    instructions.push(ir::Instruction::Binary {
                        destination,
                        operator: *operator,
                        left,
                        right,
                    });
                    instruction_spans.push(source_span);
                }
                Instruction::MakeRecord {
                    destination,
                    record,
                    type_arguments,
                    fields,
                } => {
                    let destination = define_native_register(
                        *destination,
                        &register_types,
                        &mut state,
                        &mut value_types,
                        &mut instructions,
                    );
                    let fields = fields
                        .iter()
                        .map(|(name, register)| {
                            Ok((name.clone(), native_register(&state, *register, function)?))
                        })
                        .collect::<Result<Vec<_>, FosterError>>()?;
                    instructions.push(ir::Instruction::Portable(
                        ir::PortableInstruction::MakeRecord {
                            destination,
                            record: *record,
                            type_arguments: type_arguments
                                .iter()
                                .map(|ty| substitute_verification_type(ty, &instance.substitutions))
                                .collect(),
                            fields,
                        },
                    ));
                    instruction_spans.push(source_span);
                }
                Instruction::MakeVariant {
                    destination,
                    variant,
                    type_arguments,
                    payload,
                } => {
                    let destination = define_native_register(
                        *destination,
                        &register_types,
                        &mut state,
                        &mut value_types,
                        &mut instructions,
                    );
                    let payload = payload
                        .iter()
                        .map(|register| native_register(&state, *register, function))
                        .collect::<Result<Vec<_>, _>>()?;
                    instructions.push(ir::Instruction::Portable(
                        ir::PortableInstruction::MakeVariant {
                            destination,
                            variant: *variant,
                            type_arguments: type_arguments
                                .iter()
                                .map(|ty| substitute_verification_type(ty, &instance.substitutions))
                                .collect(),
                            payload,
                        },
                    ));
                    instruction_spans.push(source_span);
                }
                Instruction::MakeClosure {
                    destination,
                    function: target,
                    specialization,
                    captures,
                } => {
                    let captures = lower_capture_arguments(
                        captures,
                        function,
                        &inferred,
                        &mut state,
                        &mut value_types,
                        &mut instructions,
                    )?;
                    let destination = define_native_register(
                        *destination,
                        &register_types,
                        &mut state,
                        &mut value_types,
                        &mut instructions,
                    );
                    let specialization =
                        resolve_specialization(specialization, &instance.substitutions);
                    instructions.push(ir::Instruction::Portable(
                        ir::PortableInstruction::MakeClosure {
                            destination,
                            function: environment.instances[&SpecializationKey {
                                function: *target,
                                substitutions: specialization,
                            }],
                            specialization: Vec::new(),
                            captures: captures
                                .into_iter()
                                .map(|value| (crate::hir::CaptureMode::Move, value))
                                .collect(),
                        },
                    ));
                    instruction_spans.push(source_span);
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
                    let receiver_type = register_type(&inferred, *object, function)?;
                    let object = native_register(&state, *object, function)?;
                    let destination = define_native_register(
                        *destination,
                        &register_types,
                        &mut state,
                        &mut value_types,
                        &mut instructions,
                    );
                    if matches!(receiver_type, NativeType::Object(_)) {
                        instructions.push(ir::Instruction::Portable(
                            ir::PortableInstruction::LoadField {
                                destination,
                                object,
                                field: field.clone(),
                                by_reference: false,
                            },
                        ));
                    } else {
                        let helper = native_field_helper(receiver_type, field)?;
                        let arguments = vec![object];
                        instructions.push(ir::Instruction::RuntimeCall {
                            destination,
                            helper,
                            signature: runtime_signature(destination, &arguments, &value_types),
                            arguments,
                        });
                    }
                    instruction_spans.push(source_span);
                }
                Instruction::StoreField {
                    object,
                    field,
                    source,
                } => {
                    let object_type = register_type(&inferred, *object, function)?;
                    if !matches!(object_type, NativeType::Object(_)) {
                        return Err(native_error(format!(
                            "native field assignment requires a Foster object, found {object_type:?}"
                        )));
                    }
                    let old = native_register(&state, *object, function)?;
                    let source = native_register(&state, *source, function)?;
                    let unique = allocate_native_value(&mut value_types, object_type);
                    instructions.push(ir::Instruction::Portable(
                        ir::PortableInstruction::CopyOnWrite {
                            destination: unique,
                            source: old,
                        },
                    ));
                    state[usize::from(object.0)] = Some(unique);
                    instructions.push(ir::Instruction::Portable(
                        ir::PortableInstruction::StoreField {
                            object: unique,
                            field: field.clone(),
                            source,
                        },
                    ));
                    instruction_spans.push(source_span.clone());
                    instruction_spans.push(source_span);
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
                        &mut instructions,
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
                        &mut instructions,
                    );
                    instructions.push(ir::Instruction::RuntimeCall {
                        destination,
                        helper,
                        signature: runtime_signature(destination, &arguments, &value_types),
                        arguments,
                    });
                }
                Instruction::Jump { target } => {
                    terminator_span = source_span;
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
                    terminator_span = source_span;
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
                Instruction::MatchPattern {
                    destination,
                    subject,
                    pattern,
                    bindings,
                } => {
                    let subject = native_register(&state, *subject, function)?;
                    let destination = define_native_register(
                        *destination,
                        &register_types,
                        &mut state,
                        &mut value_types,
                        &mut instructions,
                    );
                    let bindings = bindings
                        .iter()
                        .map(|binding| {
                            define_native_register(
                                *binding,
                                &register_types,
                                &mut state,
                                &mut value_types,
                                &mut instructions,
                            )
                        })
                        .collect();
                    instructions.push(ir::Instruction::Portable(
                        ir::PortableInstruction::MatchPattern {
                            destination,
                            subject,
                            pattern: pattern.clone(),
                            bindings,
                        },
                    ));
                    instruction_spans.push(source_span);
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
                    specialization,
                    arguments,
                } => {
                    let lowered_arguments = lower_call_arguments(
                        arguments,
                        &environment.program.functions[callee].parameter_modes,
                        function,
                        &inferred,
                        &mut state,
                        &mut value_types,
                        &mut instructions,
                    )?;
                    let destination = define_native_register(
                        *destination,
                        &register_types,
                        &mut state,
                        &mut value_types,
                        &mut instructions,
                    );
                    instructions.push(ir::Instruction::Call {
                        destination,
                        function: environment.instances[&SpecializationKey {
                            function: *callee,
                            substitutions: resolve_specialization(
                                specialization,
                                &instance.substitutions,
                            ),
                        }],
                        specialization: Vec::new(),
                        arguments: lowered_arguments,
                    });
                }
                Instruction::CallMethod {
                    destination,
                    receiver,
                    function: callee,
                    specialization,
                    arguments,
                } => {
                    let mut registers = vec![*receiver];
                    registers.extend(arguments);
                    let lowered_arguments = lower_call_arguments(
                        &registers,
                        &environment.program.functions[callee].parameter_modes,
                        function,
                        &inferred,
                        &mut state,
                        &mut value_types,
                        &mut instructions,
                    )?;
                    let destination = define_native_register(
                        *destination,
                        &register_types,
                        &mut state,
                        &mut value_types,
                        &mut instructions,
                    );
                    instructions.push(ir::Instruction::Call {
                        destination,
                        function: environment.instances[&SpecializationKey {
                            function: *callee,
                            substitutions: resolve_specialization(
                                specialization,
                                &instance.substitutions,
                            ),
                        }],
                        specialization: Vec::new(),
                        arguments: lowered_arguments,
                    });
                }
                Instruction::CallClosure {
                    destination,
                    function: callee,
                    specialization,
                    captures,
                    arguments,
                } => {
                    let mut lowered_arguments = lower_capture_arguments(
                        captures,
                        function,
                        &inferred,
                        &mut state,
                        &mut value_types,
                        &mut instructions,
                    )?;
                    lowered_arguments.extend(lower_call_arguments(
                        arguments,
                        &environment.program.functions[callee].parameter_modes,
                        function,
                        &inferred,
                        &mut state,
                        &mut value_types,
                        &mut instructions,
                    )?);
                    let destination = define_native_register(
                        *destination,
                        &register_types,
                        &mut state,
                        &mut value_types,
                        &mut instructions,
                    );
                    instructions.push(ir::Instruction::Call {
                        destination,
                        function: environment.instances[&SpecializationKey {
                            function: *callee,
                            substitutions: resolve_specialization(
                                specialization,
                                &instance.substitutions,
                            ),
                        }],
                        specialization: Vec::new(),
                        arguments: lowered_arguments,
                    });
                }
                Instruction::CallValue {
                    destination,
                    callee,
                    arguments,
                } => {
                    let callee_type = register_type(&inferred, *callee, function)?;
                    let NativeType::Object(layout) = callee_type else {
                        return Err(native_error(format!(
                            "dynamic call in `{}` crosses an erased callable boundary",
                            function.name
                        )));
                    };
                    let LayoutKind::Closure {
                        function: target, ..
                    } = &environment.layouts.get(layout).kind
                    else {
                        return Err(native_error(format!(
                            "dynamic call in `{}` requires a concrete closure",
                            function.name
                        )));
                    };
                    let lowered_arguments = lower_call_arguments(
                        arguments,
                        &environment.program.functions[target].parameter_modes,
                        function,
                        &inferred,
                        &mut state,
                        &mut value_types,
                        &mut instructions,
                    )?;
                    let callee = native_register(&state, *callee, function)?;
                    let destination = define_native_register(
                        *destination,
                        &register_types,
                        &mut state,
                        &mut value_types,
                        &mut instructions,
                    );
                    instructions.push(ir::Instruction::Portable(
                        ir::PortableInstruction::CallValue {
                            destination,
                            callee,
                            arguments: lowered_arguments,
                        },
                    ));
                }
                Instruction::Return { source } => {
                    terminator_span = source_span;
                    let returned = native_register(&state, *source, function)?;
                    let mut released = HashSet::new();
                    for value in state.iter().flatten().copied() {
                        if value != returned
                            && released.insert(value)
                            && matches!(value_types[value.0 as usize], NativeType::Object(_))
                        {
                            instructions.push(ir::Instruction::Portable(
                                ir::PortableInstruction::Drop { value },
                            ));
                        }
                    }
                    terminator = Some(ir::Terminator::Return(returned));
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
        instruction_spans.resize(instructions.len(), std::ops::Range::default());
        blocks.push(ir::BlockData {
            parameters: block_parameters[block_index].clone(),
            instructions,
            instruction_spans,
            terminator,
            terminator_span,
        });
    }

    Ok(ir::Function {
        name: function.name.clone(),
        signature: function_signature.clone(),
        parameters: function_parameters,
        captures: Vec::new(),
        capture_types: Vec::new(),
        entry_seeds,
        entry: ir::Block(0),
        entry_arguments,
        storage_hints: vec![None; value_types.len()],
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
    instructions: &mut Vec<ir::Instruction>,
) -> ir::Value {
    let index = usize::from(register.0);
    if let Some(previous) = state[index]
        && matches!(value_types[previous.0 as usize], NativeType::Object(_))
    {
        instructions.push(ir::Instruction::Portable(ir::PortableInstruction::Drop {
            value: previous,
        }));
    }
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

fn lower_call_arguments(
    registers: &[Register],
    modes: &[ParameterMode],
    function: &BytecodeFunction,
    inferred: &[Option<NativeType>],
    state: &mut [Option<ir::Value>],
    value_types: &mut Vec<NativeType>,
    instructions: &mut Vec<ir::Instruction>,
) -> Result<Vec<ir::Value>, FosterError> {
    if registers.len() != modes.len() {
        return Err(native_error(format!(
            "call in `{}` has {} arguments but {} ownership modes",
            function.name,
            registers.len(),
            modes.len()
        )));
    }
    let mut arguments = Vec::with_capacity(registers.len());
    for (register, mode) in registers.iter().zip(modes) {
        let value = native_register(state, *register, function)?;
        let ty = register_type(inferred, *register, function)?;
        if *mode == ParameterMode::Borrow && matches!(ty, NativeType::Object(_)) {
            let retained = allocate_native_value(value_types, ty);
            instructions.push(ir::Instruction::Portable(ir::PortableInstruction::Move {
                destination: retained,
                source: value,
            }));
            arguments.push(retained);
        } else {
            arguments.push(value);
            if *mode == ParameterMode::Consume {
                state[usize::from(register.0)] = None;
            }
        }
    }
    Ok(arguments)
}

fn lower_capture_arguments(
    captures: &[(crate::hir::CaptureMode, Register)],
    function: &BytecodeFunction,
    inferred: &[Option<NativeType>],
    state: &mut [Option<ir::Value>],
    value_types: &mut Vec<NativeType>,
    instructions: &mut Vec<ir::Instruction>,
) -> Result<Vec<ir::Value>, FosterError> {
    let mut arguments = Vec::with_capacity(captures.len());
    for (mode, register) in captures {
        let value = native_register(state, *register, function)?;
        let ty = register_type(inferred, *register, function)?;
        match mode {
            crate::hir::CaptureMode::Move => {
                state[usize::from(register.0)] = None;
                arguments.push(value);
            }
            crate::hir::CaptureMode::Copy => {
                if matches!(ty, NativeType::Object(_)) {
                    let retained = allocate_native_value(value_types, ty);
                    instructions.push(ir::Instruction::Portable(ir::PortableInstruction::Move {
                        destination: retained,
                        source: value,
                    }));
                    arguments.push(retained);
                } else {
                    arguments.push(value);
                }
            }
            crate::hir::CaptureMode::Ref => {
                return Err(native_error(format!(
                    "native closure `{}` cannot yet capture a reference",
                    function.name
                )));
            }
            crate::hir::CaptureMode::Pending => {
                return Err(native_error(format!(
                    "native closure `{}` has an unresolved capture mode",
                    function.name
                )));
            }
        }
    }
    Ok(arguments)
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
    backend: &NativeBackend<'_>,
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
    for seed in &function.entry_seeds {
        let ty = function.value_type(*seed);
        let value = match ty {
            NativeType::Float => builder.ins().f64const(0.0),
            _ => builder.ins().iconst(cranelift_type(ty, pointer_type), 0),
        };
        values.insert(*seed, value);
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
                        may_bind_object: true,
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
                    values.insert(*binding, value);
                }
                continue;
            }
            let result =
                lower_native_instruction(builder, module, function, instruction, &values, backend)?;
            let destinations = instruction.destinations();
            if let Some(destination) = destinations.first() {
                values.insert(
                    *destination,
                    result.expect("value-producing native instruction"),
                );
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
        Pattern::Binding(_) => {
            if let NativeType::Object(layout) = subject.ty {
                if !subject.may_bind_object {
                    return Err(native_error(
                        "native patterns do not yet bind aggregate variant payloads",
                    ));
                }
                retain_object(builder, subject.value, layout, layouts.physical);
            }
            Ok((true_value(builder), vec![subject.value]))
        }
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
            let mut matched =
                builder
                    .ins()
                    .icmp_imm_s(IntCC::Equal, tag, i64::from(alternative.tag));
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
                        may_bind_object: false,
                    },
                    pattern,
                    layouts,
                )?;
                matched = builder.ins().band(matched, field_matched);
                bindings.append(&mut field_bindings);
            }
            Ok((matched, bindings))
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
            return lower_portable_native(builder, module, function, instruction, values, backend);
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
    backend: &NativeBackend<'_>,
) -> Result<Option<ClifValue>, FosterError> {
    let objects = backend.objects;
    let get = |value: &ir::Value| values[value];
    match instruction {
        ir::PortableInstruction::Drop { value } => {
            if let NativeType::Object(layout) = function.value_type(*value) {
                objects.release(builder, module, get(value), layout)?;
            }
            Ok(None)
        }
        ir::PortableInstruction::Move {
            destination,
            source,
        } => {
            let value = get(source);
            if let NativeType::Object(layout) = function.value_type(*destination) {
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
            let PhysicalKind::Record { fields } = &physical.kind else {
                return Err(native_error(
                    "native copy-on-write currently requires a record layout",
                ));
            };
            let source = get(source);
            let copied = objects.allocate(builder, module, layout)?;
            for field in fields {
                let value = load_physical_value(builder, module, source, field.offset, field.value);
                if let Some(pointee) = field.value.pointee
                    && objects.layouts.is_managed(pointee)
                {
                    objects.retain(builder, value, pointee);
                }
                store_physical_value(builder, copied, field.offset, value);
            }
            objects.release(builder, module, source, layout)?;
            Ok(Some(copied))
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
            let LayoutKind::Record { fields, .. } = &objects.layouts.logical.get(layout).kind
            else {
                return Err(native_error("native field load requires a record"));
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
        unsupported => Err(native_error(format!(
            "portable operation reached Cranelift without native legalization: {unsupported:?}"
        ))),
    }
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
        let mut program =
            vm::compile_with_options(&compilation, vm::CompileOptions { optimize: false }).unwrap();
        let mut layouts = crate::codegen::layout::legalize(&mut program).unwrap();
        let main = program.main.unwrap();
        let instances = reachable_instances(&program, main).unwrap();
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
        let function = lower_to_native_ir(
            &program.functions[&main],
            &function_types[&instance.ir_function],
            &instance.key,
            NativeIrEnvironment {
                program: &program,
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
