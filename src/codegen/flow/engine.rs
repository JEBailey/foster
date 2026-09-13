//! Shared logical transfer rules. VM slots and SSA identities use distinct consumption policies.
use super::FunctionSchema;
use crate::ast::{BinaryOp, ParameterMode, UnaryOp};
use crate::codegen::{
    ir::Value,
    metadata::{Constant, ProgramMetadata},
    types::{ExecutableType, Specialization},
};
use crate::error::FosterError;
use crate::hir::{CaptureMode, FunctionId, RecordId, VariantId};
use crate::intrinsics::{Builtin, IntrinsicArgumentMode, IntrinsicType};
use crate::types::{DispatchSlot, NominalTypeId};
use std::collections::{HashMap, HashSet, VecDeque};
pub(crate) struct Program<'a> {
    pub metadata: &'a ProgramMetadata,
    pub functions: &'a HashMap<FunctionId, FunctionSchema>,
}
/// Slot consumption checks ownership availability; SSA consumption preserves immutable type evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StoragePolicy {
    ConsumingSlots,
    ImmutableValues,
}
pub(crate) struct Body<'a> {
    pub schema: &'a FunctionSchema,
    pub instructions: Vec<Instruction>,
    pub value_count: usize,
    pub entry_values: Vec<Value>,
    pub write_bindings: &'a HashMap<Value, Value>,
    pub storage_policy: StoragePolicy,
}
impl std::ops::Deref for Body<'_> {
    type Target = FunctionSchema;
    fn deref(&self) -> &Self::Target {
        self.schema
    }
}
#[derive(Debug, Clone)]
pub(crate) enum Instruction {
    /// Releases this frame's ownership of an inline register or promoted slot.
    ///
    /// A promoted register detaches rather than writing through its slot because
    /// reference captures may still own and observe that slot.
    Drop {
        register: Value,
    },
    LoadConstant {
        destination: Value,
        constant: u16,
    },
    Move {
        destination: Value,
        source: Value,
    },
    Unary {
        destination: Value,
        operator: UnaryOp,
        operand: Value,
    },
    Binary {
        destination: Value,
        operator: BinaryOp,
        left: Value,
        right: Value,
    },
    MakeList {
        destination: Value,
        element_type: ExecutableType,
        elements: Vec<Value>,
    },
    Index {
        destination: Value,
        object: Value,
        index: Value,
    },
    MakeRecord {
        destination: Value,
        record: RecordId,
        fields: Vec<(String, Value)>,
    },
    MakeVariant {
        destination: Value,
        variant: VariantId,
        payload: Vec<Value>,
    },
    LoadField {
        destination: Value,
        object: Value,
        by_reference: bool,
    },
    StoreField {
        object: Value,
        source: Value,
    },
    StoreIndex {
        object: Value,
        index: Value,
        source: Value,
    },
    MakeReference {
        destination: Value,
        pointee_type: ExecutableType,
        object: Value,
        index: Value,
    },
    MakeWholeReference {
        destination: Value,
        pointee_type: ExecutableType,
        object: Value,
    },
    MakeFieldReference {
        field: String,
        destination: Value,
        pointee_type: ExecutableType,
        object: Value,
    },
    MoveOut {
        by_reference: bool,
        destination: Value,
        source: Value,
    },
    Push {
        destination: Value,
        object: Value,
        value: Value,
    },
    Append {
        destination: Value,
        object: Value,
        value: Value,
    },
    Contains {
        destination: Value,
        value: Value,
        candidates: Vec<Value>,
    },
    Builtin {
        destination: Value,
        builtin: Builtin,
        arguments: Vec<Value>,
    },
    SpawnRemote {
        destination: Value,
        value: Value,
    },
    SpawnRemoteBorrow {
        destination: Value,
        source: Value,
    },
    RemoteCall {
        destination: Value,
        remote: Value,
        function: FunctionId,
        arguments: Vec<(crate::ast::ParameterMode, Value)>,
    },
    Await {
        destination: Value,
        future: Value,
    },
    MatchPattern {
        destination: Value,
        subject: Value,
        pattern: crate::hir::Pattern,
        bindings: Vec<Value>,
    },
    Jump {
        target: usize,
    },
    JumpIfFalse {
        condition: Value,
        target: usize,
    },
    Assert {
        condition: Value,
        message: Option<Value>,
    },
    Call {
        destination: Value,
        function: FunctionId,
        specialization: Specialization,
        arguments: Vec<Value>,
    },
    CallMethod {
        destination: Value,
        receiver: Value,
        function: FunctionId,
        specialization: Specialization,
        arguments: Vec<Value>,
    },
    CallContractMethod {
        destination: Value,
        receiver: Value,
        slot: DispatchSlot,
        name: String,
        arguments: Vec<Value>,
        result_type: ExecutableType,
    },
    MakeClosure {
        destination: Value,
        function: FunctionId,
        specialization: Specialization,
        captures: Vec<(crate::hir::CaptureMode, Value)>,
    },
    CallValue {
        destination: Value,
        callee: Value,
        arguments: Vec<Value>,
    },
    CallClosure {
        destination: Value,
        function: FunctionId,
        specialization: Specialization,
        captures: Vec<(crate::hir::CaptureMode, Value)>,
        arguments: Vec<Value>,
    },
    Return {
        source: Value,
    },
    CopyOnWrite {
        destination: Value,
        source: Value,
    },
    Edge {
        target: usize,
        arguments: Vec<(Value, Value)>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FlowState {
    pub(crate) bindings: Vec<Option<ExecutableType>>,
    pending_pattern: Option<PendingPattern>,
    excluded_variants: HashMap<Value, HashSet<VariantId>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingPattern {
    conditions: Vec<Value>,
    bindings: Vec<Value>,
    irrefutable: bool,
    covered_variant: Option<(Vec<Value>, VariantId)>,
}

pub(crate) fn analyze_function_flow(
    program: &Program,
    function: &Body,
) -> Result<Vec<Option<FlowState>>, FosterError> {
    if function.intrinsic_stub {
        return Ok(vec![None; function.instructions.len()]);
    }
    let mut entry = FlowState {
        bindings: vec![None; function.value_count],
        pending_pattern: None,
        excluded_variants: HashMap::new(),
    };
    for (index, ty) in function
        .captures
        .iter()
        .chain(function.parameters.iter().map(|p| &p.ty))
        .enumerate()
    {
        entry.bindings[function
            .entry_values
            .get(index)
            .map_or(index, |v| v.index())] = Some(ty.clone());
    }

    let mut states = vec![None; function.instructions.len()];
    states[0] = Some(entry);
    let mut pending = VecDeque::from([0usize]);
    while let Some(index) = pending.pop_front() {
        let state = states[index]
            .clone()
            .expect("queued bytecode instruction has an entry state");
        let successors = transfer(program, function, index, state)?;
        for (successor, incoming) in successors {
            if successor >= function.instructions.len() {
                return invalid_instruction(
                    function,
                    index,
                    "reachable control flow falls off the end",
                );
            }
            match &mut states[successor] {
                Some(current) => {
                    if merge_state(function, successor, current, &incoming)? {
                        pending.push_back(successor);
                    }
                }
                slot @ None => {
                    *slot = Some(incoming);
                    pending.push_back(successor);
                }
            }
        }
    }
    Ok(states)
}

fn transfer(
    program: &Program,
    function: &Body,
    index: usize,
    mut state: FlowState,
) -> Result<Vec<(usize, FlowState)>, FosterError> {
    let instruction = &function.instructions[index];
    let next = index + 1;
    // Drop insertion can place cleanup between a pattern test and its conditional branch.
    // Preserve the edge fact until the corresponding condition is consumed.
    let pattern = state.pending_pattern.clone();

    match instruction {
        Instruction::CopyOnWrite {
            destination,
            source,
        } => {
            state.bindings[destination.index()] = state.bindings[source.index()].clone();
        }
        Instruction::Edge { target, arguments } => {
            let old = state.clone();
            for (source, destination) in arguments {
                state.bindings[destination.index()] = old.bindings[source.index()].clone();
                state.excluded_variants.remove(destination);
                if let Some(excluded) = old.excluded_variants.get(source) {
                    state
                        .excluded_variants
                        .insert(*destination, excluded.clone());
                }
            }
            if let Some(pattern) = &mut state.pending_pattern {
                let rename = |values: &[Value]| {
                    let mut aliases = Vec::new();
                    for value in values {
                        if !arguments
                            .iter()
                            .any(|(_, destination)| destination == value)
                        {
                            aliases.push(*value);
                        }
                        aliases.extend(arguments.iter().filter_map(|(source, destination)| {
                            (source == value).then_some(*destination)
                        }));
                    }
                    aliases.sort_unstable();
                    aliases.dedup();
                    aliases
                };
                pattern.conditions = rename(&pattern.conditions);
                pattern.bindings = rename(&pattern.bindings);
                if let Some((subjects, _)) = &mut pattern.covered_variant {
                    *subjects = rename(subjects);
                }
            }
            return Ok(vec![(*target, state)]);
        }
        Instruction::Drop { register } => {
            // Liveness drops are deliberately idempotent. A consuming call can empty a
            // register before the cleanup instruction on that edge executes.
            if function.storage_policy == StoragePolicy::ConsumingSlots {
                state.bindings[register.index()] = None;
            }
            state.excluded_variants.remove(register);
        }
        Instruction::LoadConstant {
            destination,
            constant,
        } => write_type(
            function,
            index,
            &mut state,
            *destination,
            constant_type(program, &program.metadata.constants[usize::from(*constant)]),
        )?,
        Instruction::Move {
            destination,
            source,
        } => {
            let ty = if matches!(
                assignment_binding(function, &state, *destination),
                Some(ExecutableType::Reference(_))
            ) {
                read_type(function, index, &state, *source)?
            } else {
                bound_type(function, index, &state, *source)?
            };
            write_type(function, index, &mut state, *destination, ty)?;
        }
        Instruction::Unary {
            destination,
            operator,
            operand,
        } => {
            let operand = read_type(function, index, &state, *operand)?;
            let ty = unary_type(function, index, *operator, operand)?;
            write_type(function, index, &mut state, *destination, ty)?;
        }
        Instruction::Binary {
            destination,
            operator,
            left,
            right,
        } => {
            let left = read_type(function, index, &state, *left)?;
            let right = read_type(function, index, &state, *right)?;
            let ty = binary_type(function, index, *operator, left, right)?;
            write_type(function, index, &mut state, *destination, ty)?;
        }
        Instruction::MakeList {
            destination,
            element_type,
            elements,
        } => {
            let mut element = element_type.clone();
            for register in elements {
                let found = read_type(function, index, &state, *register)?;
                element = merge_types(function, index, &element, &found)?;
            }
            write_type(
                function,
                index,
                &mut state,
                *destination,
                ExecutableType::List(Box::new(element)),
            )?;
        }
        Instruction::Index {
            destination,
            object,
            index: subscript,
        } => {
            let subscript = read_type(function, index, &state, *subscript)?;
            require_type(
                function,
                index,
                &subscript,
                &ExecutableType::Integer,
                "index",
            )?;
            let object_type = read_type(function, index, &state, *object)?;
            let result = match object_type {
                ExecutableType::List(element) => *element,
                ExecutableType::Bytes | ExecutableType::ByteBuffer => ExecutableType::Byte,
                ExecutableType::Unknown => ExecutableType::Unknown,
                ExecutableType::Record { .. } if is_foster_byte_buffer(program, &object_type) => {
                    ExecutableType::Byte
                }
                found => return type_error(function, index, "indexable value", &found),
            };
            write_type(function, index, &mut state, *destination, result)?;
        }
        Instruction::MakeRecord {
            destination,
            record,
            fields,
            ..
        } => {
            for (_, register) in fields {
                read_type(function, index, &state, *register)?;
            }
            write_type(
                function,
                index,
                &mut state,
                *destination,
                record_type(program, *record),
            )?;
        }
        Instruction::MakeVariant {
            destination,
            variant,
            payload,
            ..
        } => {
            for register in payload {
                read_type(function, index, &state, *register)?;
            }
            write_type(
                function,
                index,
                &mut state,
                *destination,
                ExecutableType::Variant {
                    variant: program.metadata.variants[variant].parent,
                    arguments: Vec::new(),
                },
            )?;
        }
        Instruction::LoadField {
            destination,
            object,
            by_reference,
            ..
        } => {
            read_type(function, index, &state, *object)?;
            let ty = if *by_reference {
                ExecutableType::Reference(Box::new(ExecutableType::Unknown))
            } else {
                ExecutableType::Unknown
            };
            write_type(function, index, &mut state, *destination, ty)?;
        }
        Instruction::StoreField { object, source, .. } => {
            let object_type = read_type(function, index, &state, *object)?;
            if !matches!(
                object_type,
                ExecutableType::Record { .. }
                    | ExecutableType::List(_)
                    | ExecutableType::Bytes
                    | ExecutableType::ByteBuffer
                    | ExecutableType::Unknown
            ) {
                return type_error(function, index, "record", &object_type);
            }
            read_type(function, index, &state, *source)?;
        }
        Instruction::StoreIndex {
            object,
            index: subscript,
            source,
        } => {
            let subscript = read_type(function, index, &state, *subscript)?;
            require_type(
                function,
                index,
                &subscript,
                &ExecutableType::Integer,
                "index",
            )?;
            let source_type = read_type(function, index, &state, *source)?;
            let object_type = read_type(function, index, &state, *object)?;
            match object_type {
                ExecutableType::List(element) => {
                    require_type(function, index, &source_type, &element, "list element")?
                }
                ExecutableType::ByteBuffer => require_type(
                    function,
                    index,
                    &source_type,
                    &ExecutableType::Byte,
                    "byte-buffer element",
                )?,
                ExecutableType::Record { .. } if is_foster_byte_buffer(program, &object_type) => {
                    require_type(
                        function,
                        index,
                        &source_type,
                        &ExecutableType::Byte,
                        "byte-buffer element",
                    )?
                }
                ExecutableType::Unknown => {}
                found => return type_error(function, index, "mutable indexed value", &found),
            }
        }
        Instruction::MakeReference {
            destination,
            pointee_type,
            object,
            index: subscript,
        } => {
            let subscript = read_type(function, index, &state, *subscript)?;
            require_type(
                function,
                index,
                &subscript,
                &ExecutableType::Integer,
                "index",
            )?;
            let object_type = read_type(function, index, &state, *object)?;
            let inferred = indexed_element_type(program, &object_type).ok_or_else(|| {
                FosterError::runtime(format!(
                    "bytecode function `{}` instruction {index} has referenceable indexed value type {object_type:?}",
                    function.name
                ))
            })?;
            require_type(
                function,
                index,
                &inferred,
                pointee_type,
                "reference pointee",
            )?;
            write_type(
                function,
                index,
                &mut state,
                *destination,
                ExecutableType::Reference(Box::new(pointee_type.clone())),
            )?;
        }
        Instruction::MakeWholeReference {
            destination,
            pointee_type,
            object,
        } => {
            let value = read_type(function, index, &state, *object)?;
            let inferred = match value {
                ExecutableType::Reference(pointee) => *pointee,
                value => value,
            };
            require_type(
                function,
                index,
                &inferred,
                pointee_type,
                "reference pointee",
            )?;
            write_type(
                function,
                index,
                &mut state,
                *destination,
                ExecutableType::Reference(Box::new(pointee_type.clone())),
            )?;
        }
        Instruction::MakeFieldReference {
            destination,
            pointee_type,
            object,
            field,
        } => {
            let object = read_type(function, index, &state, *object)?;
            let inferred = verification_field_type(program, &object, field).ok_or_else(|| {
                FosterError::runtime(format!(
                    "bytecode function `{}` instruction {index} references missing field `{field}`",
                    function.name
                ))
            })?;
            require_type(
                function,
                index,
                &inferred,
                pointee_type,
                "reference pointee",
            )?;
            write_type(
                function,
                index,
                &mut state,
                *destination,
                ExecutableType::Reference(Box::new(pointee_type.clone())),
            )?;
        }
        Instruction::MoveOut {
            by_reference,
            destination,
            source,
        } => {
            let ty = if *by_reference {
                match bound_type(function, index, &state, *source)? {
                    ExecutableType::Reference(pointee) => *pointee,
                    _ => {
                        return invalid_instruction(
                            function,
                            index,
                            "projected move requires a reference",
                        );
                    }
                }
            } else {
                take_type(function, index, &mut state, *source)?
            };
            write_type(function, index, &mut state, *destination, ty)?;
        }
        Instruction::Push {
            destination,
            object,
            value,
        } => {
            let value = read_type(function, index, &state, *value)?;
            let object_type = read_type(function, index, &state, *object)?;
            match object_type {
                ExecutableType::List(element) => {
                    require_type(function, index, &value, &element, "list element")?
                }
                ExecutableType::ByteBuffer => require_type(
                    function,
                    index,
                    &value,
                    &ExecutableType::Byte,
                    "byte-buffer element",
                )?,
                ExecutableType::Record { .. } if is_foster_byte_buffer(program, &object_type) => {
                    require_type(
                        function,
                        index,
                        &value,
                        &ExecutableType::Byte,
                        "byte-buffer element",
                    )?
                }
                ExecutableType::Unknown => {}
                found => return type_error(function, index, "List or ByteBuffer", &found),
            }
            write_type(
                function,
                index,
                &mut state,
                *destination,
                ExecutableType::Unit,
            )?;
        }
        Instruction::Append {
            destination,
            object,
            value,
        } => {
            let value = read_type(function, index, &state, *value)?;
            let result = match read_type(function, index, &state, *object)? {
                ExecutableType::List(element) => {
                    require_type(function, index, &value, &element, "list element")?;
                    ExecutableType::List(element)
                }
                ExecutableType::Unknown => ExecutableType::Unknown,
                found => return type_error(function, index, "List", &found),
            };
            write_type(function, index, &mut state, *destination, result)?;
        }
        Instruction::Contains {
            destination,
            value,
            candidates,
        } => {
            let value = read_type(function, index, &state, *value)?;
            for candidate in candidates {
                let candidate = read_type(function, index, &state, *candidate)?;
                require_type(function, index, &candidate, &value, "candidate")?;
            }
            write_type(
                function,
                index,
                &mut state,
                *destination,
                ExecutableType::Bool,
            )?;
        }
        Instruction::Builtin {
            destination,
            builtin,
            arguments,
        } => {
            let signature = builtin.descriptor().signature;
            if !signature.accepts_arity(arguments.len()) {
                return invalid_instruction(function, index, "has invalid builtin arity");
            }
            for (argument_index, argument) in arguments.iter().enumerate() {
                let parameter = signature
                    .parameter(argument_index)
                    .expect("accepted builtin arity has a parameter");
                let expected = intrinsic_verification_type(program, parameter.ty);
                let found = read_type(function, index, &state, *argument)?;
                require_type(function, index, &found, &expected, "builtin argument")?;
            }
            for (argument_index, argument) in arguments.iter().enumerate() {
                let parameter = signature
                    .parameter(argument_index)
                    .expect("accepted builtin arity has a parameter");
                if parameter.mode == IntrinsicArgumentMode::Consume {
                    take_type(function, index, &mut state, *argument)?;
                }
            }
            write_type(
                function,
                index,
                &mut state,
                *destination,
                intrinsic_verification_type(program, signature.result),
            )?;
        }
        Instruction::SpawnRemote { destination, value } => {
            let value = read_type(function, index, &state, *value)?;
            write_type(
                function,
                index,
                &mut state,
                *destination,
                ExecutableType::Remote(Box::new(value)),
            )?;
        }
        Instruction::SpawnRemoteBorrow {
            destination,
            source,
        } => {
            let value = read_type(function, index, &state, *source)?;
            write_type(
                function,
                index,
                &mut state,
                *destination,
                ExecutableType::Remote(Box::new(value)),
            )?;
        }
        Instruction::RemoteCall {
            destination,
            remote,
            function: target,
            arguments,
        } => {
            let target = &program.functions[target];
            let receiver = match read_type(function, index, &state, *remote)? {
                ExecutableType::Remote(value) => *value,
                ExecutableType::Unknown => ExecutableType::Unknown,
                found => return type_error(function, index, "Remote", &found),
            };
            let mut substitutions = std::collections::BTreeMap::new();
            target.parameters[0]
                .ty
                .infer_specialization(&receiver, &mut substitutions);
            for (schema, (_, argument)) in target
                .parameters
                .iter()
                .map(|p| &p.ty)
                .skip(1)
                .zip(arguments)
            {
                schema.infer_specialization(
                    &read_type(function, index, &state, *argument)?,
                    &mut substitutions,
                );
            }
            let specialization = substitutions.into();
            let parameter_types = target
                .parameters
                .iter()
                .map(|p| &p.ty)
                .map(|ty| ty.specialize(&specialization))
                .collect::<Vec<_>>();
            require_type(
                function,
                index,
                &receiver,
                &parameter_types[0],
                "remote receiver",
            )?;
            verify_arguments(
                function,
                index,
                &mut state,
                arguments.iter().map(|(mode, register)| (*mode, *register)),
                target.parameters.iter().map(|p| p.mode).skip(1),
                parameter_types.iter().skip(1),
            )?;
            write_type(
                function,
                index,
                &mut state,
                *destination,
                ExecutableType::Future(Box::new(
                    program
                        .metadata
                        .remote_outcome_type(target.result_type.specialize(&specialization)),
                )),
            )?;
        }
        Instruction::Await {
            destination,
            future,
        } => {
            let result = match read_type(function, index, &state, *future)? {
                ExecutableType::Future(value) => *value,
                ExecutableType::Unknown => ExecutableType::Unknown,
                found => return type_error(function, index, "Future", &found),
            };
            write_type(function, index, &mut state, *destination, result)?;
        }
        Instruction::MatchPattern {
            destination,
            subject,
            bindings,
            pattern,
        } => {
            let subject_type = read_type(function, index, &state, *subject)?;
            let covered_variant = fully_covered_variant(pattern).map(|variant| (*subject, variant));
            let exhaustive = covered_variant.is_some_and(|(_, variant)| {
                let parent = program.metadata.variants[&variant].parent;
                program
                    .metadata
                    .variants
                    .iter()
                    .filter(|(_, metadata)| metadata.parent == parent)
                    .all(|(candidate, _)| {
                        *candidate == variant
                            || state
                                .excluded_variants
                                .get(subject)
                                .is_some_and(|excluded| excluded.contains(candidate))
                    })
            });
            if let crate::hir::Pattern::Variant { variant, .. } = pattern.unspanned()
                && let ExecutableType::Variant {
                    variant: parent, ..
                } = subject_type
                && program.metadata.variants[variant].parent != parent
            {
                return invalid_instruction(
                    function,
                    index,
                    "matches a variant case against the wrong enum type",
                );
            }
            write_type(
                function,
                index,
                &mut state,
                *destination,
                ExecutableType::Bool,
            )?;
            state.pending_pattern = Some(PendingPattern {
                conditions: vec![*destination],
                bindings: bindings.clone(),
                irrefutable: pattern_irrefutable(pattern) || exhaustive,
                covered_variant: covered_variant.map(|(subject, variant)| (vec![subject], variant)),
            });
        }
        Instruction::Jump { target } => return Ok(vec![(*target, state)]),
        Instruction::JumpIfFalse { condition, target } => {
            let found = read_type(function, index, &state, *condition)?;
            require_type(function, index, &found, &ExecutableType::Bool, "condition")?;
            let mut truthy = state.clone();
            state.pending_pattern = None;
            truthy.pending_pattern = None;
            let pattern = pattern.filter(|pattern| pattern.conditions.contains(condition));
            if let Some(pattern) = &pattern {
                for binding in &pattern.bindings {
                    write_type(
                        function,
                        index,
                        &mut truthy,
                        *binding,
                        ExecutableType::Unknown,
                    )?;
                }
                if let Some((subjects, variant)) = &pattern.covered_variant {
                    for subject in subjects {
                        state
                            .excluded_variants
                            .entry(*subject)
                            .or_default()
                            .insert(*variant);
                    }
                }
            }
            return if pattern.is_some_and(|pattern| pattern.irrefutable) {
                Ok(vec![(next, truthy)])
            } else {
                Ok(vec![(*target, state), (next, truthy)])
            };
        }
        Instruction::Assert { condition, message } => {
            let found = read_type(function, index, &state, *condition)?;
            require_type(function, index, &found, &ExecutableType::Bool, "condition")?;
            if let Some(message) = message {
                let expected = program
                    .metadata
                    .string_record
                    .map(nominal_record)
                    .unwrap_or(ExecutableType::Unknown);
                let found = read_type(function, index, &state, *message)?;
                require_type(function, index, &found, &expected, "assertion message")?;
            }
        }
        Instruction::Call {
            destination,
            function: target,
            arguments,
            specialization,
        } => {
            let target = &program.functions[target];
            let parameter_types = target
                .parameters
                .iter()
                .map(|p| &p.ty)
                .map(|ty| ty.specialize(specialization))
                .collect::<Vec<_>>();
            verify_arguments(
                function,
                index,
                &mut state,
                target
                    .parameters
                    .iter()
                    .map(|p| p.mode)
                    .zip(arguments.iter().copied()),
                target.parameters.iter().map(|p| p.mode),
                parameter_types.iter(),
            )?;
            write_type(
                function,
                index,
                &mut state,
                *destination,
                target.result_type.specialize(specialization),
            )?;
        }
        Instruction::CallMethod {
            destination,
            receiver,
            function: target,
            arguments,
            specialization,
        } => {
            let target = &program.functions[target];
            let parameter_types = target
                .parameters
                .iter()
                .map(|p| &p.ty)
                .map(|ty| ty.specialize(specialization))
                .collect::<Vec<_>>();
            let receiver = read_type(function, index, &state, *receiver)?;
            require_type(
                function,
                index,
                &receiver,
                &parameter_types[0],
                "method receiver",
            )?;
            verify_arguments(
                function,
                index,
                &mut state,
                target
                    .parameters
                    .iter()
                    .map(|p| p.mode)
                    .skip(1)
                    .zip(arguments.iter().copied()),
                target.parameters.iter().map(|p| p.mode).skip(1),
                parameter_types.iter().skip(1),
            )?;
            write_type(
                function,
                index,
                &mut state,
                *destination,
                target.result_type.specialize(specialization),
            )?;
        }
        Instruction::CallContractMethod {
            destination,
            receiver,
            slot,
            arguments,
            result_type,
            name,
            ..
        } => {
            let receiver_type = read_type(function, index, &state, *receiver)?;
            if *slot == crate::types::DEINIT_SLOT {
                return invalid_instruction(
                    function,
                    index,
                    "deinit can only be invoked by ownership cleanup",
                );
            }
            if *slot == crate::types::CAN_COPY_SLOT || *slot == crate::types::COPY_SLOT {
                if !arguments.is_empty() {
                    return invalid_instruction(
                        function,
                        index,
                        "copy capability does not accept arguments",
                    );
                }
                let result = if *slot == crate::types::CAN_COPY_SLOT {
                    ExecutableType::Bool
                } else {
                    receiver_type
                };
                write_type(function, index, &mut state, *destination, result)?;
                return Ok(vec![(index + 1, state)]);
            }
            let nominal = match &receiver_type {
                ExecutableType::Record { record, .. } => Some(NominalTypeId::Record(*record)),
                ExecutableType::Variant { variant, .. } => Some(NominalTypeId::Variant(*variant)),
                _ => None,
            };
            if let Some(target) = nominal
                .and_then(|nominal| program.metadata.dispatch.get(&(nominal, *slot)))
                .and_then(|target| program.functions.get(target))
            {
                let mut substitutions = std::collections::BTreeMap::new();
                if let Some(parameter) = target.parameters.first().map(|p| &p.ty) {
                    parameter.infer_specialization(&receiver_type, &mut substitutions);
                }
                for (parameter, argument) in target
                    .parameters
                    .iter()
                    .map(|p| &p.ty)
                    .skip(1)
                    .zip(arguments)
                {
                    parameter.infer_specialization(
                        &read_type(function, index, &state, *argument)?,
                        &mut substitutions,
                    );
                }
                let concrete = target.result_type.specialize(&substitutions.into());
                require_type(function, index, &concrete, result_type, "contract result")?;
                verify_arguments(
                    function,
                    index,
                    &mut state,
                    target
                        .parameters
                        .iter()
                        .map(|p| p.mode)
                        .skip(1)
                        .zip(arguments.iter().copied()),
                    target.parameters.iter().map(|p| p.mode).skip(1),
                    target.parameters.iter().map(|p| &p.ty).skip(1),
                )?;
                write_type(
                    function,
                    index,
                    &mut state,
                    *destination,
                    result_type.clone(),
                )?;
            } else {
                if arguments.is_empty()
                    && let Some(concrete) = verification_field_type(program, &receiver_type, name)
                {
                    require_type(
                        function,
                        index,
                        &concrete,
                        result_type,
                        "contract accessor result",
                    )?;
                }
                for argument in arguments {
                    read_type(function, index, &state, *argument)?;
                }
                write_type(
                    function,
                    index,
                    &mut state,
                    *destination,
                    result_type.clone(),
                )?;
            }
        }
        Instruction::MakeClosure {
            destination,
            function: target,
            specialization,
            captures,
        } => {
            let target = &program.functions[target];
            let capture_types = target
                .captures
                .iter()
                .map(|ty| ty.specialize(specialization))
                .collect::<Vec<_>>();
            verify_captures(function, index, &mut state, captures, &capture_types)?;
            write_type(
                function,
                index,
                &mut state,
                *destination,
                callable_type(target),
            )?;
        }
        Instruction::CallValue {
            destination,
            callee,
            arguments,
        } => {
            let callee = read_type(function, index, &state, *callee)?;
            let ExecutableType::Function { parameters, result } = callee else {
                if callee == ExecutableType::Unknown {
                    for argument in arguments {
                        read_type(function, index, &state, *argument)?;
                    }
                    write_type(
                        function,
                        index,
                        &mut state,
                        *destination,
                        ExecutableType::Unknown,
                    )?;
                    return Ok(vec![(next, state)]);
                }
                return type_error(function, index, "callable value", &callee);
            };
            if arguments.len() != parameters.len() {
                return invalid_instruction(
                    function,
                    index,
                    "calls a closure with the wrong arity",
                );
            }
            verify_arguments(
                function,
                index,
                &mut state,
                parameters
                    .iter()
                    .map(|p| p.mode)
                    .zip(arguments.iter().copied()),
                parameters.iter().map(|p| p.mode),
                parameters.iter().map(|p| &p.ty),
            )?;
            write_type(function, index, &mut state, *destination, *result)?;
        }
        Instruction::CallClosure {
            destination,
            function: target,
            specialization,
            captures,
            arguments,
        } => {
            let target = &program.functions[target];
            let capture_types = target
                .captures
                .iter()
                .map(|ty| ty.specialize(specialization))
                .collect::<Vec<_>>();
            let parameter_types = target
                .parameters
                .iter()
                .map(|p| &p.ty)
                .map(|ty| ty.specialize(specialization))
                .collect::<Vec<_>>();
            verify_captures(function, index, &mut state, captures, &capture_types)?;
            verify_arguments(
                function,
                index,
                &mut state,
                target
                    .parameters
                    .iter()
                    .map(|p| p.mode)
                    .zip(arguments.iter().copied()),
                target.parameters.iter().map(|p| p.mode),
                parameter_types.iter(),
            )?;
            write_type(
                function,
                index,
                &mut state,
                *destination,
                target.result_type.specialize(specialization),
            )?;
        }
        Instruction::Return { source } => {
            let actual = if function.returns_reference {
                bound_type(function, index, &state, *source)?
            } else {
                read_type(function, index, &state, *source)?
            };
            require_type(
                function,
                index,
                &actual,
                &function.result_type,
                "return value",
            )?;
            return Ok(Vec::new());
        }
    }
    Ok(vec![(next, state)])
}

fn verification_field_type(
    program: &Program,
    receiver: &ExecutableType,
    field: &str,
) -> Option<ExecutableType> {
    match receiver {
        ExecutableType::Reference(pointee) => verification_field_type(program, pointee, field),
        ExecutableType::Record { record, arguments } => {
            let metadata = program.metadata.records.get(record)?;
            let index = metadata
                .layout()
                .names()
                .iter()
                .position(|name| name == field)?;
            let substitutions = metadata
                .parameters
                .iter()
                .cloned()
                .zip(arguments.iter().cloned())
                .collect::<HashMap<_, _>>();
            Some(metadata.fields().get(index)?.ty.substitute(&substitutions))
        }
        ExecutableType::List(element) => match field {
            "empty?" => Some(ExecutableType::Bool),
            "length" => Some(ExecutableType::Integer),
            "head" => Some((**element).clone()),
            "rest" => Some(receiver.clone()),
            _ => None,
        },
        ExecutableType::Unknown => Some(ExecutableType::Unknown),
        _ => None,
    }
}

fn verify_arguments<'a>(
    function: &Body,
    index: usize,
    state: &mut FlowState,
    actual: impl Iterator<Item = (ParameterMode, Value)>,
    expected_modes: impl Iterator<Item = ParameterMode>,
    expected_types: impl Iterator<Item = &'a ExecutableType>,
) -> Result<(), FosterError> {
    let actual = actual.collect::<Vec<_>>();
    let modes = expected_modes.collect::<Vec<_>>();
    let types = expected_types.collect::<Vec<_>>();
    if actual.len() != modes.len() || actual.len() != types.len() {
        return invalid_instruction(function, index, "has an invalid argument layout");
    }
    for (((encoded_mode, register), expected_mode), expected_type) in
        actual.iter().zip(&modes).zip(&types)
    {
        if encoded_mode != expected_mode {
            return invalid_instruction(
                function,
                index,
                "has inconsistent argument ownership modes",
            );
        }
        let bound = bound_type(function, index, state, *register)?;
        let found = match expected_mode {
            // A borrowed reference parameter binds the wrapper itself. Other borrowed
            // parameters observe the VM's ordinary read-through-reference semantics.
            ParameterMode::Borrow if compatible(&bound, expected_type) => bound,
            ParameterMode::Borrow if matches!(expected_type, ExecutableType::Reference(value) if compatible(&bound, value)) =>
            {
                // Whole-place reference construction may be optimized into passing the
                // underlying register; call-frame promotion recreates the same place binding.
                ExecutableType::Reference(Box::new(bound))
            }
            ParameterMode::Borrow => readable_type(function, index, bound)?,
            ParameterMode::Consume => bound,
        };
        require_type(function, index, &found, expected_type, "call argument")?;
    }
    for ((_, register), mode) in actual.into_iter().zip(modes) {
        if mode == ParameterMode::Consume {
            take_type(function, index, state, register)?;
        }
    }
    Ok(())
}

fn verify_captures(
    function: &Body,
    index: usize,
    state: &mut FlowState,
    captures: &[(CaptureMode, Value)],
    expected: &[ExecutableType],
) -> Result<(), FosterError> {
    if captures.len() != expected.len() {
        return invalid_instruction(function, index, "has an invalid capture layout");
    }
    for ((mode, register), expected) in captures.iter().zip(expected) {
        let found = match mode {
            CaptureMode::Move => bound_type(function, index, state, *register)?,
            CaptureMode::Copy | CaptureMode::Pending => {
                read_type(function, index, state, *register)?
            }
            CaptureMode::Ref => match bound_type(function, index, state, *register)? {
                reference @ ExecutableType::Reference(_) => reference,
                value => ExecutableType::Reference(Box::new(value)),
            },
        };
        require_type(function, index, &found, expected, "closure capture")?;
    }
    for (mode, register) in captures {
        if *mode == CaptureMode::Move {
            take_type(function, index, state, *register)?;
        }
    }
    Ok(())
}

fn bound_type(
    function: &Body,
    index: usize,
    state: &FlowState,
    register: Value,
) -> Result<ExecutableType, FosterError> {
    state.bindings[register.index()].clone().ok_or_else(|| {
        FosterError::runtime(format!(
            "bytecode function `{}` instruction {index} reads unavailable r{} in {:?}",
            function.name, register.0, function.instructions[index]
        ))
    })
}

fn read_type(
    function: &Body,
    index: usize,
    state: &FlowState,
    register: Value,
) -> Result<ExecutableType, FosterError> {
    readable_type(
        function,
        index,
        bound_type(function, index, state, register)?,
    )
}

fn readable_type(
    function: &Body,
    index: usize,
    ty: ExecutableType,
) -> Result<ExecutableType, FosterError> {
    match ty {
        ExecutableType::Reference(value) => Ok(*value),
        ExecutableType::Alternatives(members) => {
            let mut members = members.into_iter();
            let Some(first) = members.next() else {
                return Ok(ExecutableType::Unknown);
            };
            let mut result = readable_type(function, index, first)?;
            for member in members {
                result = merge_types(
                    function,
                    index,
                    &result,
                    &readable_type(function, index, member)?,
                )?;
            }
            Ok(result)
        }
        value => Ok(value),
    }
}

fn take_type(
    function: &Body,
    index: usize,
    state: &mut FlowState,
    register: Value,
) -> Result<ExecutableType, FosterError> {
    if function.storage_policy == StoragePolicy::ImmutableValues {
        return bound_type(function, index, state, register);
    }
    state.excluded_variants.remove(&register);
    state.bindings[register.index()].take().ok_or_else(|| {
        FosterError::runtime(format!(
            "bytecode function `{}` instruction {index} consumes unavailable r{} in {:?}",
            function.name, register.0, function.instructions[index]
        ))
    })
}

fn assignment_binding<'a>(
    function: &Body,
    state: &'a FlowState,
    destination: Value,
) -> Option<&'a ExecutableType> {
    let previous = if function.storage_policy == StoragePolicy::ImmutableValues {
        *function.write_bindings.get(&destination)?
    } else {
        destination
    };
    state.bindings[previous.index()].as_ref()
}

