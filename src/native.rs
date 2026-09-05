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
    StackSlotData, StackSlotKind, Type as ClifType, Value as ClifValue, types,
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
    AlternativeLayout, DropField, DropPlan, PhysicalKind, PhysicalRegistry, ScalarKind,
    TargetLayout, ValueLayout, ValueSemantic,
};
use crate::codegen::layout::{LayoutId, LayoutKind, Registry as LayoutRegistry};
use crate::compiler::Compilation;
use crate::error::FosterError;
use crate::hir::{FunctionId, Pattern};
use crate::types::{Type, TypeId};
use crate::vm::{
    self, BytecodeFunction, Constant, Instruction, Program, Register, VerificationType,
};

pub mod abi;
mod emission;
mod text_boundary;
use emission::{emit_object, ordered_entries};
mod ownership;
mod program;
mod runtime;
pub use ownership::MemoryManagement;
use ownership::*;
pub use program::{LogicalSignature, NativeFunction, NativeProgram, prepare};
mod equality_runtime;
mod host_runtime;

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
    releases_result: bool,
}

/// Immutable inputs for specialization and representation legalization.
#[derive(Clone, Copy)]
struct NativeIrEnvironment<'a> {
    compilation: &'a Compilation,
    program: &'a Program,
    function_types: &'a HashMap<FunctionId, ir::Signature>,
    runtime_string_indices: &'a HashMap<u16, u64>,
    runtime_literal_indices: &'a HashMap<String, u64>,
    layouts: &'a LayoutRegistry,
    physical_layouts: &'a PhysicalRegistry,
    instances: &'a HashMap<SpecializationKey, FunctionId>,
    builtin_result_types: &'a HashMap<crate::intrinsics::Builtin, crate::vm::VerificationType>,
}

/// Shared immutable state for lowering one module's functions to Cranelift.
struct NativeBackend<'a> {
    ir: NativeIrEnvironment<'a>,
    functions: &'a HashMap<FunctionId, FuncId>,
    callable_thunks: &'a HashMap<LayoutId, FuncId>,
    remote_thunks: &'a HashMap<FunctionId, FuncId>,
    release_thunks: &'a HashMap<LayoutId, FuncId>,
    objects: ObjectRuntime<'a>,
}

#[derive(Clone, Copy)]
struct PatternSubject {
    value: ClifValue,
    ty: NativeType,
}

