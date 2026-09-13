//! Reachability, specialization, and cached verification facts.
use super::{
    BTreeSet, BytecodeFunction, Compilation, ContractCandidate, FosterError, FunctionId, HashMap,
    Instruction, LayoutKind, LayoutRegistry, NativeInstance, NativeIrEnvironment, NativeType,
    Program, RawIdx, Register, SpecializationKey, Type, concrete_native_type,
    instruction_layout_type, ir, native_error, native_type, record_uses_dynamic_dispatch,
    specialized_executable_type, vm,
};
use crate::codegen::types::ExecutableType;

type RegisterTypes = Vec<Option<ExecutableType>>;
type FunctionFlow = Vec<Option<RegisterTypes>>;

/// Verified flow facts for the immutable construction program, shared by all specializations.
#[derive(Default)]
pub(super) struct FlowFacts {
    functions: HashMap<FunctionId, FunctionFlow>,
}
impl FlowFacts {
    pub(super) fn get(
        &mut self,
        program: &Program,
        function: FunctionId,
    ) -> Result<&[Option<RegisterTypes>], FosterError> {
        if let std::collections::hash_map::Entry::Vacant(entry) = self.functions.entry(function) {
            crate::compiler::profile::count("native.flow_analysis");
            entry.insert(vm::type_states(program, &program.functions[&function])?);
        }
        Ok(&self.functions[&function])
    }
}