fn write_type(
    function: &Body,
    index: usize,
    state: &mut FlowState,
    register: Value,
    value: ExecutableType,
) -> Result<(), FosterError> {
    state.excluded_variants.remove(&register);
    let previous = assignment_binding(function, state, register).cloned();
    if let Some(ExecutableType::Reference(target)) = previous {
        require_type(function, index, &value, &target, "reference assignment")?;
        state.bindings[register.index()] = Some(ExecutableType::Reference(target));
    } else {
        state.bindings[register.index()] = Some(value);
    }
    Ok(())
}

fn merge_state(
    function: &Body,
    index: usize,
    current: &mut FlowState,
    incoming: &FlowState,
) -> Result<bool, FosterError> {
    let mut changed = false;
    for (left, right) in current.bindings.iter_mut().zip(&incoming.bindings) {
        let merged = match (&*left, right) {
            (Some(left_type), Some(right_type)) => {
                // Value coloring can reuse one physical register for unrelated values on
                // disjoint predecessors. An incompatible join becomes unavailable; any later
                // read is then rejected by definite-initialization checking.
                merge_types(function, index, left_type, right_type).ok()
            }
            _ => None,
        };
        if *left != merged {
            *left = merged;
            changed = true;
        }
    }
    let pending = (current.pending_pattern == incoming.pending_pattern)
        .then(|| current.pending_pattern.clone())
        .flatten();
    if current.pending_pattern != pending {
        current.pending_pattern = pending;
        changed = true;
    }
    let keys = current
        .excluded_variants
        .keys()
        .copied()
        .collect::<Vec<_>>();
    for register in keys {
        let Some(incoming) = incoming.excluded_variants.get(&register) else {
            current.excluded_variants.remove(&register);
            changed = true;
            continue;
        };
        let (before, after) = {
            let excluded = current.excluded_variants.get_mut(&register).unwrap();
            let before = excluded.len();
            excluded.retain(|variant| incoming.contains(variant));
            (before, excluded.len())
        };
        if after == 0 {
            current.excluded_variants.remove(&register);
        }
        if after != before {
            changed = true;
        }
    }
    Ok(changed)
}