#[derive(Clone, Copy)]
struct NativeLowering<'a, 'backend> {
    function: &'a ir::Function,
    values: &'a HashMap<ir::Value, ClifValue>,
    homes: &'a HashMap<u16, StackSlot>,
    mutable_parameter_homes: &'a HashSet<u16>,
    backend: &'a NativeBackend<'backend>,
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

#[derive(Debug, Clone, Copy)]
struct ContractCandidate {
    layout: LayoutId,
    implementation: FunctionId,
    function: FunctionId,
}

/// Render the same verified program consumed by native object emission.
pub fn emit_ir(compilation: &Compilation) -> Result<String, FosterError> {
    Ok(prepare(compilation)?.emit_ir())
}

/// Prepare and compile the reachable portion of main to a host-native object.
pub fn compile_object(
    compilation: &Compilation,
    options: CompileOptions,
) -> Result<ObjectArtifact, FosterError> {
    prepare(compilation)?.compile_object(options)
}

/// Compile and link a standalone host executable using the installed Rust linker toolchain.
pub fn build_executable(
    compilation: &Compilation,
    output: impl AsRef<Path>,
    options: CompileOptions,
) -> Result<(), FosterError> {
    prepare(compilation)?.build_executable(output, options)
}

fn reachable_instances(
    compilation: &Compilation,
    program: &Program,
    shared_functions: &HashMap<FunctionId, ir::Function>,
    main: FunctionId,
) -> Result<Vec<NativeInstance>, FosterError> {
    let mut reachable = BTreeSet::new();
    let mut concrete_nominals = BTreeSet::new();
    let mut contract_calls = BTreeSet::new();
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
        for ty in body
            .parameter_types
            .iter()
            .chain(&body.capture_types)
            .chain(std::iter::once(&body.result_type))
            .map(|ty| ty.specialize(&instance.substitutions))
        {
            collect_nominal_types(&ty, &mut concrete_nominals);
        }
        for instruction in &body.instructions {
            if let Some(ty) = instruction_layout_type(program, instruction, &instance.substitutions)
            {
                collect_nominal_types(&ty, &mut concrete_nominals);
            }
        }
        let type_states = vm::type_states(program, body)?;
        for (index, instruction) in body.instructions.iter().enumerate() {
            let Some(state) = type_states[index].as_ref() else {
                continue;
            };
            match instruction {
                Instruction::CallContractMethod {
                    slot, arguments, ..
                } => {
                    let argument_types = arguments
                        .iter()
                        .map(|argument| {
                            state[usize::from(argument.0)]
                                .as_ref()
                                .map(|ty| ty.specialize(&instance.substitutions))
                                .ok_or_else(|| {
                                    native_error(format!(
                                        "contract argument r{} in `{}` has no verified type",
                                        argument.0, body.name
                                    ))
                                })
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    contract_calls.insert((*slot, argument_types));
                }
                Instruction::RemoteCall {
                    remote,
                    function,
                    arguments,
                    ..
                } => {
                    let receiver = state[usize::from(remote.0)]
                        .as_ref()
                        .map(|ty| ty.specialize(&instance.substitutions))
                        .ok_or_else(|| {
                            native_error(format!(
                                "remote receiver r{} in `{}` has no verified type",
                                remote.0, body.name
                            ))
                        })?;
                    let VerificationType::Remote(receiver) = receiver else {
                        return Err(native_error(format!(
                            "remote receiver in `{}` does not have a Remote type",
                            body.name
                        )));
                    };
                    let argument_types = arguments
                        .iter()
                        .map(|(_, argument)| {
                            state[usize::from(argument.0)]
                                .as_ref()
                                .map(|ty| ty.specialize(&instance.substitutions))
                                .ok_or_else(|| {
                                    native_error(format!(
                                        "remote argument r{} in `{}` has no verified type",
                                        argument.0, body.name
                                    ))
                                })
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    pending.push(SpecializationKey {
                        function: *function,
                        substitutions: remote_specialization(
                            compilation,
                            *function,
                            &receiver,
                            &argument_types,
                        )?,
                    });
                }
                _ => {}
            }
        }
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
        for ty in &concrete_nominals {
            let Some(nominal) = nominal_id(ty) else {
                continue;
            };
            for (slot, argument_types) in &contract_calls {
                let Some(target) = program.dispatch.get(&(nominal, *slot)).copied() else {
                    continue;
                };
                let target_body = &program.functions[&target];
                let mut substitutions = std::collections::BTreeMap::new();
                let hir_signature = compilation.types.function_type(target).ok_or_else(|| {
                    native_error(format!(
                        "contract implementation `{}` has no inferred signature",
                        target_body.name
                    ))
                })?;
                let parameter_types = hir_signature
                    .parameters
                    .iter()
                    .map(|ty| specialized_verification_type(compilation, *ty, &Vec::new(), 0))
                    .collect::<Result<Vec<_>, _>>()?;
                if let Some(receiver) = parameter_types.first() {
                    receiver.infer_specialization(ty, &mut substitutions);
                }
                for (parameter, argument) in parameter_types.iter().skip(1).zip(argument_types) {
                    parameter.infer_specialization(argument, &mut substitutions);
                }
                let substitutions = substitutions.into_iter().collect::<Vec<_>>();
                pending.push(SpecializationKey {
                    function: target,
                    substitutions,
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

fn nominal_id(ty: &crate::vm::VerificationType) -> Option<crate::types::NominalTypeId> {
    match ty {
        crate::vm::VerificationType::Record { record, .. } => {
            Some(crate::types::NominalTypeId::Record(*record))
        }
        crate::vm::VerificationType::Variant { variant, .. } => {
            Some(crate::types::NominalTypeId::Variant(*variant))
        }
        _ => None,
    }
}

fn collect_nominal_types(
    ty: &crate::vm::VerificationType,
    output: &mut BTreeSet<crate::vm::VerificationType>,
) {
    use crate::vm::VerificationType;
    match ty {
        VerificationType::Record { arguments, .. }
        | VerificationType::Variant { arguments, .. } => {
            if !ty.contains_generic() {
                output.insert(ty.clone());
            }
            for argument in arguments {
                collect_nominal_types(argument, output);
            }
        }
        VerificationType::List(value)
        | VerificationType::Reference(value)
        | VerificationType::Remote(value)
        | VerificationType::Future(value) => collect_nominal_types(value, output),
        VerificationType::Function {
            parameters, result, ..
        } => {
            for parameter in parameters {
                collect_nominal_types(parameter, output);
            }
            collect_nominal_types(result, output);
        }
        VerificationType::Union(values) => {
            for value in values {
                collect_nominal_types(value, output);
            }
        }
        VerificationType::Unknown
        | VerificationType::Generic(_)
        | VerificationType::Unit
        | VerificationType::Bool
        | VerificationType::Integer
        | VerificationType::Float
        | VerificationType::CodePoint
        | VerificationType::Byte
        | VerificationType::Bytes
        | VerificationType::ByteBuffer => {}
    }
}

fn remote_specialization(
    compilation: &Compilation,
    function: FunctionId,
    receiver: &VerificationType,
    arguments: &[VerificationType],
) -> Result<crate::vm::Specialization, FosterError> {
    let declaration = &compilation.hir.functions[function];
    let signature = compilation.types.function_type(function).ok_or_else(|| {
        native_error(format!(
            "remote method `{}` has no inferred signature",
            declaration.name
        ))
    })?;
    if signature.parameters.len() != arguments.len() + 1 {
        return Err(native_error(format!(
            "remote method `{}` has inconsistent parameter metadata",
            declaration.name
        )));
    }
    let schemas = signature
        .parameters
        .iter()
        .map(|ty| specialized_verification_type(compilation, *ty, &Vec::new(), 0))
        .collect::<Result<Vec<_>, _>>()?;
    let mut substitutions = std::collections::BTreeMap::new();
    schemas[0].infer_specialization(receiver, &mut substitutions);
    for (schema, argument) in schemas.iter().skip(1).zip(arguments) {
        schema.infer_specialization(argument, &mut substitutions);
    }
    for generic in &declaration.type_parameters {
        if !substitutions.contains_key(generic) {
            return Err(native_error(format!(
                "native remote call cannot infer `{generic}` for `{}`",
                declaration.name
            )));
        }
    }
    Ok(substitutions.into_iter().collect())
}

fn verification_type_for_native(
    ty: NativeType,
    program: &Program,
    layouts: &LayoutRegistry,
) -> VerificationType {
    match ty {
        NativeType::Unit => VerificationType::Unit,
        NativeType::Bool => VerificationType::Bool,
        NativeType::Int => VerificationType::Integer,
        NativeType::Float => VerificationType::Float,
        NativeType::CodePoint => VerificationType::CodePoint,
        NativeType::Byte => VerificationType::Byte,
        NativeType::String => program
            .string_record
            .map_or(VerificationType::Unknown, |record| {
                VerificationType::Record {
                    record,
                    arguments: Vec::new(),
                }
            }),
        NativeType::Object(layout) => match &layouts.get(layout).kind {
            LayoutKind::Record {
                record, arguments, ..
            } => VerificationType::Record {
                record: *record,
                arguments: arguments.clone(),
            },
            LayoutKind::Variant {
                variant_type,
                arguments,
                ..
            } => VerificationType::Variant {
                variant: *variant_type,
                arguments: arguments.clone(),
            },
            LayoutKind::Pointer { pointee, .. } => {
                VerificationType::Reference(Box::new(pointee.clone()))
            }
            LayoutKind::Builtin { ty } => ty.clone(),
            LayoutKind::Opaque | LayoutKind::Closure { .. } => VerificationType::Unknown,
        },
        NativeType::Opaque => VerificationType::Unknown,
    }
}

struct VerifiedRemoteCall {
    target: FunctionId,
    result: VerificationType,
}

fn verified_remote_calls(
    function: &BytecodeFunction,
    states: &[Option<Vec<Option<VerificationType>>>],
    instance: &SpecializationKey,
    environment: NativeIrEnvironment<'_>,
) -> Result<HashMap<u16, VerifiedRemoteCall>, FosterError> {
    let mut calls = HashMap::new();
    for (instruction, state) in function.instructions.iter().zip(states) {
        let Instruction::RemoteCall {
            destination,
            remote,
            function: target,
            arguments,
        } = instruction
        else {
            continue;
        };
        let Some(state) = state else {
            continue;
        };
        let logical_type = |register: Register| {
            state[usize::from(register.0)]
                .as_ref()
                .map(|ty| ty.specialize(&instance.substitutions))
                .ok_or_else(|| native_error("remote call operand has no verified type"))
        };
        let VerificationType::Remote(receiver) = logical_type(*remote)? else {
            return Err(native_error(
                "remote call receiver has no verified Remote type",
            ));
        };
        let arguments = arguments
            .iter()
            .map(|(_, argument)| logical_type(*argument))
            .collect::<Result<Vec<_>, _>>()?;
        let substitutions =
            remote_specialization(environment.compilation, *target, &receiver, &arguments)?;
        let result = environment.program.functions[target]
            .result_type
            .specialize(&substitutions);
        let key = SpecializationKey {
            function: *target,
            substitutions,
        };
        let target = environment.instances.get(&key).copied().ok_or_else(|| {
            native_error("verified remote specialization was not included in native reachability")
        })?;
        calls.insert(destination.0, VerifiedRemoteCall { target, result });
    }
    Ok(calls)
}

fn contract_candidates(
    slot: crate::types::DispatchSlot,
    receiver: NativeType,
    argument_types: &[NativeType],
    environment: NativeIrEnvironment<'_>,
) -> Result<Vec<ContractCandidate>, FosterError> {
    let receiver_layout = match receiver {
        NativeType::Object(layout) => layout,
        _ => return Ok(Vec::new()),
    };
    let receiver_nominal = match &environment.layouts.get(receiver_layout).kind {
        LayoutKind::Record { record, .. } => Some(crate::types::NominalTypeId::Record(*record)),
        LayoutKind::Variant { variant_type, .. } => {
            Some(crate::types::NominalTypeId::Variant(*variant_type))
        }
        _ => None,
    };
    let dynamic = matches!(
        environment.layouts.get(receiver_layout).kind,
        LayoutKind::Opaque
    ) || receiver_nominal
        .is_some_and(|nominal| !environment.program.dispatch.contains_key(&(nominal, slot)));
    let mut candidates = Vec::new();
    for layout in environment
        .layouts
        .layouts()
        .iter()
        .filter(|layout| layout.materialized)
    {
        if !dynamic && layout.id != receiver_layout {
            continue;
        }
        let concrete = match &layout.kind {
            LayoutKind::Record {
                record, arguments, ..
            } => crate::vm::VerificationType::Record {
                record: *record,
                arguments: arguments.clone(),
            },
            LayoutKind::Variant {
                variant_type,
                arguments,
                ..
            } => crate::vm::VerificationType::Variant {
                variant: *variant_type,
                arguments: arguments.clone(),
            },
            _ => continue,
        };
        let nominal = nominal_id(&concrete).expect("record and variant layouts are nominal");
        let Some(implementation) = environment.program.dispatch.get(&(nominal, slot)).copied()
        else {
            continue;
        };
        let mut matching = environment
            .instances
            .iter()
            .filter_map(|(key, function)| {
                if key.function != implementation {
                    return None;
                }
                let signature = &environment.function_types[function];
                (signature.parameters.first() == Some(&NativeType::Object(layout.id))
                    && signature.parameters.get(1..) == Some(argument_types))
                .then_some(*function)
            })
            .collect::<Vec<_>>();
        matching.sort_unstable_by_key(|function| function.into_raw().into_u32());
        let Some(function) = matching.into_iter().next() else {
            continue;
        };
        candidates.push(ContractCandidate {
            layout: layout.id,
            implementation,
            function,
        });
    }
    Ok(candidates)
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
    builtin_result_types: &HashMap<crate::intrinsics::Builtin, crate::vm::VerificationType>,
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
            for state in vm::type_states(program, &program.functions[&function])?
                .into_iter()
                .flatten()
            {
                for ty in state.into_iter().flatten() {
                    layouts.instantiate_type(&ty.specialize(&instance.key.substitutions))?;
                }
            }
            for instruction in &program.functions[&function].instructions {
                if let Some(ty) =
                    instruction_layout_type(program, instruction, &instance.key.substitutions)
                {
                    layouts.instantiate_type(&ty)?;
                }
                if let Instruction::Builtin { builtin, .. } = instruction
                    && builtin.descriptor().native == crate::intrinsics::NativeIntrinsic::Host
                {
                    let ty = builtin_result_types.get(builtin).ok_or_else(|| {
                        native_error(format!(
                            "native host intrinsic `{builtin:?}` has no declared result type"
                        ))
                    })?;
                    layouts.instantiate_type(ty)?;
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

fn native_builtin_result_types(
    compilation: &Compilation,
) -> Result<HashMap<crate::intrinsics::Builtin, crate::vm::VerificationType>, FosterError> {
    let mut result = HashMap::new();
    for (function, declaration) in compilation.hir.functions.iter() {
        let Some(builtin) = declaration
            .intrinsic
            .as_deref()
            .and_then(crate::intrinsics::Intrinsic::from_key)
            .and_then(crate::intrinsics::Intrinsic::builtin)
        else {
            continue;
        };
        if builtin.descriptor().native != crate::intrinsics::NativeIntrinsic::Host {
            continue;
        }
        let signature = compilation.types.function_type(function).ok_or_else(|| {
            native_error(format!(
                "native host intrinsic `{builtin:?}` is missing type information"
            ))
        })?;
        let ty = specialized_verification_type(compilation, signature.result, &Vec::new(), 0)?;
        result.insert(builtin, ty);
    }
    Ok(result)
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
        Type::Record { record, .. }
            if compilation.hir.records[record].name == "Symbol"
                && compilation.hir.modules[compilation.hir.records[record].module].name
                    == "core.symbol" =>
        {
            Ok(NativeType::String)
        }
        Type::Record { record, .. } if record_uses_dynamic_dispatch(compilation, record) => {
            Ok(NativeType::Object(layouts.opaque()))
        }
        Type::Record { record, .. }
            if compilation.hir.records[record].name == "Bytes"
                && compilation.hir.modules[compilation.hir.records[record].module].name
                    == "core.bytes" =>
        {
            let concrete = crate::vm::VerificationType::Bytes;
            layouts.instantiate_type(&concrete)?;
            layouts
                .builtin(&concrete)
                .map(NativeType::Object)
                .ok_or_else(|| native_error(format!("Bytes type in `{function}` has no layout")))
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
        } if compilation.hir.variant_types[variant].kind == crate::ast::VariantKind::Union => {
            let members = arguments
                .iter()
                .map(|ty| specialized_verification_type(compilation, *ty, substitutions, 0))
                .collect::<Result<Vec<_>, _>>()?;
            layouts.instantiate_type(&crate::vm::VerificationType::Union(members))?;
            Ok(NativeType::Object(layouts.opaque()))
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
        Type::Record { record, .. }
            if compilation.hir.records[*record].name == "Bytes"
                && compilation.hir.modules[compilation.hir.records[*record].module].name
                    == "core.bytes" =>
        {
            VerificationType::Bytes
        }
        Type::Record { record, arguments }
            if compilation.hir.records[*record].name == "List"
                && compilation.hir.modules[compilation.hir.records[*record].module].name
                    == "core.list"
                && arguments.len() == 1 =>
        {
            VerificationType::List(Box::new(nested(arguments[0])?))
        }
        Type::Record { record, arguments } => VerificationType::Record {
            record: *record,
            arguments: arguments
                .iter()
                .map(|ty| nested(*ty))
                .collect::<Result<_, _>>()?,
        },
        Type::Variant { variant, arguments }
            if compilation.hir.variant_types[*variant].kind == crate::ast::VariantKind::Union =>
        {
            VerificationType::Union(
                arguments
                    .iter()
                    .map(|ty| nested(*ty))
                    .collect::<Result<_, _>>()?,
            )
        }
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
        VerificationType::Record { record, .. }
            if record_uses_dynamic_dispatch(compilation, *record) =>
        {
            Ok(NativeType::Object(layouts.opaque()))
        }
        VerificationType::Record { record, .. }
            if compilation
                .types
                .record_names
                .get(record)
                .is_some_and(|name| name == "Symbol") =>
        {
            Ok(NativeType::String)
        }
        VerificationType::Record { record, .. }
            if compilation
                .types
                .record_names
                .get(record)
                .is_some_and(|name| name == "Bytes") =>
        {
            layouts.instantiate_type(&VerificationType::Bytes)?;
            layouts
                .builtin(&VerificationType::Bytes)
                .map(NativeType::Object)
                .ok_or_else(|| native_error(format!("Bytes type in `{function}` has no layout")))
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

fn record_uses_dynamic_dispatch(compilation: &Compilation, record: crate::hir::RecordId) -> bool {
    let declaration = &compilation.hir.records[record];
    // Private storage is nominal implementation state, so values of this exact type always keep
    // their concrete descriptor. Pure public structural surfaces may be erased when they have no
    // implementation of their own.
    if declaration.fields.iter().any(|field| !field.public) {
        return false;
    }
    let has_contract_surface =
        !declaration.methods.is_empty() || !declaration.compositions.is_empty();
    let has_implementation = compilation
        .types
        .dispatch
        .keys()
        .any(|(nominal, _)| *nominal == crate::types::NominalTypeId::Record(record));
    has_contract_surface && !has_implementation
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
                    | Instruction::SpawnRemote { .. }
                    | Instruction::SpawnRemoteBorrow { .. }
                    | Instruction::RemoteCall { .. }
                    | Instruction::Await { .. }
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
        }
    }
    let _ = compilation;
    Ok(())
}

fn concrete_closure_target(
    layouts: NativeLayouts<'_>,
    instances: &HashMap<SpecializationKey, FunctionId>,
    layout: LayoutId,
) -> Option<(FunctionId, usize)> {
    let LayoutKind::Closure {
        function,
        specialization,
        captures,
    } = &layouts.logical.get(layout).kind
    else {
        return None;
    };
    instances
        .get(&SpecializationKey {
            function: *function,
            substitutions: specialization.clone(),
        })
        .copied()
        .map(|target| (target, captures.len()))
}

fn declare_callable_thunks(
    module: &mut ObjectModule,
    layouts: NativeLayouts<'_>,
    instances: &HashMap<SpecializationKey, FunctionId>,
    function_types: &HashMap<FunctionId, ir::Signature>,
) -> Result<HashMap<LayoutId, FuncId>, FosterError> {
    let mut result = HashMap::new();
    for layout in layouts
        .physical
        .layouts()
        .iter()
        .filter(|layout| layout.materialized)
    {
        let Some((target, captures)) = concrete_closure_target(layouts, instances, layout.id)
        else {
            continue;
        };
        let target_signature = &function_types[&target];
        let mut parameters = vec![NativeType::Object(layout.id)];
        parameters.extend_from_slice(&target_signature.parameters[captures..]);
        let thunk_signature = signature(
            module,
            &ir::Signature {
                parameters,
                result: target_signature.result,
            },
        );
        let name = format!("foster_callable_l{}", layout.id.0);
        let id = module
            .declare_function(&name, Linkage::Local, &thunk_signature)
            .map_err(|error| native_error(format!("cannot declare `{name}`: {error}")))?;
        result.insert(layout.id, id);
    }
    Ok(result)
}

fn declare_remote_thunks(
    module: &mut ObjectModule,
    instances: &[NativeInstance],
    method_receivers: &HashSet<FunctionId>,
) -> Result<HashMap<FunctionId, FuncId>, FosterError> {
    let thunk_signature = signature(
        module,
        &ir::Signature {
            parameters: vec![NativeType::Int, NativeType::Opaque],
            result: NativeType::Int,
        },
    );
    instances
        .iter()
        .filter(|instance| method_receivers.contains(&instance.key.function))
        .map(|instance| {
            let name = format!(
                "foster_remote_{}",
                instance.ir_function.into_raw().into_u32()
            );
            let thunk = module
                .declare_function(&name, Linkage::Local, &thunk_signature)
                .map_err(|error| native_error(format!("cannot declare `{name}`: {error}")))?;
            Ok((instance.ir_function, thunk))
        })
        .collect()
}

fn define_function(
    module: &mut ObjectModule,
    prepared: &NativeFunction,
    native_id: FuncId,
    backend: &NativeBackend<'_>,
) -> Result<(), FosterError> {
    let instance = &prepared.instance;
    let function = &backend.ir.program.functions[&instance.key.function];
    let frontend_config = module.target_config();
    let mut context = module.make_context();
    context.func.signature = signature(module, &backend.ir.function_types[&instance.ir_function]);
    let mut builder_context = FunctionBuilderContext::new();
    {
        let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
        lower_native_ir(&mut builder, module, prepared, backend)?;
        builder.finalize(frontend_config);
    }
    module
        .define_function(native_id, &mut context)
        .map_err(|error| native_error(format!("cannot compile `{}`: {error}", function.name)))?;
    module.clear_context(&mut context);
    Ok(())
}

fn define_callable_thunks(
    module: &mut ObjectModule,
    backend: &NativeBackend<'_>,
) -> Result<(), FosterError> {
    for (layout, thunk_id) in ordered_entries(backend.callable_thunks) {
        let (target, capture_count) =
            concrete_closure_target(backend.objects.layouts, backend.ir.instances, layout)
                .ok_or_else(|| native_error("callable thunk has no concrete closure target"))?;
        let target_signature = &backend.ir.function_types[&target];
        let mut parameters = vec![NativeType::Object(layout)];
        parameters.extend_from_slice(&target_signature.parameters[capture_count..]);
        let thunk_signature = ir::Signature {
            parameters,
            result: target_signature.result,
        };
        let mut context = module.make_context();
        context.func.signature = signature(module, &thunk_signature);
        let frontend_config = module.target_config();
        let mut builder_context = FunctionBuilderContext::new();
        {
            let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
            let entry = builder.create_block();
            builder.append_block_params_for_function_params(entry);
            builder.switch_to_block(entry);
            let inputs = builder.block_params(entry).to_vec();
            let environment = inputs[0];
            let physical = backend.objects.layouts.physical.get(layout);
            let PhysicalKind::Closure { captures, .. } = &physical.kind else {
                return Err(native_error("callable thunk environment is not a closure"));
            };
            let mut arguments = Vec::with_capacity(captures.len() + inputs.len() - 1);
            for field in captures {
                let value = load_physical_value(
                    &mut builder,
                    module,
                    environment,
                    field.offset,
                    field.value,
                );
                if let Some(pointee) = field.value.pointee
                    && backend.objects.layouts.is_managed(pointee)
                {
                    backend.objects.retain(&mut builder, value, pointee);
                }
                arguments.push(value);
            }
            arguments.extend_from_slice(&inputs[1..]);
            let target = module.declare_func_in_func(backend.functions[&target], builder.func);
            let call = builder.ins().call(target, &arguments);
            let results = builder.inst_results(call).to_vec();
            builder.ins().return_(&results);
            builder.seal_all_blocks();
            builder.finalize(frontend_config);
        }
        module
            .define_function(thunk_id, &mut context)
            .map_err(|error| {
                native_error(format!(
                    "cannot compile callable thunk l{}: {error}",
                    layout.0
                ))
            })?;
        module.clear_context(&mut context);
    }
    Ok(())
}

fn native_to_remote_word(
    builder: &mut FunctionBuilder<'_>,
    module: &ObjectModule,
    value: ClifValue,
    ty: NativeType,
) -> ClifValue {
    match ty {
        NativeType::Int => value,
        NativeType::Float => builder
            .ins()
            .bitcast(types::I64, MemFlagsData::new(), value),
        NativeType::Unit | NativeType::Bool | NativeType::Byte => {
            builder.ins().uextend(types::I64, value)
        }
        NativeType::CodePoint => builder.ins().uextend(types::I64, value),
        NativeType::String | NativeType::Object(_) | NativeType::Opaque => {
            if module.target_config().pointer_type() == types::I64 {
                value
            } else {
                builder.ins().uextend(types::I64, value)
            }
        }
    }
}

fn remote_word_to_native(
    builder: &mut FunctionBuilder<'_>,
    module: &ObjectModule,
    value: ClifValue,
    ty: NativeType,
) -> ClifValue {
    match ty {
        NativeType::Int => value,
        NativeType::Float => builder
            .ins()
            .bitcast(types::F64, MemFlagsData::new(), value),
        NativeType::Unit | NativeType::Bool | NativeType::Byte => {
            builder.ins().ireduce(types::I8, value)
        }
        NativeType::CodePoint => builder.ins().ireduce(types::I32, value),
        NativeType::String | NativeType::Object(_) | NativeType::Opaque => {
            if module.target_config().pointer_type() == types::I64 {
                value
            } else {
                builder
                    .ins()
                    .ireduce(module.target_config().pointer_type(), value)
            }
        }
    }
}

fn define_remote_thunks(
    module: &mut ObjectModule,
    backend: &NativeBackend<'_>,
) -> Result<(), FosterError> {
    for (target, thunk_id) in ordered_entries(backend.remote_thunks) {
        let target_signature = &backend.ir.function_types[&target];
        let Some(&receiver_type) = target_signature.parameters.first() else {
            return Err(native_error("remote method has no receiver parameter"));
        };
        let mut context = module.make_context();
        context.func.signature = signature(
            module,
            &ir::Signature {
                parameters: vec![NativeType::Int, NativeType::Opaque],
                result: NativeType::Int,
            },
        );
        let frontend_config = module.target_config();
        let mut builder_context = FunctionBuilderContext::new();
        {
            let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
            let entry = builder.create_block();
            builder.append_block_params_for_function_params(entry);
            builder.switch_to_block(entry);
            let state_word = builder.block_params(entry)[0];
            let state = remote_word_to_native(&mut builder, module, state_word, receiver_type);
            if let Some(layout) = backend.objects.layouts.managed_layout(receiver_type) {
                backend.objects.retain(&mut builder, state, layout);
            }
            let argument_data = builder.block_params(entry)[1];
            let mut arguments = Vec::with_capacity(target_signature.parameters.len());
            arguments.push(state);
            for (index, ty) in target_signature
                .parameters
                .iter()
                .copied()
                .skip(1)
                .enumerate()
            {
                let word = builder.ins().load(
                    types::I64,
                    MemFlagsData::trusted(),
                    argument_data,
                    i32::try_from(index * 8)
                        .map_err(|_| native_error("remote argument frame exceeds i32 offsets"))?,
                );
                arguments.push(remote_word_to_native(&mut builder, module, word, ty));
            }
            let target = module.declare_func_in_func(backend.functions[&target], builder.func);
            let call = builder.ins().call(target, &arguments);
            let result = builder.inst_results(call)[0];
            let result =
                native_to_remote_word(&mut builder, module, result, target_signature.result);
            builder.ins().return_(&[result]);
            builder.seal_all_blocks();
            builder.finalize(frontend_config);
        }
        module
            .define_function(thunk_id, &mut context)
            .map_err(|error| {
                native_error(format!(
                    "cannot compile remote thunk for function #{}: {error}",
                    target.into_raw().into_u32()
                ))
            })?;
        module.clear_context(&mut context);
    }
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
    cranelift_representation(ty.representation(), pointer_type)
}

fn cranelift_representation(
    representation: ir::Representation,
    pointer_type: ClifType,
) -> ClifType {
    match representation {
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
    remote_calls: &HashMap<u16, VerifiedRemoteCall>,
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
                        Constant::Symbol(_) => NativeType::String,
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
                let result_type = match &environment.layouts.get(layout).kind {
                    LayoutKind::Closure {
                        function: target,
                        specialization,
                        ..
                    } => {
                        let target = environment.instances[&SpecializationKey {
                            function: *target,
                            substitutions: specialization.clone(),
                        }];
                        environment.function_types[&target].result
                    }
                    LayoutKind::Builtin {
                        ty: crate::vm::VerificationType::Function { result, .. },
                    } => native_verification_type(
                        environment.program,
                        environment.layouts,
                        result,
                        None,
                    )?,
                    _ => {
                        return Err(native_error(format!(
                            "dynamic call in `{}` does not reference a callable layout",
                            function.name
                        )));
                    }
                };
                result[usize::from(destination.0)] = Some(result_type);
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
                by_reference,
            } => {
                let object = dereference_native_type(
                    register_type(&result, *object, function)?,
                    environment,
                )?;
                if *by_reference {
                    let NativeType::Object(layout) = object else {
                        return Err(native_error("projected field requires a record"));
                    };
                    let LayoutKind::Record { fields, .. } = &environment.layouts.get(layout).kind
                    else {
                        return Err(native_error("projected field requires a record layout"));
                    };
                    let slot = fields
                        .iter()
                        .find(|slot| slot.name == *field)
                        .ok_or_else(|| native_error("projected field has no logical slot"))?;
                    let pointer = environment
                        .layouts
                        .pointer(&slot.ty, crate::codegen::layout::Ownership::Borrowed)
                        .ok_or_else(|| {
                            native_error("projected field has no borrowed pointer layout")
                        })?;
                    result[usize::from(destination.0)] = Some(NativeType::Object(pointer));
                    continue;
                }
                result[usize::from(destination.0)] = Some(
                    field_type(
                        environment.program,
                        environment.layouts,
                        environment.physical_layouts,
                        object,
                        field,
                    )
                    .map_err(|error| {
                        native_error(format!(
                            "{} while lowering field `{field}` in `{}`",
                            error.message, function.name
                        ))
                    })?,
                );
            }
            Instruction::Index {
                destination,
                object,
                ..
            } => {
                let object = register_type(&result, *object, function)?;
                result[usize::from(destination.0)] = Some(match object {
                    NativeType::String => NativeType::CodePoint,
                    NativeType::Object(layout) => {
                        match &environment.physical_layouts.get(layout).kind {
                            PhysicalKind::Buffer { element, .. } => {
                                match &environment.layouts.get(layout).kind {
                                    LayoutKind::Builtin {
                                        ty: VerificationType::List(item),
                                    } => native_verification_type(
                                        environment.program,
                                        environment.layouts,
                                        item,
                                        element.pointee,
                                    )?,
                                    _ => native_type_from_value_layout(*element),
                                }
                            }
                            PhysicalKind::Bytes { .. } => NativeType::Byte,
                            _ => {
                                return Err(native_error(format!(
                                    "native indexing requires bytes or a buffer in `{}`",
                                    function.name
                                )));
                            }
                        }
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
                result[usize::from(destination.0)] =
                    Some(native_intrinsic_result_type(*builtin, environment)?);
            }
            Instruction::SpawnRemote { destination, value }
            | Instruction::SpawnRemoteBorrow {
                destination,
                source: value,
            } => {
                let value = register_type(&result, *value, function)?;
                let remote = VerificationType::Remote(Box::new(verification_type_for_native(
                    value,
                    environment.program,
                    environment.layouts,
                )));
                let layout = environment.layouts.builtin(&remote).ok_or_else(|| {
                    native_error(format!(
                        "remote value in `{}` has no concrete native layout",
                        function.name
                    ))
                })?;
                result[usize::from(destination.0)] = Some(NativeType::Object(layout));
            }
            Instruction::RemoteCall { destination, .. } => {
                let call = remote_calls
                    .get(&destination.0)
                    .ok_or_else(|| native_error("remote call has no verified specialization"))?;
                let future = VerificationType::Future(Box::new(call.result.clone()));
                let layout = environment.layouts.builtin(&future).ok_or_else(|| {
                    native_error(format!(
                        "future in `{}` has no concrete native layout",
                        function.name
                    ))
                })?;
                result[usize::from(destination.0)] = Some(NativeType::Object(layout));
            }
            Instruction::Await {
                destination,
                future,
            } => {
                let NativeType::Object(layout) = register_type(&result, *future, function)? else {
                    return Err(native_error(format!(
                        "await in `{}` has a non-object future",
                        function.name
                    )));
                };
                let LayoutKind::Builtin {
                    ty: VerificationType::Future(value),
                } = &environment.layouts.get(layout).kind
                else {
                    return Err(native_error(format!(
                        "await in `{}` does not receive Future<T>",
                        function.name
                    )));
                };
                result[usize::from(destination.0)] = Some(native_verification_type(
                    environment.program,
                    environment.layouts,
                    value,
                    None,
                )?);
            }
            Instruction::CallContractMethod {
                destination,
                receiver,
                slot,
                name,
                arguments,
                ..
            } => {
                let receiver = register_type(&result, *receiver, function)?;
                let argument_types = arguments
                    .iter()
                    .map(|argument| register_type(&result, *argument, function))
                    .collect::<Result<Vec<_>, _>>()?;
                let candidates =
                    contract_candidates(*slot, receiver, &argument_types, environment)?;
                if let Some(first) = candidates.first() {
                    let signature = &environment.function_types[&first.function];
                    if signature.parameters.len() != arguments.len() + 1 {
                        return Err(native_error(format!(
                            "contract implementation for `{name}` has an inconsistent arity"
                        )));
                    }
                    if candidates.iter().any(|candidate| {
                        environment.function_types[&candidate.function].result != signature.result
                    }) {
                        return Err(native_error(format!(
                            "contract implementations for `{name}` disagree on their native result ABI"
                        )));
                    }
                    result[usize::from(destination.0)] = Some(signature.result);
                } else {
                    if !arguments.is_empty() {
                        return Err(native_error(format!(
                            "value has no native implementation of required method `{name}`"
                        )));
                    }
                    result[usize::from(destination.0)] = Some(
                        field_type(
                            environment.program,
                            environment.layouts,
                            environment.physical_layouts,
                            receiver,
                            name,
                        )
                        .map_err(|error| {
                            native_error(format!(
                                "{} while resolving contract `{name}` in `{}`",
                                error.message, function.name
                            ))
                        })?,
                    );
                }
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
                ..
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
        (NativeType::String, "empty?") => Ok(NativeType::Bool),
        (NativeType::String, "length") => Ok(NativeType::Int),
        (NativeType::String, "head") => Ok(NativeType::CodePoint),
        (NativeType::String, "rest") => Ok(NativeType::String),
        (NativeType::String, "whitespace?") => Ok(NativeType::Bool),
        (NativeType::String, "bytes" | "value") => layouts
            .builtin(&crate::vm::VerificationType::Bytes)
            .map(NativeType::Object)
            .ok_or_else(|| native_error("String byte storage has no native layout")),
        (NativeType::Byte, "int") => Ok(NativeType::Int),
        (NativeType::CodePoint, "whitespace?") => Ok(NativeType::Bool),
        (NativeType::CodePoint, "string") => Ok(NativeType::String),
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
            LayoutKind::Builtin {
                ty: crate::vm::VerificationType::Bytes,
            } => match field {
                "empty?" => Ok(NativeType::Bool),
                "length" => Ok(NativeType::Int),
                "head" => Ok(NativeType::Byte),
                "rest" => Ok(NativeType::Object(layout)),
                _ => Err(native_error(format!("native Bytes has no field `{field}`"))),
            },
            LayoutKind::Builtin {
                ty: crate::vm::VerificationType::ByteBuffer,
            } => match field {
                "empty?" => Ok(NativeType::Bool),
                "length" | "capacity" => Ok(NativeType::Int),
                _ => Err(native_error(format!(
                    "native ByteBuffer has no field `{field}`"
                ))),
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
        VerificationType::Record { record, .. } if Some(*record) == program.symbol_record => {
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

fn native_intrinsic_result_type(
    builtin: crate::intrinsics::Builtin,
    environment: NativeIrEnvironment<'_>,
) -> Result<NativeType, FosterError> {
    if builtin.descriptor().signature.result != crate::intrinsics::IntrinsicType::Any {
        return native_intrinsic_type(builtin.descriptor().signature.result, environment.layouts);
    }
    let ty = environment
        .builtin_result_types
        .get(&builtin)
        .ok_or_else(|| {
            native_error(format!(
                "native intrinsic `{builtin:?}` has no concrete result type"
            ))
        })?;
    native_verification_type(environment.program, environment.layouts, ty, None)
}

fn lower_shared_to_native_ir(
    shared: &ir::Function,
    metadata: &BytecodeFunction,
    source_states: &[Option<Vec<Option<VerificationType>>>],
    function_signature: &ir::Signature,
    instance: &SpecializationKey,
    environment: NativeIrEnvironment<'_>,
) -> Result<ir::Function, FosterError> {
    let remote_calls = verified_remote_calls(metadata, source_states, instance, environment)?;
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
        &remote_calls,
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
    let mut cleanup_edges = Vec::new();

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
                NativeFunctionFacts {
                    reference_homes: &reference_homes,
                    remote_calls: &remote_calls,
                },
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
        let mut terminator = block.terminator.clone();
        if let ir::Terminator::Return(returned) = &terminator {
            let returned = *returned;
            if let Some(conversion) = erased_conversion(
                value_types[returned.0 as usize],
                function_signature.result,
                environment.layouts,
            ) {
                let converted = allocate_shared_value(
                    &mut value_types,
                    &mut storage_hints,
                    function_signature.result,
                );
                instructions.push(match conversion {
                    ErasedConversion::Box => ir::Instruction::BoxValue {
                        destination: converted,
                        source: returned,
                    },
                    ErasedConversion::Unbox => ir::Instruction::UnboxValue {
                        destination: converted,
                        source: returned,
                    },
                });
                spans.push(block.terminator_span.clone());
                terminator = ir::Terminator::Return(converted);
            } else if result_error_conversion(
                value_types[returned.0 as usize],
                function_signature.result,
                environment.layouts,
            ) {
                let converted = allocate_shared_value(
                    &mut value_types,
                    &mut storage_hints,
                    function_signature.result,
                );
                instructions.push(ir::Instruction::ConvertResultError {
                    destination: converted,
                    source: returned,
                });
                spans.push(block.terminator_span.clone());
                instructions.push(ir::Instruction::Portable(ir::PortableInstruction::Drop {
                    value: returned,
                }));
                spans.push(block.terminator_span.clone());
                remove_shared_home(&mut state, &storage_hints, returned);
                terminator = ir::Terminator::Return(converted);
            }
            let ir::Terminator::Return(returned) = &terminator else {
                unreachable!()
            };
            let returned = *returned;
            for value in state.values().copied().collect::<BTreeSet<_>>() {
                if value != returned
                    && matches!(
                        value_types[value.0 as usize],
                        NativeType::Object(_) | NativeType::String
                    )
                {
                    instructions.push(ir::Instruction::Portable(ir::PortableInstruction::Drop {
                        value,
                    }));
                    spans.push(block.terminator_span.clone());
                }
            }
        }
        // Pruned SSA block arguments no longer carry dead storage into the successor.
        // Release that storage on the particular edge where its lifetime ends.
        let owned = state.values().copied().collect::<BTreeSet<_>>();
        let dying = |arguments: &[ir::Value]| {
            owned
                .iter()
                .copied()
                .filter(|value| !arguments.contains(value))
                .filter(|value| {
                    matches!(
                        value_types[value.0 as usize],
                        NativeType::Object(_) | NativeType::String
                    )
                })
                .map(|value| ir::Instruction::Portable(ir::PortableInstruction::Drop { value }))
                .collect::<Vec<_>>()
        };
        match &mut terminator {
            ir::Terminator::Jump { arguments, .. } => {
                for drop in dying(arguments) {
                    instructions.push(drop);
                    spans.push(block.terminator_span.clone());
                }
            }
            ir::Terminator::Branch {
                then_target,
                then_arguments,
                else_target,
                else_arguments,
                ..
            } => {
                for (target, arguments) in
                    [(then_target, then_arguments), (else_target, else_arguments)]
                {
                    let drops = dying(arguments);
                    if drops.is_empty() {
                        continue;
                    }
                    let edge = ir::Block((shared.blocks.len() + cleanup_edges.len()) as u32);
                    cleanup_edges.push(ir::BlockData {
                        parameters: Vec::new(),
                        instruction_spans: vec![block.terminator_span.clone(); drops.len()],
                        instructions: drops,
                        terminator: ir::Terminator::Jump {
                            target: *target,
                            arguments: arguments.clone(),
                        },
                        terminator_span: block.terminator_span.clone(),
                    });
                    *target = edge;
                    arguments.clear();
                }
            }
            ir::Terminator::Return(_) => {}
        }
        blocks.push(ir::BlockData {
            parameters: block.parameters.clone(),
            instructions,
            instruction_spans: spans,
            terminator,
            terminator_span: block.terminator_span.clone(),
        });
    }
    blocks.extend(cleanup_edges);

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

fn result_error_conversion(
    source: NativeType,
    target: NativeType,
    layouts: &LayoutRegistry,
) -> bool {
    let (NativeType::Object(source), NativeType::Object(target)) = (source, target) else {
        return false;
    };
    if source == target {
        return false;
    }
    let LayoutKind::Variant {
        name: source_name,
        alternatives: source_alternatives,
        ..
    } = &layouts.get(source).kind
    else {
        return false;
    };
    let LayoutKind::Variant {
        name: target_name,
        alternatives: target_alternatives,
        ..
    } = &layouts.get(target).kind
    else {
        return false;
    };
    if source_name != "Result" || target_name != "Result" {
        return false;
    }
    let Some(source_error) = source_alternatives
        .iter()
        .find(|alternative| alternative.name == "Error")
    else {
        return false;
    };
    let Some(target_error) = target_alternatives
        .iter()
        .find(|alternative| alternative.name == "Error")
    else {
        return false;
    };
    source_error.payload == target_error.payload
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

struct NativeFunctionFacts<'a> {
    reference_homes: &'a HashMap<u16, ir::Value>,
    remote_calls: &'a HashMap<u16, VerifiedRemoteCall>,
}

fn lower_shared_instruction(
    instruction: &ir::Instruction,
    metadata: &BytecodeFunction,
    instance: &SpecializationKey,
    environment: NativeIrEnvironment<'_>,
    value_types: &mut Vec<NativeType>,
    storage_hints: &mut Vec<Option<u16>>,
    facts: NativeFunctionFacts<'_>,
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
                    ir::Constant::RuntimeString(environment.runtime_string_indices[constant])
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
                .and_then(|home| facts.reference_homes.get(&home).copied())
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
            let target = environment.instances[&SpecializationKey {
                function: *function,
                substitutions: resolve_specialization(specialization, &instance.substitutions),
            }];
            let (arguments, consumed) = shared_call_arguments(
                arguments,
                &environment.program.functions[function].parameter_modes,
                &environment.function_types[&target].parameters,
                environment,
                value_types,
                storage_hints,
                &mut result,
            )?;
            Ok({
                result.push((
                    ir::Instruction::Call {
                        destination: *destination,
                        function: target,
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
            let target = environment.instances[&SpecializationKey {
                function: *function,
                substitutions: resolve_specialization(specialization, &instance.substitutions),
            }];
            let (arguments, consumed) = shared_call_arguments(
                &sources,
                &environment.program.functions[function].parameter_modes,
                &environment.function_types[&target].parameters,
                environment,
                value_types,
                storage_hints,
                &mut result,
            )?;
            result.push((
                ir::Instruction::Call {
                    destination: *destination,
                    function: target,
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
                environment.layouts,
                value_types,
                storage_hints,
                &mut result,
                &metadata.name,
            )?;
            let (ordinary, ordinary_consumed) = shared_call_arguments(
                arguments,
                &environment.program.functions[function].parameter_modes,
                &environment.function_types[&target].parameters[captures.len()..],
                environment,
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
                environment.layouts,
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
            let mut result = Vec::new();
            let (modes, expected) = match &environment.layouts.get(layout).kind {
                LayoutKind::Closure {
                    function: target,
                    specialization,
                    captures,
                } => {
                    let target_instance = environment.instances[&SpecializationKey {
                        function: *target,
                        substitutions: specialization.clone(),
                    }];
                    (
                        environment.program.functions[target]
                            .parameter_modes
                            .as_slice(),
                        environment.function_types[&target_instance].parameters[captures.len()..]
                            .to_vec(),
                    )
                }
                LayoutKind::Builtin {
                    ty:
                        crate::vm::VerificationType::Function {
                            parameters,
                            parameter_modes,
                            ..
                        },
                } => (
                    parameter_modes.as_slice(),
                    parameters
                        .iter()
                        .map(|parameter| {
                            native_verification_type(
                                environment.program,
                                environment.layouts,
                                parameter,
                                None,
                            )
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                ),
                _ => {
                    return Err(native_error(format!(
                        "dynamic call in `{}` requires a callable layout",
                        metadata.name
                    )));
                }
            };
            let (arguments, consumed) = shared_call_arguments(
                arguments,
                modes,
                &expected,
                environment,
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
                NativeType::String => Some(abi::STRING_GET),
                NativeType::Object(layout)
                    if matches!(
                        environment.physical_layouts.get(layout).kind,
                        PhysicalKind::Buffer { .. } | PhysicalKind::Bytes { .. }
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
            if ty(*object) == NativeType::String && matches!(field.as_str(), "bytes" | "value") {
                let bytes = environment
                    .layouts
                    .builtin(&crate::vm::VerificationType::Bytes)
                    .ok_or_else(|| native_error("String byte storage has no native layout"))?;
                value_types[destination.0 as usize] = NativeType::Object(bytes);
                return Ok(one(ir::Instruction::StringToBytes {
                    destination: *destination,
                    source: *object,
                }));
            }
            if ty(*object) == NativeType::Byte && field == "int" {
                return Ok(one(ir::Instruction::IntegerExtend {
                    destination: *destination,
                    operand: *object,
                }));
            }
            if ty(*object) == NativeType::CodePoint && field == "whitespace?" {
                return Ok(one(ir::Instruction::RuntimeCall {
                    destination: *destination,
                    helper: abi::CODE_POINT_WHITESPACE,
                    signature: ir::Signature {
                        parameters: vec![NativeType::CodePoint],
                        result: NativeType::Bool,
                    },
                    arguments: vec![*object],
                }));
            }
            if ty(*object) == NativeType::CodePoint && field == "string" {
                return Ok(one(ir::Instruction::RuntimeCall {
                    destination: *destination,
                    helper: abi::CODE_POINT_STRING,
                    signature: ir::Signature {
                        parameters: vec![NativeType::CodePoint],
                        result: NativeType::String,
                    },
                    arguments: vec![*object],
                }));
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
            slot,
            name,
            arguments,
            ..
        } => {
            let argument_types = arguments
                .iter()
                .map(|argument| ty(*argument))
                .collect::<Vec<_>>();
            let candidates =
                contract_candidates(*slot, ty(*receiver), &argument_types, environment)?;
            if let Some(first) = candidates.first() {
                let target = &environment.function_types[&first.function];
                let modes = &environment.program.functions[&first.implementation].parameter_modes;
                if target.parameters.len() != arguments.len() + 1
                    || modes.len() != arguments.len() + 1
                {
                    return Err(native_error(format!(
                        "contract implementation for `{name}` has an inconsistent arity"
                    )));
                }
                let mut lowered = Vec::new();
                let (arguments, consumed) = shared_call_arguments(
                    arguments,
                    &modes[1..],
                    &target.parameters[1..],
                    environment,
                    value_types,
                    storage_hints,
                    &mut lowered,
                )?;
                lowered.push((
                    ir::Instruction::Portable(ir::PortableInstruction::CallContractMethod {
                        destination: *destination,
                        receiver: *receiver,
                        slot: *slot,
                        name: name.clone(),
                        arguments,
                    }),
                    consumed,
                ));
                return Ok(lowered);
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
        ir::PortableInstruction::Builtin {
            destination,
            builtin,
            ..
        } => {
            value_types[destination.0 as usize] =
                native_intrinsic_result_type(*builtin, environment)?;
            Ok(one(instruction.clone()))
        }
        ir::PortableInstruction::SpawnRemote { value, .. } => {
            Ok(vec![(instruction.clone(), vec![*value])])
        }
        ir::PortableInstruction::SpawnRemoteBorrow { source, .. } => {
            if !matches!(ty(*source), NativeType::Object(_)) {
                return Err(native_error(format!(
                    "native borrowed remote state in `{}` must use an object layout",
                    metadata.name
                ))
                .with_help("wrap scalar state in a Foster record before borrowing it remotely"));
            }
            Ok(one(instruction.clone()))
        }
        ir::PortableInstruction::RemoteCall {
            destination,
            remote,
            arguments,
            ..
        } => {
            let target = storage_hints[destination.0 as usize]
                .and_then(|home| facts.remote_calls.get(&home))
                .ok_or_else(|| native_error("remote SSA call has no verified specialization"))?
                .target;
            let modes = arguments.iter().map(|(mode, _)| *mode).collect::<Vec<_>>();
            let sources = arguments
                .iter()
                .map(|(_, value)| *value)
                .collect::<Vec<_>>();
            let mut result = Vec::new();
            let (lowered, consumed) = shared_call_arguments(
                &sources,
                &modes,
                &environment.function_types[&target].parameters[1..],
                environment,
                value_types,
                storage_hints,
                &mut result,
            )?;
            result.push((
                ir::Instruction::Portable(ir::PortableInstruction::RemoteCall {
                    destination: *destination,
                    remote: *remote,
                    function: target,
                    arguments: modes.into_iter().zip(lowered).collect(),
                }),
                consumed,
            ));
            Ok(result)
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
    expected_types: &[NativeType],
    environment: NativeIrEnvironment<'_>,
    value_types: &mut Vec<NativeType>,
    storage_hints: &mut Vec<Option<u16>>,
    instructions: &mut Vec<(ir::Instruction, Vec<ir::Value>)>,
) -> Result<(Vec<ir::Value>, Vec<ir::Value>), FosterError> {
    if arguments.len() != modes.len() || arguments.len() != expected_types.len() {
        return Err(native_error(
            "shared call ownership metadata has the wrong arity",
        ));
    }
    let layouts = environment.layouts;
    let mut lowered = Vec::with_capacity(arguments.len());
    let mut consumed = Vec::new();
    for ((argument, mode), expected) in arguments.iter().zip(modes).zip(expected_types) {
        let source_type = value_types[argument.0 as usize];
        let pointee_type = dereference_native_type(source_type, environment)?;
        let loaded;
        let argument = if *mode == ParameterMode::Borrow
            && source_type != pointee_type
            && source_type != *expected
        {
            loaded = allocate_shared_value(value_types, storage_hints, pointee_type);
            instructions.push((
                ir::Instruction::RuntimeCall {
                    destination: loaded,
                    helper: reference_load_helper(pointee_type),
                    signature: ir::Signature {
                        parameters: vec![source_type],
                        result: pointee_type,
                    },
                    arguments: vec![*argument],
                },
                Vec::new(),
            ));
            &loaded
        } else {
            argument
        };
        let ty = value_types[argument.0 as usize];
        if callable_conversion(ty, *expected, layouts) {
            let callable = allocate_shared_value(value_types, storage_hints, *expected);
            instructions.push((
                ir::Instruction::WrapCallable {
                    destination: callable,
                    source: *argument,
                },
                Vec::new(),
            ));
            lowered.push(callable);
            if *mode == ParameterMode::Consume {
                instructions.push((
                    ir::Instruction::Portable(ir::PortableInstruction::Drop { value: *argument }),
                    Vec::new(),
                ));
            }
            continue;
        }
        if let Some(boxing) = erased_conversion(ty, *expected, layouts) {
            let converted = allocate_shared_value(value_types, storage_hints, *expected);
            instructions.push((
                match boxing {
                    ErasedConversion::Box => ir::Instruction::BoxValue {
                        destination: converted,
                        source: *argument,
                    },
                    ErasedConversion::Unbox => ir::Instruction::UnboxValue {
                        destination: converted,
                        source: *argument,
                    },
                },
                Vec::new(),
            ));
            lowered.push(converted);
            if *mode == ParameterMode::Consume {
                instructions.push((
                    ir::Instruction::Portable(ir::PortableInstruction::Drop { value: *argument }),
                    Vec::new(),
                ));
            }
            continue;
        }
        if *mode == ParameterMode::Borrow
            && matches!(ty, NativeType::Object(_) | NativeType::String)
        {
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

fn callable_conversion(actual: NativeType, expected: NativeType, layouts: &LayoutRegistry) -> bool {
    let (NativeType::Object(actual), NativeType::Object(expected)) = (actual, expected) else {
        return false;
    };
    matches!(layouts.get(actual).kind, LayoutKind::Closure { .. })
        && matches!(
            layouts.get(expected).kind,
            LayoutKind::Builtin {
                ty: crate::vm::VerificationType::Function { .. }
            }
        )
}

#[derive(Clone, Copy)]
enum ErasedConversion {
    Box,
    Unbox,
}

fn erased_conversion(
    actual: NativeType,
    expected: NativeType,
    layouts: &LayoutRegistry,
) -> Option<ErasedConversion> {
    let opaque = |ty| {
        matches!(
            ty,
            NativeType::Object(layout) if matches!(layouts.get(layout).kind, LayoutKind::Opaque)
        )
    };
    match (opaque(actual), opaque(expected)) {
        (false, true) => Some(ErasedConversion::Box),
        (true, false) => Some(ErasedConversion::Unbox),
        _ => None,
    }
}

fn shared_capture_arguments(
    captures: &[(crate::hir::CaptureMode, ir::Value)],
    expected_types: &[NativeType],
    layouts: &LayoutRegistry,
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
        if callable_conversion(ty, *expected, layouts) {
            let callable = allocate_shared_value(value_types, storage_hints, *expected);
            instructions.push((
                ir::Instruction::WrapCallable {
                    destination: callable,
                    source: *value,
                },
                Vec::new(),
            ));
            lowered.push(callable);
            if *mode == crate::hir::CaptureMode::Move {
                instructions.push((
                    ir::Instruction::Portable(ir::PortableInstruction::Drop { value: *value }),
                    Vec::new(),
                ));
            }
            continue;
        }
        if let Some(boxing) = erased_conversion(ty, *expected, layouts) {
            let converted = allocate_shared_value(value_types, storage_hints, *expected);
            instructions.push((
                match boxing {
                    ErasedConversion::Box => ir::Instruction::BoxValue {
                        destination: converted,
                        source: *value,
                    },
                    ErasedConversion::Unbox => ir::Instruction::UnboxValue {
                        destination: converted,
                        source: *value,
                    },
                },
                Vec::new(),
            ));
            lowered.push(converted);
            if *mode == crate::hir::CaptureMode::Move {
                instructions.push((
                    ir::Instruction::Portable(ir::PortableInstruction::Drop { value: *value }),
                    Vec::new(),
                ));
            }
            continue;
        }
        match mode {
            crate::hir::CaptureMode::Move => {
                lowered.push(*value);
                consumed.push(*value);
            }
            crate::hir::CaptureMode::Copy => {
                if matches!(ty, NativeType::Object(_) | NativeType::String) {
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
        NativeType::String => Some(crate::intrinsics::NativeReceiverKind::String),
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
        NativeType::Unit | NativeType::Bool | NativeType::Byte => abi::REF_LOAD_I8,
        NativeType::CodePoint => abi::REF_LOAD_I32,
        NativeType::Int => abi::REF_LOAD_I64,
        NativeType::Float => abi::REF_LOAD_F64,
        NativeType::String | NativeType::Object(_) | NativeType::Opaque => abi::REF_LOAD_PTR,
    }
}

fn reference_store_helper(ty: NativeType) -> &'static str {
    match ty {
        NativeType::Unit | NativeType::Bool | NativeType::Byte => abi::REF_STORE_I8,
        NativeType::CodePoint => abi::REF_STORE_I32,
        NativeType::Int => abi::REF_STORE_I64,
        NativeType::Float => abi::REF_STORE_F64,
        NativeType::String | NativeType::Object(_) | NativeType::Opaque => abi::REF_STORE_PTR,
    }
}

fn lower_native_ir(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    prepared: &NativeFunction,
    backend: &NativeBackend<'_>,
) -> Result<(), FosterError> {
    let function = &prepared.ir;
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

fn native_type_semantic(ty: NativeType, layouts: &LayoutRegistry) -> ValueSemantic {
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

fn lower_portable_native(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    instruction: &ir::PortableInstruction,
    context: NativeLowering<'_, '_>,
) -> Result<Option<ClifValue>, FosterError> {
    let NativeLowering {
        function,
        values,
        homes,
        mutable_parameter_homes,
        backend,
    } = context;
    let objects = backend.objects;
    let get = |value: &ir::Value| values[value];
    match instruction {
        ir::PortableInstruction::Drop { value } => {
            if let Some(layout) = objects.layouts.managed_layout(function.value_type(*value)) {
                objects.release(builder, module, get(value), layout)?;
            }
            Ok(None)
        }
        ir::PortableInstruction::Move {
            destination,
            source,
        } => {
            let value = get(source);
            if let Some(layout) = objects
                .layouts
                .managed_layout(function.value_type(*destination))
            {
                objects.retain(builder, value, layout);
            }
            Ok(Some(value))
        }
        ir::PortableInstruction::CopyOnWrite {
            destination,
            source,
        } => {
            let source_value = *source;
            let original = get(source);
            let source_type = function.value_type(*destination);
            let (source, object_type) =
                native_reference_receiver(builder, module, original, source_type, backend)?;
            let address = (source_type != object_type).then_some(original);
            let NativeType::Object(layout) = object_type else {
                return Err(native_error("copy-on-write requires a native object"));
            };
            let physical = objects.layouts.physical.get(layout);
            if address.is_none()
                && function.storage_hints[source_value.0 as usize]
                    .is_some_and(|home| mutable_parameter_homes.contains(&home))
            {
                return Ok(Some(source));
            }
            // A builder's unique storage can be mutated directly. Shared values still detach
            // before mutation so snapshots and borrowed callers retain value semantics.
            let pointer_type = module.target_config().pointer_type();
            let count_address = builder
                .ins()
                .iadd_imm_s(source, i64::from(physical.header.strong_count_offset));
            let count =
                builder
                    .ins()
                    .atomic_load(pointer_type, MemFlagsData::trusted(), count_address);
            let unique = builder.ins().icmp_imm_s(IntCC::Equal, count, 1);
            let detach = builder.create_block();
            let ready = builder.create_block();
            builder.append_block_param(ready, pointer_type);
            builder
                .ins()
                .brif(unique, ready, &[source.into()], detach, &[]);
            builder.switch_to_block(detach);
            let copied = match &physical.kind {
                PhysicalKind::Record { fields, .. } => {
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
            builder.ins().jump(ready, &[copied.into()]);
            builder.switch_to_block(ready);
            let unique = builder.block_params(ready)[0];
            if let Some(address) = address {
                store_physical_value(builder, address, 0, unique);
                Ok(Some(address))
            } else {
                Ok(Some(unique))
            }
        }
        ir::PortableInstruction::SpawnRemote { destination, value } => {
            lower_native_spawn_remote(builder, module, *destination, *value, false, context)
                .map(Some)
        }
        ir::PortableInstruction::SpawnRemoteBorrow {
            destination,
            source,
        } => lower_native_spawn_remote(builder, module, *destination, *source, true, context)
            .map(Some),
        ir::PortableInstruction::RemoteCall {
            destination,
            remote,
            function: target,
            arguments,
        } => lower_native_remote_call(
            builder,
            module,
            *destination,
            *remote,
            *target,
            arguments,
            context,
        )
        .map(Some),
        ir::PortableInstruction::Await {
            destination,
            future,
        } => lower_native_await(builder, module, *destination, *future, context).map(Some),
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
                    semantic: ValueSemantic::Object,
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
            let (address, element) = match objects.layouts.physical.get(layout).kind {
                PhysicalKind::Bytes {
                    data_offset,
                    length_offset,
                } => {
                    let word = module.target_config().pointer_type();
                    let length = builder.ins().load(
                        word,
                        MemFlagsData::trusted(),
                        get(object),
                        length_offset as i32,
                    );
                    let outside =
                        builder
                            .ins()
                            .icmp(IntCC::UnsignedGreaterThanOrEqual, get(index), length);
                    fail_if(
                        builder,
                        module,
                        outside,
                        abi::failure::INDEX_OUT_OF_BOUNDS,
                        get(index),
                        length,
                    )?;
                    let data = builder.ins().load(
                        word,
                        MemFlagsData::trusted(),
                        get(object),
                        data_offset as i32,
                    );
                    (
                        builder.ins().iadd(data, get(index)),
                        ValueLayout {
                            size: 1,
                            align: 1,
                            kind: ScalarKind::I8,
                            semantic: ValueSemantic::Byte,
                            pointee: None,
                        },
                    )
                }
                _ => native_buffer_element_address(
                    builder,
                    module,
                    get(object),
                    get(index),
                    layout,
                    objects,
                )?,
            };
            let value = builder.ins().load(
                physical_cranelift_type(element.kind, module.target_config().pointer_type()),
                MemFlagsData::trusted(),
                address,
                0,
            );
            if let Some(pointee) = objects
                .layouts
                .managed_layout(function.value_type(*destination))
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
            let (object, object_type) = native_reference_receiver(
                builder,
                module,
                get(object),
                function.value_type(*object),
                backend,
            )?;
            let NativeType::Object(layout) = object_type else {
                return Err(native_error("native push requires a buffer"));
            };
            push_native_buffer(builder, module, object, get(value), layout, objects)?;
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
                    objects.layouts.logical,
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
            let layout = objects
                .layouts
                .managed_layout(function.value_type(*destination))
                .ok_or_else(|| native_error("record result uses the wrong native layout"))?;
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
            let PhysicalKind::Record { fields, .. } = &physical.kind else {
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
                if let Some(pointee) = objects.layouts.managed_layout(function.value_type(*source))
                {
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
                if let Some(pointee) = objects.layouts.managed_layout(function.value_type(*source))
                {
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
            if let LayoutKind::Builtin {
                ty:
                    crate::vm::VerificationType::Function {
                        parameters, result, ..
                    },
            } = &objects.layouts.logical.get(layout).kind
            {
                let PhysicalKind::Callable {
                    code_offset,
                    environment_offset,
                    ..
                } = objects.layouts.physical.get(layout).kind
                else {
                    return Err(native_error("callable has the wrong physical layout"));
                };
                let callable = get(callee);
                let word = module.target_config().pointer_type();
                let code =
                    builder
                        .ins()
                        .load(word, MemFlagsData::trusted(), callable, code_offset as i32);
                let environment = builder.ins().load(
                    word,
                    MemFlagsData::trusted(),
                    callable,
                    environment_offset as i32,
                );
                let mut lowered = Vec::with_capacity(arguments.len() + 1);
                lowered.push(environment);
                lowered.extend(arguments.iter().map(get));
                let mut abi_parameters = vec![NativeType::Opaque];
                abi_parameters.extend(
                    parameters
                        .iter()
                        .map(|parameter| {
                            native_verification_type(
                                objects.layouts.program,
                                objects.layouts.logical,
                                parameter,
                                None,
                            )
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                );
                let result = native_verification_type(
                    objects.layouts.program,
                    objects.layouts.logical,
                    result,
                    None,
                )?;
                let signature = signature(
                    module,
                    &ir::Signature {
                        parameters: abi_parameters,
                        result,
                    },
                );
                let signature = builder.func.import_signature(signature);
                let call = builder.ins().call_indirect(signature, code, &lowered);
                return Ok(Some(builder.inst_results(call)[0]));
            }
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
        ir::PortableInstruction::CallContractMethod {
            destination,
            receiver,
            slot,
            name,
            arguments,
        } => lower_contract_dispatch(
            builder,
            module,
            get(receiver),
            function.value_type(*receiver),
            function.value_type(*destination),
            *slot,
            name,
            &arguments.iter().map(get).collect::<Vec<_>>(),
            &arguments
                .iter()
                .map(|argument| function.value_type(*argument))
                .collect::<Vec<_>>(),
            backend,
        )
        .map(Some),
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
                    ty: crate::vm::VerificationType::Bytes
                }
            ) {
                let (data_offset, length_offset) = native_bytes_layout(layout, objects)?;
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
                        let index = zero_i64(builder);
                        fail_if(
                            builder,
                            module,
                            empty,
                            abi::failure::INDEX_OUT_OF_BOUNDS,
                            index,
                            length,
                        )?;
                        let data = builder.ins().load(
                            word,
                            MemFlagsData::trusted(),
                            get(object),
                            data_offset as i32,
                        );
                        builder
                            .ins()
                            .load(types::I8, MemFlagsData::trusted(), data, 0)
                    }
                    "rest" => native_bytes_tail(builder, module, get(object), layout, objects)?,
                    _ => return Err(native_error(format!("native Bytes has no field `{field}`"))),
                };
                return Ok(Some(result));
            }
            if matches!(
                objects.layouts.logical.get(layout).kind,
                LayoutKind::Builtin {
                    ty: crate::vm::VerificationType::ByteBuffer
                }
            ) {
                let (_, length_offset, capacity_offset, _) = native_buffer_layout(layout, objects)?;
                let offset = match field.as_str() {
                    "length" => length_offset,
                    "capacity" => capacity_offset,
                    "empty?" => {
                        let length = builder.ins().load(
                            module.target_config().pointer_type(),
                            MemFlagsData::trusted(),
                            get(object),
                            length_offset as i32,
                        );
                        return Ok(Some(builder.ins().icmp_imm_s(IntCC::Equal, length, 0)));
                    }
                    _ => {
                        return Err(native_error(format!(
                            "native ByteBuffer has no field `{field}`"
                        )));
                    }
                };
                return Ok(Some(builder.ins().load(
                    module.target_config().pointer_type(),
                    MemFlagsData::trusted(),
                    get(object),
                    offset as i32,
                )));
            }
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
                        let index = zero_i64(builder);
                        fail_if(
                            builder,
                            module,
                            empty,
                            abi::failure::INDEX_OUT_OF_BOUNDS,
                            index,
                            length,
                        )?;
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
            if let Some(pointee) = objects
                .layouts
                .managed_layout(function.value_type(*destination))
            {
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
            if let Some(pointee) = objects.layouts.managed_layout(function.value_type(*source)) {
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
                NativeIntrinsic::Print { newline } => {
                    for (index, (argument, value)) in
                        arguments.iter().zip(lowered.iter().copied()).enumerate()
                    {
                        if index > 0 {
                            write_native_separator(builder, module)?;
                        }
                        write_native_value(builder, module, value, function.value_type(*argument))?;
                    }
                    if newline {
                        write_native_newline(builder, module)?;
                    }
                    builder.ins().iconst(types::I8, 0)
                }
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
                    let limit = zero_i64(builder);
                    fail_if(
                        builder,
                        module,
                        invalid,
                        abi::failure::INVALID_CODE_POINT,
                        value,
                        limit,
                    )?;
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
                    let limit = builder.ins().iconst(types::I64, 255);
                    fail_if(
                        builder,
                        module,
                        invalid,
                        abi::failure::INVALID_BYTE,
                        lowered[0],
                        limit,
                    )?;
                    builder.ins().ireduce(types::I8, lowered[0])
                }
                NativeIntrinsic::Inline(NativeInlineIntrinsic::BytesFromList) => {
                    let NativeType::Object(result_layout) = function.value_type(*destination)
                    else {
                        return Err(native_error("Bytes result has no object layout"));
                    };
                    let NativeType::Object(source_layout) = function.value_type(arguments[0])
                    else {
                        return Err(native_error("Bytes.from requires a native byte list"));
                    };
                    let (source_data, length, _, element) =
                        native_buffer_layout(source_layout, objects)?;
                    if element.kind != ScalarKind::I8 {
                        return Err(native_error("Bytes.from requires List<Byte>"));
                    }
                    let source_data = builder.ins().load(
                        module.target_config().pointer_type(),
                        MemFlagsData::trusted(),
                        lowered[0],
                        source_data as i32,
                    );
                    let length = builder.ins().load(
                        module.target_config().pointer_type(),
                        MemFlagsData::trusted(),
                        lowered[0],
                        length as i32,
                    );
                    let (object, data) =
                        allocate_native_bytes(builder, module, objects, result_layout, length)?;
                    copy_native_bytes(builder, module, data, source_data, length)?;
                    object
                }
                NativeIntrinsic::Inline(NativeInlineIntrinsic::BytesToList) => {
                    let NativeType::Object(result_layout) = function.value_type(*destination)
                    else {
                        return Err(native_error("byte list result has no object layout"));
                    };
                    let NativeType::Object(source_layout) = function.value_type(arguments[0])
                    else {
                        return Err(native_error("Bytes.list requires native Bytes"));
                    };
                    let (source_data_offset, source_length_offset) =
                        native_bytes_layout(source_layout, objects)?;
                    let source_data = builder.ins().load(
                        module.target_config().pointer_type(),
                        MemFlagsData::trusted(),
                        lowered[0],
                        source_data_offset as i32,
                    );
                    let length = builder.ins().load(
                        module.target_config().pointer_type(),
                        MemFlagsData::trusted(),
                        lowered[0],
                        source_length_offset as i32,
                    );
                    let (object, data) = allocate_native_byte_buffer(
                        builder,
                        module,
                        objects,
                        result_layout,
                        length,
                    )?;
                    copy_native_bytes(builder, module, data, source_data, length)?;
                    object
                }
                NativeIntrinsic::Runtime(helper) => runtime_call(
                    builder,
                    module,
                    helper,
                    &runtime_signature(*destination, arguments, &function.value_types),
                    &lowered,
                )?,
                NativeIntrinsic::Host => lower_native_host_intrinsic(
                    builder,
                    module,
                    *builtin,
                    NativeHostArguments {
                        values: arguments,
                        lowered: &lowered,
                    },
                    function.value_type(*destination),
                    function,
                    objects,
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

#[allow(clippy::too_many_arguments)]
fn lower_contract_dispatch(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    receiver: ClifValue,
    receiver_type: NativeType,
    result_type: NativeType,
    slot: crate::types::DispatchSlot,
    name: &str,
    arguments: &[ClifValue],
    argument_types: &[NativeType],
    backend: &NativeBackend<'_>,
) -> Result<ClifValue, FosterError> {
    let candidates = contract_candidates(slot, receiver_type, argument_types, backend.ir)?;
    if candidates.is_empty() {
        return Err(native_error(format!(
            "value has no native implementation of required method `{name}`"
        )));
    }
    let word = module.target_config().pointer_type();
    let payload = match receiver_type {
        NativeType::Object(layout)
            if matches!(backend.ir.layouts.get(layout).kind, LayoutKind::Opaque) =>
        {
            let PhysicalKind::Opaque { value_offset, .. } =
                backend.ir.physical_layouts.get(layout).kind
            else {
                return Err(native_error(
                    "contract receiver has an invalid erased layout",
                ));
            };
            builder
                .ins()
                .load(word, MemFlagsData::trusted(), receiver, value_offset as i32)
        }
        NativeType::Object(_) => receiver,
        _ => {
            return Err(native_error(
                "native contract dispatch requires a descriptor-backed object",
            ));
        }
    };
    let descriptor = builder.ins().load(
        word,
        MemFlagsData::trusted(),
        payload,
        backend.ir.physical_layouts.header().descriptor_offset as i32,
    );
    let join = builder.create_block();
    builder.append_block_param(join, cranelift_type(result_type, word));
    for candidate in candidates {
        let call_block = builder.create_block();
        let next = builder.create_block();
        let expected = module
            .declare_data_in_func(backend.objects.descriptors[&candidate.layout], builder.func);
        let expected = builder.ins().symbol_value(word, expected);
        let matches = builder.ins().icmp(IntCC::Equal, descriptor, expected);
        builder.ins().brif(matches, call_block, &[], next, &[]);
        builder.switch_to_block(call_block);
        backend.objects.retain(builder, payload, candidate.layout);
        let mut lowered = Vec::with_capacity(arguments.len() + 1);
        lowered.push(payload);
        lowered.extend_from_slice(arguments);
        let target =
            module.declare_func_in_func(backend.functions[&candidate.function], builder.func);
        let call = builder.ins().call(target, &lowered);
        let result = builder.inst_results(call)[0];
        builder.ins().jump(join, &[result.into()]);
        builder.switch_to_block(next);
    }
    let kind = builder
        .ins()
        .iconst(types::I64, abi::failure::CONTRACT_DISPATCH);
    let detail = builder.ins().iconst(types::I64, i64::from(slot.0));
    let limit = zero_i64(builder);
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
    let fallback = if result_type == NativeType::Float {
        builder.ins().f64const(0.0)
    } else {
        builder.ins().iconst(cranelift_type(result_type, word), 0)
    };
    builder.ins().jump(join, &[fallback.into()]);
    builder.switch_to_block(join);
    Ok(builder.block_params(join)[0])
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
            ..
        } => Ok((data_offset, length_offset, capacity_offset, element)),
        _ => Err(native_error(
            "native list operation requires a buffer layout",
        )),
    }
}

fn native_bytes_layout(
    layout: LayoutId,
    objects: ObjectRuntime<'_>,
) -> Result<(u32, u32), FosterError> {
    match objects.layouts.physical.get(layout).kind {
        PhysicalKind::Bytes {
            data_offset,
            length_offset,
        } => Ok((data_offset, length_offset)),
        ref kind => Err(native_error(format!(
            "native Bytes l{} has the wrong physical layout {kind:?}",
            layout.0
        ))),
    }
}

fn native_handle_layout(
    layout: LayoutId,
    objects: ObjectRuntime<'_>,
) -> Result<(u32, u32), FosterError> {
    match objects.layouts.physical.get(layout).kind {
        PhysicalKind::Handle {
            handle_offset,
            value_descriptor_offset,
        } => Ok((handle_offset, value_descriptor_offset)),
        _ => Err(native_error("native remote value requires a handle layout")),
    }
}

fn native_release_address(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    ty: NativeType,
    backend: &NativeBackend<'_>,
) -> ClifValue {
    if let Some(layout) = backend.objects.layouts.managed_layout(ty) {
        let release = module.declare_func_in_func(backend.release_thunks[&layout], builder.func);
        builder
            .ins()
            .func_addr(module.target_config().pointer_type(), release)
    } else {
        builder
            .ins()
            .iconst(module.target_config().pointer_type(), 0)
    }
}

fn native_descriptor_address(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    ty: NativeType,
    backend: &NativeBackend<'_>,
) -> ClifValue {
    if let NativeType::Object(layout) = ty {
        let descriptor =
            module.declare_data_in_func(backend.objects.descriptors[&layout], builder.func);
        builder
            .ins()
            .symbol_value(module.target_config().pointer_type(), descriptor)
    } else {
        builder
            .ins()
            .iconst(module.target_config().pointer_type(), 0)
    }
}

fn allocate_native_handle(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    layout: LayoutId,
    handle: ClifValue,
    value_type: NativeType,
    backend: &NativeBackend<'_>,
) -> Result<ClifValue, FosterError> {
    let object = backend.objects.allocate(builder, module, layout)?;
    let (handle_offset, descriptor_offset) = native_handle_layout(layout, backend.objects)?;
    store_physical_value(builder, object, handle_offset, handle);
    let descriptor = native_descriptor_address(builder, module, value_type, backend);
    store_physical_value(builder, object, descriptor_offset, descriptor);
    Ok(object)
}

fn lower_native_spawn_remote(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    destination: ir::Value,
    source: ir::Value,
    borrowed: bool,
    context: NativeLowering<'_, '_>,
) -> Result<ClifValue, FosterError> {
    let NativeType::Object(layout) = context.function.value_type(destination) else {
        return Err(native_error("native Remote<T> has a non-object layout"));
    };
    let source_type = context.function.value_type(source);
    if borrowed && !matches!(source_type, NativeType::Object(_)) {
        return Err(native_error(
            "borrowed native remote state must be an object",
        ));
    }
    let state = native_to_remote_word(builder, module, context.values[&source], source_type);
    if borrowed
        && let NativeType::Object(source_layout) = source_type
        && context.backend.objects.layouts.is_managed(source_layout)
    {
        context
            .backend
            .objects
            .retain(builder, context.values[&source], source_layout);
    }
    let release = native_release_address(builder, module, source_type, context.backend);
    let borrowed = builder.ins().iconst(types::I8, i64::from(borrowed));
    let handle = runtime_call(
        builder,
        module,
        abi::REMOTE_SPAWN,
        &ir::Signature {
            parameters: vec![NativeType::Int, NativeType::Opaque, NativeType::Bool],
            result: NativeType::Opaque,
        },
        &[state, release, borrowed],
    )?;
    allocate_native_handle(
        builder,
        module,
        layout,
        handle,
        source_type,
        context.backend,
    )
}

fn lower_native_remote_call(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    destination: ir::Value,
    remote: ir::Value,
    target: FunctionId,
    arguments: &[(ParameterMode, ir::Value)],
    context: NativeLowering<'_, '_>,
) -> Result<ClifValue, FosterError> {
    let target_signature = &context.backend.ir.function_types[&target];
    if target_signature.parameters.len() != arguments.len() + 1 {
        return Err(native_error(
            "native remote call has the wrong argument arity",
        ));
    }
    let frame_size = arguments
        .len()
        .checked_mul(8)
        .and_then(|size| u32::try_from(size.max(8)).ok())
        .ok_or_else(|| native_error("native remote argument frame is too large"))?;
    let frame = builder.create_sized_stack_slot(StackSlotData::new(
        StackSlotKind::ExplicitSlot,
        frame_size,
        3,
    ));
    for (index, (_, argument)) in arguments.iter().enumerate() {
        let value = native_to_remote_word(
            builder,
            module,
            context.values[argument],
            context.function.value_type(*argument),
        );
        builder.ins().stack_store(
            module.target_config().pointer_type(),
            value,
            frame,
            i32::try_from(index * 8)
                .map_err(|_| native_error("native remote argument frame exceeds i32 offsets"))?,
        );
    }
    let NativeType::Object(remote_layout) = context.function.value_type(remote) else {
        return Err(native_error("native remote call has a non-object receiver"));
    };
    let (handle_offset, _) = native_handle_layout(remote_layout, context.backend.objects)?;
    let handle = builder.ins().load(
        module.target_config().pointer_type(),
        MemFlagsData::trusted(),
        context.values[&remote],
        handle_offset as i32,
    );
    let thunk = context
        .backend
        .remote_thunks
        .get(&target)
        .copied()
        .ok_or_else(|| native_error("native remote method has no callback thunk"))?;
    let thunk = module.declare_func_in_func(thunk, builder.func);
    let thunk = builder
        .ins()
        .func_addr(module.target_config().pointer_type(), thunk);
    let argument_data = builder
        .ins()
        .stack_addr(module.target_config().pointer_type(), frame, 0);
    let argument_count = builder.ins().iconst(
        types::I64,
        i64::try_from(arguments.len())
            .map_err(|_| native_error("native remote argument count exceeds Int"))?,
    );
    let blocking = arguments.iter().any(|(mode, argument)| {
        *mode == ParameterMode::Borrow
            && matches!(
                context.function.value_type(*argument),
                NativeType::Object(_) | NativeType::String
            )
    });
    let blocking = builder.ins().iconst(types::I8, i64::from(blocking));
    let result_release =
        native_release_address(builder, module, target_signature.result, context.backend);
    let future = runtime_call(
        builder,
        module,
        abi::REMOTE_CALL,
        &ir::Signature {
            parameters: vec![
                NativeType::Opaque,
                NativeType::Opaque,
                NativeType::Opaque,
                NativeType::Int,
                NativeType::Bool,
                NativeType::Opaque,
            ],
            result: NativeType::Opaque,
        },
        &[
            handle,
            thunk,
            argument_data,
            argument_count,
            blocking,
            result_release,
        ],
    )?;
    let NativeType::Object(future_layout) = context.function.value_type(destination) else {
        return Err(native_error("native Future<T> has a non-object layout"));
    };
    allocate_native_handle(
        builder,
        module,
        future_layout,
        future,
        target_signature.result,
        context.backend,
    )
}

fn lower_native_await(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    destination: ir::Value,
    future: ir::Value,
    context: NativeLowering<'_, '_>,
) -> Result<ClifValue, FosterError> {
    let NativeType::Object(future_layout) = context.function.value_type(future) else {
        return Err(native_error("native await has a non-object future"));
    };
    let (handle_offset, _) = native_handle_layout(future_layout, context.backend.objects)?;
    let handle = builder.ins().load(
        module.target_config().pointer_type(),
        MemFlagsData::trusted(),
        context.values[&future],
        handle_offset as i32,
    );
    let value = runtime_call(
        builder,
        module,
        abi::FUTURE_AWAIT,
        &ir::Signature {
            parameters: vec![NativeType::Opaque],
            result: NativeType::Int,
        },
        &[handle],
    )?;
    Ok(remote_word_to_native(
        builder,
        module,
        value,
        context.function.value_type(destination),
    ))
}

fn lower_result_error_conversion(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    source: ClifValue,
    source_type: NativeType,
    target_type: NativeType,
    objects: ObjectRuntime<'_>,
) -> Result<ClifValue, FosterError> {
    let (NativeType::Object(source_layout), NativeType::Object(target_layout)) =
        (source_type, target_type)
    else {
        return Err(native_error(
            "Result error conversion requires object layouts",
        ));
    };
    let result_error = |layout| -> Result<(u32, AlternativeLayout), FosterError> {
        let PhysicalKind::Variant {
            tag_offset,
            alternatives,
            ..
        } = &objects.layouts.physical.get(layout).kind
        else {
            return Err(native_error(
                "Result error conversion requires variant layouts",
            ));
        };
        let alternative = alternatives
            .iter()
            .find(|alternative| alternative.name == "Error")
            .cloned()
            .ok_or_else(|| native_error("Result layout is missing its Error alternative"))?;
        Ok((*tag_offset, alternative))
    };
    let (_, source_error) = result_error(source_layout)?;
    let (target_tag_offset, target_error) = result_error(target_layout)?;
    if source_error.fields.len() != 1
        || target_error.fields.len() != 1
        || source_error.fields[0].value != target_error.fields[0].value
    {
        return Err(native_error(
            "Result error conversion has incompatible error payloads",
        ));
    }
    let field = &source_error.fields[0];
    let value = load_physical_value(builder, module, source, field.offset, field.value);
    if let Some(pointee) = field.value.pointee
        && objects.layouts.is_managed(pointee)
    {
        objects.retain(builder, value, pointee);
    }
    let target = objects.allocate(builder, module, target_layout)?;
    let tag = builder
        .ins()
        .iconst(types::I32, i64::from(target_error.tag));
    builder.ins().store(
        MemFlagsData::trusted(),
        tag,
        target,
        target_tag_offset as i32,
    );
    store_physical_value(builder, target, target_error.fields[0].offset, value);
    Ok(target)
}

#[derive(Clone, Copy)]
struct NativeHostArguments<'a> {
    values: &'a [ir::Value],
    lowered: &'a [ClifValue],
}

fn lower_native_host_intrinsic(
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

fn allocate_byte_data(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    length: ClifValue,
) -> Result<ClifValue, FosterError> {
    let word = module.target_config().pointer_type();
    let empty = builder.ins().icmp_imm_s(IntCC::Equal, length, 0);
    let one = builder.ins().iconst(word, 1);
    let size = builder.ins().select(empty, one, length);
    let align = builder.ins().iconst(types::I64, 1);
    runtime_call(
        builder,
        module,
        abi::ALLOC,
        &ir::Signature {
            parameters: vec![NativeType::Int, NativeType::Int],
            result: NativeType::Opaque,
        },
        &[size, align],
    )
}

fn allocate_native_bytes(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    objects: ObjectRuntime<'_>,
    layout: LayoutId,
    length: ClifValue,
) -> Result<(ClifValue, ClifValue), FosterError> {
    let (data_offset, length_offset) = native_bytes_layout(layout, objects)?;
    let object = objects.allocate(builder, module, layout)?;
    let data = allocate_byte_data(builder, module, length)?;
    store_physical_value(builder, object, data_offset, data);
    store_physical_value(builder, object, length_offset, length);
    Ok((object, data))
}

fn allocate_native_byte_buffer(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    objects: ObjectRuntime<'_>,
    layout: LayoutId,
    length: ClifValue,
) -> Result<(ClifValue, ClifValue), FosterError> {
    let (data_offset, length_offset, capacity_offset, element) =
        native_buffer_layout(layout, objects)?;
    if element.size != 1 {
        return Err(native_error(
            "byte buffer allocation requires one-byte elements",
        ));
    }
    let object = objects.allocate(builder, module, layout)?;
    let data = allocate_byte_data(builder, module, length)?;
    store_physical_value(builder, object, data_offset, data);
    store_physical_value(builder, object, length_offset, length);
    store_physical_value(builder, object, capacity_offset, length);
    Ok((object, data))
}

fn copy_native_bytes(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    destination: ClifValue,
    source: ClifValue,
    length: ClifValue,
) -> Result<(), FosterError> {
    runtime_call(
        builder,
        module,
        abi::COPY_BYTES,
        &ir::Signature {
            parameters: vec![NativeType::Opaque, NativeType::Opaque, NativeType::Int],
            result: NativeType::Unit,
        },
        &[destination, source, length],
    )?;
    Ok(())
}

fn native_bytes_tail(
    builder: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    source: ClifValue,
    layout: LayoutId,
    objects: ObjectRuntime<'_>,
) -> Result<ClifValue, FosterError> {
    let (data_offset, length_offset) = native_bytes_layout(layout, objects)?;
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
    let (target, target_data) =
        allocate_native_bytes(builder, module, objects, layout, tail_length)?;
    let first_tail_byte = builder.ins().iadd_imm_s(source_data, 1);
    copy_native_bytes(builder, module, target_data, first_tail_byte, tail_length)?;
    Ok(target)
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
        abi::ALLOC,
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
    module: &mut ObjectModule,
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
    fail_if(
        builder,
        module,
        outside,
        abi::failure::INDEX_OUT_OF_BOUNDS,
        index,
        length,
    )?;
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
        abi::ALLOC,
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
    // Capacity growth is storage policy, not a Foster collection algorithm. Keep byte-size
    // arithmetic representable before allocating or writing the next element.
    let maximum = isize::MAX as i64 / i64::from(element.size);
    let full = builder
        .ins()
        .icmp_imm_s(IntCC::SignedGreaterThanOrEqual, length, maximum);
    let limit = builder.ins().iconst(word, maximum);
    fail_if(
        builder,
        module,
        full,
        abi::failure::INTEGER_OVERFLOW,
        length,
        limit,
    )?;
    let new_length = builder.ins().iadd_imm_s(length, 1);
    let grow = builder.create_block();
    let ready = builder.create_block();
    builder.append_block_param(ready, word);
    let needs_growth = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThan, new_length, old_capacity);
    builder
        .ins()
        .brif(needs_growth, grow, &[], ready, &[old_data.into()]);
    builder.switch_to_block(grow);
    let can_double =
        builder
            .ins()
            .icmp_imm_s(IntCC::SignedLessThanOrEqual, old_capacity, maximum / 2);
    let doubled = builder.ins().imul_imm_u(old_capacity, 2);
    let grown = builder.ins().select(can_double, doubled, limit);
    let too_small = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, grown, new_length);
    let new_capacity = builder.ins().select(too_small, new_length, grown);
    let size = builder
        .ins()
        .imul_imm_u(new_capacity, i64::from(element.size));
    let align = builder.ins().iconst(types::I64, i64::from(element.align));
    let new_data = runtime_call(
        builder,
        module,
        abi::ALLOC,
        &ir::Signature {
            parameters: vec![NativeType::Int, NativeType::Int],
            result: NativeType::Opaque,
        },
        &[size, align],
    )?;
    copy_native_buffer_data(builder, module, old_data, new_data, length, element);
    store_physical_value(builder, object, data_offset, new_data);
    store_physical_value(builder, object, capacity_offset, new_capacity);
    let old_size = builder
        .ins()
        .imul_imm_u(old_capacity, i64::from(element.size));
    runtime_call(
        builder,
        module,
        abi::DEALLOC,
        &ir::Signature {
            parameters: vec![NativeType::Opaque, NativeType::Int, NativeType::Int],
            result: NativeType::Unit,
        },
        &[old_data, old_size, align],
    )?;
    builder.ins().jump(ready, &[new_data.into()]);
    builder.switch_to_block(ready);
    let data = builder.block_params(ready)[0];
    let offset = builder.ins().imul_imm_u(length, i64::from(element.size));
    let address = builder.ins().iadd(data, offset);
    if let Some(pointee) = element.pointee
        && objects.layouts.is_managed(pointee)
    {
        objects.retain(builder, value, pointee);
    }
    store_physical_value(builder, address, 0, value);
    store_physical_value(builder, object, length_offset, new_length);
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
            let detail = builder.ins().uextend(types::I64, right);
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

fn runtime_call(
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
    Ok(builder.inst_results(call)[0])
}

fn fail_if(
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

fn zero_i64(builder: &mut FunctionBuilder<'_>) -> ClifValue {
    builder.ins().iconst(types::I64, 0)
}

fn write_native_value(
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

fn write_native_separator(
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

fn write_native_newline(
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

fn runtime_strings(program: &Program) -> (Vec<String>, HashMap<u16, u64>, HashMap<String, u64>) {
    let mut values = Vec::new();
    let mut indices = HashMap::new();
    let mut literals = HashMap::new();
    for (index, constant) in program.constants.iter().enumerate() {
        if let Constant::String(value) | Constant::Symbol(value) = constant {
            indices.insert(index as u16, values.len() as u64);
            literals.entry(value.clone()).or_insert(values.len() as u64);
            values.push(value.clone());
        }
    }
    let mut functions = program.functions.iter().collect::<Vec<_>>();
    functions.sort_unstable_by_key(|(function, _)| function.into_raw().into_u32());
    for (_, function) in functions {
        for pattern in function
            .instructions
            .iter()
            .filter_map(|instruction| match instruction {
                Instruction::MatchPattern { pattern, .. } => Some(pattern),
                _ => None,
            })
        {
            collect_pattern_literals(pattern, &mut |literal| {
                if !literals.contains_key(literal) {
                    let index = values.len() as u64;
                    literals.insert(literal.to_owned(), index);
                    values.push(literal.to_owned());
                }
            });
        }
    }
    (values, indices, literals)
}

fn collect_pattern_literals(pattern: &Pattern, visit: &mut impl FnMut(&str)) {
    match pattern.unspanned() {
        Pattern::String(value) | Pattern::Symbol(value) => visit(value),
        Pattern::Variant { fields, .. } => {
            for field in fields {
                collect_pattern_literals(field, visit);
            }
        }
        Pattern::Spanned { .. } => unreachable!(),
        Pattern::Wildcard
        | Pattern::Binding(_)
        | Pattern::Bool(_)
        | Pattern::Integer(_)
        | Pattern::Float(_)
        | Pattern::CodePoint(_) => {}
    }
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
        let prepared = prepare(&compilation).unwrap();
        let program = &prepared.program;
        let main = prepared.main;
        let function = prepared
            .functions()
            .iter()
            .find(|function| function.source_function() == main)
            .unwrap()
            .ir();
        let function_types = &prepared.function_types;

        assert!(function.blocks.len() > 1);
        function.verify(function_types).unwrap();
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