pub(super) fn reachable_instances(
    compilation: &Compilation,
    program: &Program,
    shared_functions: &HashMap<FunctionId, ir::Function>,
    main: FunctionId,
    facts: &mut FlowFacts,
) -> Result<Vec<NativeInstance>, FosterError> {
    let mut reachable = BTreeSet::new();
    let mut concrete_nominals = BTreeSet::new();
    let mut contract_calls = BTreeSet::from([(crate::types::DEINIT_SLOT, Vec::new())]);
    let mut pending = vec![SpecializationKey {
        function: main,
        substitutions: Default::default(),
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
        let type_states = facts.get(program, instance.function)?;
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
                    contract_calls.insert((
                        if *slot == crate::types::CAN_COPY_SLOT {
                            crate::types::COPY_SLOT
                        } else {
                            *slot
                        },
                        argument_types,
                    ));
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
                    let ExecutableType::Remote(receiver) = receiver else {
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
            let target = match &instruction.instruction {
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
            let Some(nominal) = nominal_id(ty, compilation) else {
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
                    .map(|ty| {
                        specialized_executable_type(compilation, ty.ty, &Default::default(), 0)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if let Some(receiver) = parameter_types.first() {
                    receiver.infer_specialization(ty, &mut substitutions);
                }
                for (parameter, argument) in parameter_types.iter().skip(1).zip(argument_types) {
                    parameter.infer_specialization(argument, &mut substitutions);
                }
                let substitutions = substitutions.into();
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

fn nominal_id(
    ty: &crate::codegen::types::ExecutableType,
    compilation: &Compilation,
) -> Option<crate::types::NominalTypeId> {
    match ty {
        crate::codegen::types::ExecutableType::Record { record, .. } => {
            Some(crate::types::NominalTypeId::Record(*record))
        }
        crate::codegen::types::ExecutableType::Variant { variant, .. } => {
            Some(crate::types::NominalTypeId::Variant(*variant))
        }
        crate::codegen::types::ExecutableType::List(_)
        | crate::codegen::types::ExecutableType::Bytes
        | crate::codegen::types::ExecutableType::ByteBuffer => {
            let (module, name) = match ty {
                crate::codegen::types::ExecutableType::List(_) => ("core.list", "List"),
                crate::codegen::types::ExecutableType::Bytes => ("core.bytes", "Bytes"),
                _ => ("core.bytes.buffer", "ByteBuffer"),
            };
            let module = compilation.hir.module_named(module)?;
            compilation
                .hir
                .record_named(module, name)
                .map(crate::types::NominalTypeId::Record)
        }
        _ => None,
    }
}

fn collect_nominal_types(
    ty: &crate::codegen::types::ExecutableType,
    output: &mut BTreeSet<crate::codegen::types::ExecutableType>,
) {
    use crate::codegen::types::ExecutableType;
    match ty {
        ExecutableType::Record { arguments, .. } | ExecutableType::Variant { arguments, .. } => {
            if !ty.contains_generic() {
                output.insert(ty.clone());
            }
            for argument in arguments {
                collect_nominal_types(argument, output);
            }
        }
        ExecutableType::List(value) => {
            if !ty.contains_generic() {
                output.insert(ty.clone());
            }
            collect_nominal_types(value, output);
        }
        ExecutableType::Bytes | ExecutableType::ByteBuffer => {
            output.insert(ty.clone());
        }
        ExecutableType::Reference(value)
        | ExecutableType::Remote(value)
        | ExecutableType::Future(value) => collect_nominal_types(value, output),
        ExecutableType::Function {
            parameters, result, ..
        } => {
            for parameter in parameters {
                collect_nominal_types(&parameter.ty, output);
            }
            collect_nominal_types(result, output);
        }
        ExecutableType::Alternatives(values) => {
            for value in values {
                collect_nominal_types(value, output);
            }
        }
        ExecutableType::Intersection(values) => {
            for value in values {
                collect_nominal_types(value, output);
            }
        }
        ExecutableType::AliasArguments {
            arguments: values, ..
        } => {
            for value in values {
                collect_nominal_types(value, output);
            }
        }
        ExecutableType::Unknown
        | ExecutableType::Generic(_)
        | ExecutableType::Unit
        | ExecutableType::Bool
        | ExecutableType::Integer
        | ExecutableType::Float
        | ExecutableType::CodePoint
        | ExecutableType::Byte => {}
    }
}

fn remote_specialization(
    compilation: &Compilation,
    function: FunctionId,
    receiver: &ExecutableType,
    arguments: &[ExecutableType],
) -> Result<crate::codegen::types::Specialization, FosterError> {
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
        .map(|ty| specialized_executable_type(compilation, ty.ty, &Default::default(), 0))
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
    Ok(substitutions.into())
}

pub(super) fn executable_type_for_native(
    ty: NativeType,
    program: &Program,
    layouts: &LayoutRegistry,
) -> ExecutableType {
    match ty {
        NativeType::Unit => ExecutableType::Unit,
        NativeType::Bool => ExecutableType::Bool,
        NativeType::Int => ExecutableType::Integer,
        NativeType::Float => ExecutableType::Float,
        NativeType::CodePoint => ExecutableType::CodePoint,
        NativeType::Byte => ExecutableType::Byte,
        NativeType::String => program
            .string_record
            .map_or(ExecutableType::Unknown, |record| ExecutableType::Record {
                record,
                arguments: Vec::new(),
            }),
        NativeType::Object(layout) => match &layouts.get(layout).kind {
            LayoutKind::Record {
                record, arguments, ..
            } => ExecutableType::Record {
                record: *record,
                arguments: arguments.clone(),
            },
            LayoutKind::Variant {
                variant_type,
                arguments,
                ..
            } => ExecutableType::Variant {
                variant: *variant_type,
                arguments: arguments.clone(),
            },
            LayoutKind::Pointer { pointee, .. } => {
                ExecutableType::Reference(Box::new(pointee.clone()))
            }
            LayoutKind::Builtin { ty } => ty.clone(),
            LayoutKind::Opaque | LayoutKind::Closure { .. } => ExecutableType::Unknown,
        },
        NativeType::Opaque => ExecutableType::Unknown,
    }
}

pub(super) struct VerifiedRemoteCall {
    pub(super) target: FunctionId,
    pub(super) result: ExecutableType,
}

pub(super) fn verified_remote_calls(
    function: &BytecodeFunction,
    states: &[Option<Vec<Option<ExecutableType>>>],
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
        let ExecutableType::Remote(receiver) = logical_type(*remote)? else {
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

pub(super) fn contract_candidates(
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
            } => crate::codegen::types::ExecutableType::Record {
                record: *record,
                arguments: arguments.clone(),
            },
            LayoutKind::Variant {
                variant_type,
                arguments,
                ..
            } => crate::codegen::types::ExecutableType::Variant {
                variant: *variant_type,
                arguments: arguments.clone(),
            },
            LayoutKind::Builtin { ty } => ty.clone(),
            _ => continue,
        };
        let Some(nominal) = nominal_id(&concrete, environment.compilation) else {
            continue;
        };
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
                let receiver_type = if matches!(layout.kind, LayoutKind::Record { record, .. } if Some(record) == environment.program.string_record) { NativeType::String } else { NativeType::Object(layout.id) };
                (signature.parameters.first() == Some(&receiver_type)
                    && signature.parameters.len() == argument_types.len() + 1
                    && signature.parameters[1..].iter().zip(argument_types).all(|(expected, actual)| contract_argument_matches(*actual, *expected, environment)))
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

// Candidate selection precedes ABI argument legalization. A checked concrete value may
// be boxed into an erased structural parameter; shared_call_arguments owns that conversion.
// Do not accept arbitrary unboxing here, which could select an incompatible specialization.
pub(super) fn contract_argument_matches(
    actual: NativeType,
    expected: NativeType,
    environment: NativeIrEnvironment<'_>,
) -> bool {
    if actual == expected {
        return true;
    }
    if matches!(
        super::erased_conversion(actual, expected, environment.layouts),
        Some(super::ErasedConversion::Box)
    ) {
        return true;
    }
    let (NativeType::Object(actual), NativeType::Object(expected)) = (actual, expected) else {
        return false;
    };
    let LayoutKind::Closure {
        function,
        specialization,
        ..
    } = &environment.layouts.get(actual).kind
    else {
        return false;
    };
    let LayoutKind::Builtin {
        ty: ExecutableType::Function { parameters, result },
    } = &environment.layouts.get(expected).kind
    else {
        return false;
    };
    let signature = &environment.program.functions[function];
    let substitutions = specialization.iter().cloned().collect::<HashMap<_, _>>();
    signature
        .parameter_modes
        .iter()
        .copied()
        .eq(parameters.iter().map(|p| p.mode))
        && signature
            .parameter_types
            .iter()
            .map(|ty| ty.substitute(&substitutions))
            .collect::<Vec<_>>()
            == parameters.iter().map(|p| p.ty.clone()).collect::<Vec<_>>()
        && signature.result_type.substitute(&substitutions) == **result
}

pub(super) fn resolve_specialization(
    specialization: &crate::codegen::types::Specialization,
    outer: &crate::codegen::types::Specialization,
) -> crate::codegen::types::Specialization {
    specialization.map_values(|ty| ty.specialize(outer))
}

pub(super) fn collect_function_types(
    compilation: &Compilation,
    program: &Program,
    instances: &[NativeInstance],
    builtin_result_types: &HashMap<
        crate::intrinsics::Builtin,
        crate::codegen::types::ExecutableType,
    >,
    layouts: &mut LayoutRegistry,
    facts: &mut FlowFacts,
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
                    .enumerate()
                    .map(|(index, ty)| {
                        // Dispatch selects a concrete implementation before entering its
                        // receiver. Other values of a default-providing contract stay erased.
                        if index == 0
                            && definition.receiver.is_some()
                            && let Type::Record { record, .. } = compilation.types.types[ty.ty]
                            && record_uses_dynamic_dispatch(compilation, record)
                            && compilation
                                .types
                                .record_methods
                                .get(&record)
                                .is_none_or(|methods| {
                                    methods.iter().all(|name| {
                                        let owner = &compilation.hir.records[record];
                                        compilation
                                            .hir
                                            .functions_named(
                                                owner.module,
                                                &format!("{}.{name}", owner.name),
                                            )
                                            .iter()
                                            .any(|method| {
                                                compilation.hir.functions[*method]
                                                    .receiver
                                                    .is_some()
                                            })
                                    })
                                })
                        {
                            let concrete = specialized_executable_type(
                                compilation,
                                ty.ty,
                                &instance.key.substitutions,
                                0,
                            )?;
                            layouts.instantiate_type(&concrete)?;
                            if let ExecutableType::Record { record, arguments } = concrete {
                                return layouts
                                    .record_instance(record, &arguments)
                                    .map(NativeType::Object)
                                    .ok_or_else(|| {
                                        native_error("default receiver has no concrete layout")
                                    });
                            }
                        }
                        native_type(
                            compilation,
                            layouts,
                            ty.ty,
                            &instance.key.substitutions,
                            &definition.name,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            );
            if layouts.closure(function).is_some() {
                layouts.instantiate_closure(function, &instance.key.substitutions)?;
            }
            for state in facts.get(program, function)?.iter().flatten() {
                for ty in state.iter().flatten() {
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
            // Function results use the same callable ABI as parameters and fields. A callee
            // may forward another callable or select between distinct closure environments.
            let result = native_type(
                compilation,
                layouts,
                signature.result,
                &instance.key.substitutions,
                &definition.name,
            )?;
            Ok((instance.ir_function, ir::Signature { parameters, result }))
        })
        .collect()
}