fn merge_types(
    _function: &Body,
    _index: usize,
    left: &ExecutableType,
    right: &ExecutableType,
) -> Result<ExecutableType, FosterError> {
    if left == right {
        return Ok(left.clone());
    }
    if matches!(left, ExecutableType::Unknown) || matches!(right, ExecutableType::Unknown) {
        return Ok(ExecutableType::Unknown);
    }
    match (left, right) {
        (ExecutableType::List(left), ExecutableType::List(right)) => Ok(ExecutableType::List(
            Box::new(merge_types(_function, _index, left, right)?),
        )),
        (ExecutableType::Reference(left), ExecutableType::Reference(right)) => Ok(
            ExecutableType::Reference(Box::new(merge_types(_function, _index, left, right)?)),
        ),
        (ExecutableType::Remote(left), ExecutableType::Remote(right)) => Ok(
            ExecutableType::Remote(Box::new(merge_types(_function, _index, left, right)?)),
        ),
        (ExecutableType::Future(left), ExecutableType::Future(right)) => Ok(
            ExecutableType::Future(Box::new(merge_types(_function, _index, left, right)?)),
        ),
        _ => Ok(ExecutableType::alternatives(vec![
            left.clone(),
            right.clone(),
        ])),
    }
}

fn require_type(
    function: &Body,
    index: usize,
    found: &ExecutableType,
    expected: &ExecutableType,
    role: &str,
) -> Result<(), FosterError> {
    if compatible(found, expected) {
        Ok(())
    } else {
        Err(FosterError::runtime(format!(
            "bytecode function `{}` instruction {index} has {role} type {found:?}, expected {expected:?}",
            function.name
        )))
    }
}

pub(crate) fn compatible(found: &ExecutableType, expected: &ExecutableType) -> bool {
    if found == expected
        || matches!(found, ExecutableType::Unknown | ExecutableType::Generic(_))
        || matches!(
            expected,
            ExecutableType::Unknown | ExecutableType::Generic(_)
        )
    {
        return true;
    }
    match (found, expected) {
        (ExecutableType::Alternatives(found), expected) => {
            found.iter().all(|found| compatible(found, expected))
        }
        (found, ExecutableType::Alternatives(expected)) => {
            expected.iter().any(|expected| compatible(found, expected))
        }
        (ExecutableType::CodePoint | ExecutableType::Byte, ExecutableType::Integer) => true,
        // Structural record and variant conformance is resolved before bytecode lowering. The
        // verification vocabulary retains runtime representation, not the source contract proof.
        (ExecutableType::Record { .. }, ExecutableType::Record { .. })
        | (ExecutableType::Variant { .. }, ExecutableType::Variant { .. }) => true,
        (ExecutableType::List(found), ExecutableType::List(expected))
        | (ExecutableType::Reference(found), ExecutableType::Reference(expected))
        | (ExecutableType::Remote(found), ExecutableType::Remote(expected))
        | (ExecutableType::Future(found), ExecutableType::Future(expected)) => {
            compatible(found, expected)
        }
        (
            ExecutableType::Function {
                parameters: found_parameters,
                result: found_result,
            },
            ExecutableType::Function {
                parameters: expected_parameters,
                result: expected_result,
            },
        ) => {
            // Callable representation erasure can adapt source ownership modes while the
            // closure still carries its concrete callee modes for execution.
            found_parameters.len() == expected_parameters.len()
                && found_parameters
                    .iter()
                    .zip(expected_parameters)
                    .all(|(found, expected)| compatible(&found.ty, &expected.ty))
                && compatible(found_result, expected_result)
        }
        _ => false,
    }
}

fn unary_type(
    function: &Body,
    index: usize,
    operator: UnaryOp,
    operand: ExecutableType,
) -> Result<ExecutableType, FosterError> {
    Ok(match (operator, operand) {
        (UnaryOp::Negate, ExecutableType::Float) => ExecutableType::Float,
        (
            UnaryOp::Negate,
            ExecutableType::Integer | ExecutableType::CodePoint | ExecutableType::Byte,
        ) => ExecutableType::Integer,
        (UnaryOp::Not, ExecutableType::Bool) => ExecutableType::Bool,
        (UnaryOp::BitNot, ExecutableType::Byte) => ExecutableType::Byte,
        (_, ExecutableType::Unknown) => ExecutableType::Unknown,
        (_, found) => return type_error(function, index, "valid unary operand", &found),
    })
}

fn binary_type(
    function: &Body,
    index: usize,
    operator: BinaryOp,
    left: ExecutableType,
    right: ExecutableType,
) -> Result<ExecutableType, FosterError> {
    use BinaryOp::*;
    if matches!(operator, Equal | NotEqual) {
        if !(is_integer_like(&left) && is_integer_like(&right)) {
            require_type(function, index, &left, &right, "binary operand")?;
        }
        return Ok(ExecutableType::Bool);
    }
    if left == ExecutableType::Unknown || right == ExecutableType::Unknown {
        return Ok(ExecutableType::Unknown);
    }
    if matches!(operator, BitAnd | BitOr | BitXor)
        && left == ExecutableType::Byte
        && right == ExecutableType::Byte
    {
        return Ok(ExecutableType::Byte);
    }
    if matches!(operator, ShiftLeft | ShiftRight)
        && left == ExecutableType::Byte
        && right == ExecutableType::Integer
    {
        return Ok(ExecutableType::Byte);
    }
    if is_integer_like(&left) && is_integer_like(&right) {
        return Ok(
            if matches!(operator, Less | LessEqual | Greater | GreaterEqual) {
                ExecutableType::Bool
            } else if matches!(operator, Add | Subtract | Multiply | Divide) {
                ExecutableType::Integer
            } else {
                return type_error(function, index, "valid integer operation", &left);
            },
        );
    }
    if left == ExecutableType::Float && right == ExecutableType::Float {
        return Ok(
            if matches!(operator, Less | LessEqual | Greater | GreaterEqual) {
                ExecutableType::Bool
            } else if matches!(operator, Add | Subtract | Multiply | Divide) {
                ExecutableType::Float
            } else {
                return type_error(function, index, "valid float operation", &left);
            },
        );
    }
    if operator == Add && left == right {
        return Ok(left);
    }
    Err(FosterError::runtime(format!(
        "bytecode function `{}` instruction {index} applies {operator:?} to incompatible types {left:?} and {right:?}",
        function.name
    )))
}

fn is_integer_like(ty: &ExecutableType) -> bool {
    matches!(
        ty,
        ExecutableType::Integer | ExecutableType::CodePoint | ExecutableType::Byte
    )
}

fn nominal_record(record: crate::hir::RecordId) -> ExecutableType {
    ExecutableType::Record {
        record,
        arguments: Vec::new(),
    }
}

fn constant_type(program: &Program, constant: &Constant) -> ExecutableType {
    match constant {
        Constant::Unit => ExecutableType::Unit,
        Constant::Bool(_) => ExecutableType::Bool,
        Constant::Integer(_) => ExecutableType::Integer,
        Constant::Float(_) => ExecutableType::Float,
        Constant::String(_) => program
            .metadata
            .string_record
            .map(nominal_record)
            .unwrap_or(ExecutableType::Unknown),
        Constant::CodePoint(_) => ExecutableType::CodePoint,
        Constant::Symbol(_) => program
            .metadata
            .symbol_record
            .map(nominal_record)
            .unwrap_or(ExecutableType::Unknown),
    }
}

fn record_type(program: &Program, record: crate::hir::RecordId) -> ExecutableType {
    match Some(record) {
        id if id == program.metadata.list_record => {
            ExecutableType::List(Box::new(ExecutableType::Unknown))
        }
        id if id == program.metadata.bytes_record => ExecutableType::Bytes,
        _ => nominal_record(record),
    }
}

fn is_foster_byte_buffer(program: &Program, ty: &ExecutableType) -> bool {
    let ExecutableType::Record { record, .. } = ty else {
        return false;
    };
    Some(*record) == program.metadata.byte_buffer_record
}

fn indexed_element_type(program: &Program, ty: &ExecutableType) -> Option<ExecutableType> {
    match ty {
        ExecutableType::Reference(pointee) => indexed_element_type(program, pointee),
        _ if is_foster_byte_buffer(program, ty) => Some(ExecutableType::Byte),
        _ => ty.indexed_element(),
    }
}

fn callable_type(function: &FunctionSchema) -> ExecutableType {
    ExecutableType::Function {
        parameters: function.parameters.clone(),
        result: Box::new(function.result_type.clone()),
    }
}

fn intrinsic_verification_type(program: &Program, ty: IntrinsicType) -> ExecutableType {
    match ty {
        IntrinsicType::Any => ExecutableType::Unknown,
        IntrinsicType::Unit => ExecutableType::Unit,
        IntrinsicType::Bool => ExecutableType::Bool,
        IntrinsicType::Integer => ExecutableType::Integer,
        IntrinsicType::Float => ExecutableType::Float,
        IntrinsicType::CodePoint => ExecutableType::CodePoint,
        IntrinsicType::Byte => ExecutableType::Byte,
        IntrinsicType::Bytes => ExecutableType::Bytes,
        IntrinsicType::ByteBuffer => ExecutableType::ByteBuffer,
        IntrinsicType::String => program
            .metadata
            .string_record
            .map(nominal_record)
            .unwrap_or(ExecutableType::Unknown),
        IntrinsicType::ListByte => ExecutableType::List(Box::new(ExecutableType::Byte)),
    }
}

fn pattern_irrefutable(pattern: &crate::hir::Pattern) -> bool {
    matches!(
        pattern.unspanned(),
        crate::hir::Pattern::Wildcard | crate::hir::Pattern::Binding(_)
    )
}

fn fully_covered_variant(pattern: &crate::hir::Pattern) -> Option<VariantId> {
    let crate::hir::Pattern::Variant { variant, fields } = pattern.unspanned() else {
        return None;
    };
    fields.iter().all(pattern_irrefutable).then_some(*variant)
}

fn invalid_instruction<T>(
    function: &Body,
    index: usize,
    message: impl std::fmt::Display,
) -> Result<T, FosterError> {
    Err(FosterError::runtime(format!(
        "bytecode function `{}` instruction {index} {message}",
        function.name
    )))
}

fn type_error<T>(
    function: &Body,
    index: usize,
    expected: &str,
    found: &ExecutableType,
) -> Result<T, FosterError> {
    invalid_instruction(
        function,
        index,
        format!("requires {expected}, found {found:?}"),
    )
}
