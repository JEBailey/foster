use std::collections::{HashMap, HashSet, VecDeque};

use crate::ast::{BinaryOp, ParameterMode, UnaryOp};
use crate::error::FosterError;
use crate::hir::{CaptureMode, FunctionId, VariantId};
use crate::intrinsics::{IntrinsicArgumentMode, IntrinsicType};
use crate::types::NominalTypeId;

use super::{BytecodeFunction, Constant, Instruction, Program, Register};
use crate::codegen::types::ExecutableType;

pub fn verify(program: &Program) -> Result<(), FosterError> {
    verify_program_metadata(program)?;
    for (id, function) in &program.functions {
        verify_function_structure(program, *id, function)?;
    }
    for (id, function) in &program.functions {
        verify_function_flow(program, *id, function)?;
    }
    program.symbols.validate(program)?;
    Ok(())
}

fn verify_program_metadata(program: &Program) -> Result<(), FosterError> {
    if let Some(main) = program.main {
        let main = program
            .functions
            .get(&main)
            .ok_or_else(|| FosterError::runtime("bytecode references a missing `main` function"))?;
        let expected = u16::from(program.main_arguments);
        if main.parameters != expected || main.captures != 0 {
            return Err(FosterError::runtime(format!(
                "bytecode `main` must have {expected} parameter(s) and no captures"
            )));
        }
    } else if program.main_arguments {
        return Err(FosterError::runtime(
            "bytecode without `main` cannot accept command arguments",
        ));
    }
    let remote_used = program.functions.values().any(|function| {
        function.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::RemoteCall { .. } | Instruction::Await { .. }
            )
        })
    });
    if remote_used || program.remote_result.is_some() || program.remote_error.is_some() {
        let invalid = || FosterError::runtime("bytecode has invalid remote outcome metadata");
        let result = program.remote_result.ok_or_else(invalid)?;
        let error = program.remote_error.ok_or_else(invalid)?;
        if result == error {
            return Err(invalid());
        }
        for (parent, name, parameters, cases) in [
            (result, "Result", 2, vec![("Ok", 1), ("Error", 1)]),
            (
                error,
                "RemoteError",
                0,
                vec![("Failed", 1), ("Shutdown", 0)],
            ),
        ] {
            let variants = program
                .variants
                .values()
                .filter(|variant| variant.parent == parent)
                .collect::<Vec<_>>();
            if variants.len() != cases.len() {
                return Err(invalid());
            }
            for (case, arity) in cases {
                let variant = variants
                    .iter()
                    .find(|variant| variant.alternative.as_ref() == case)
                    .ok_or_else(invalid)?;
                if variant.type_name.as_ref() != name
                    || variant.parameters.len() != parameters
                    || variant.payload.len() != arity
                {
                    return Err(invalid());
                }
                let expected = match case {
                    "Ok" => Some(ExecutableType::Generic(variant.parameters[0].clone())),
                    "Error" => Some(ExecutableType::Generic(variant.parameters[1].clone())),
                    "Failed" => Some(ExecutableType::Record {
                        record: program.string_record.ok_or_else(invalid)?,
                        arguments: vec![],
                    }),
                    _ => None,
                };
                if variant.payload.first() != expected.as_ref() {
                    return Err(invalid());
                }
            }
        }
    }
    for record in [
        program.string_record,
        program.symbol_record,
        program.list_record,
        program.bytes_record,
        program.byte_buffer_record,
    ]
    .into_iter()
    .flatten()
    {
        if !program.records.contains_key(&record) {
            return Err(FosterError::runtime(
                "bytecode wrapper metadata references a missing record",
            ));
        }
    }
    for record in program.records.values() {
        if record.layout.names().len() != record.field_types.len() {
            return Err(FosterError::runtime(format!(
                "bytecode record `{}` has inconsistent typed field metadata",
                record.name
            )));
        }
        for ty in &record.field_types {
            verify_metadata_type(program, ty, 0)?;
        }
    }
    for variant in program.variants.values() {
        if variant.payload.len() > 1 {
            return Err(FosterError::runtime(format!(
                "bytecode enum case `{}.{}` has more than one payload value",
                variant.type_name, variant.alternative
            )));
        }
        for ty in &variant.payload {
            verify_metadata_type(program, ty, 0)?;
        }
    }
    for ((nominal, slot), target) in &program.dispatch {
        let Some(target) = program.functions.get(target) else {
            return Err(FosterError::runtime(
                "bytecode dispatch table references a missing function",
            ));
        };
        if target.intrinsic_stub {
            return Err(FosterError::runtime(
                "bytecode dispatch table references a non-executable intrinsic declaration",
            ));
        }
        if *slot == crate::types::CAN_COPY_SLOT {
            return Err(FosterError::runtime(
                "the copy capability query slot cannot be implemented",
            ));
        }
        if *slot == crate::types::COPY_SLOT || *slot == crate::types::DEINIT_SLOT {
            let result_valid = if *slot == crate::types::COPY_SLOT {
                target.parameter_types.first() == Some(&target.result_type)
            } else {
                target.result_type == ExecutableType::Unit
            };
            if target.parameters != 1
                || target.captures != 0
                || target.parameter_modes != [ParameterMode::Borrow]
                || target.mutable_parameters != [false]
                || !result_valid
            {
                return Err(FosterError::runtime(
                    "invalid Copy or Drop capability signature in bytecode dispatch",
                ));
            }
        }
        let nominal_exists = match nominal {
            NominalTypeId::Record(record) => program.records.contains_key(record),
            NominalTypeId::Variant(variant) => program
                .variants
                .values()
                .any(|value| value.parent == *variant),
        };
        if !nominal_exists {
            return Err(FosterError::runtime(
                "bytecode dispatch table references a missing nominal type",
            ));
        }
    }
    Ok(())
}

fn verify_metadata_type(
    program: &Program,
    ty: &ExecutableType,
    depth: usize,
) -> Result<(), FosterError> {
    if depth >= 64 {
        return Err(FosterError::runtime(
            "bytecode aggregate metadata has excessively nested verification types",
        ));
    }
    let nested = |ty| verify_metadata_type(program, ty, depth + 1);
    match ty {
        ExecutableType::List(value)
        | ExecutableType::Reference(value)
        | ExecutableType::Remote(value)
        | ExecutableType::Future(value) => nested(value),
        ExecutableType::Function { parameters, result } => {
            for parameter in parameters {
                nested(&parameter.ty)?;
            }
            nested(result)
        }
        ExecutableType::Intersection(_) | ExecutableType::AliasArguments { .. } => Err(
            FosterError::runtime("native structural metadata is not valid bytecode metadata"),
        ),
        ExecutableType::Alternatives(members) => {
            if !ExecutableType::canonical_alternatives(members) {
                return Err(FosterError::runtime(
                    "bytecode aggregate metadata has a non-canonical union type",
                ));
            }
            for member in members {
                nested(member)?;
            }
            Ok(())
        }
        ExecutableType::Record { record, arguments } => {
            if !program.records.contains_key(record) {
                return Err(FosterError::runtime(
                    "bytecode aggregate metadata references a missing record",
                ));
            }
            for argument in arguments {
                nested(argument)?;
            }
            Ok(())
        }
        ExecutableType::Variant { variant, arguments } => {
            if !program
                .variants
                .values()
                .any(|metadata| metadata.parent == *variant)
            {
                return Err(FosterError::runtime(
                    "bytecode aggregate metadata references a missing enum",
                ));
            }
            for argument in arguments {
                nested(argument)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn verify_specialization(
    program: &Program,
    specialization: &crate::codegen::types::Specialization,
) -> Result<(), FosterError> {
    for (_, ty) in specialization {
        verify_metadata_type(program, ty, 0)?;
    }
    Ok(())
}

fn verify_function_structure(
    program: &Program,
    _id: FunctionId,
    function: &BytecodeFunction,
) -> Result<(), FosterError> {
    let parameter_count = usize::from(function.parameters);
    let capture_count = usize::from(function.captures);
    if function.parameter_types.len() != parameter_count
        || function.parameter_modes.len() != parameter_count
        || function.mutable_parameters.len() != parameter_count
    {
        return Err(FosterError::runtime(format!(
            "bytecode function `{}` has invalid parameter metadata",
            function.name
        )));
    }
    if function.capture_types.len() != capture_count {
        return Err(FosterError::runtime(format!(
            "bytecode function `{}` has invalid capture type metadata",
            function.name
        )));
    }
    if function.captures.saturating_add(function.parameters) > function.registers {
        return Err(FosterError::runtime(format!(
            "bytecode function `{}` has an invalid capture/parameter register prefix",
            function.name
        )));
    }
    if function.instructions.len() != function.instruction_spans.len() {
        return Err(FosterError::runtime(format!(
            "bytecode function `{}` has mismatched instruction and span tables",
            function.name
        )));
    }
    if function.instructions.is_empty() {
        return Err(FosterError::runtime(format!(
            "bytecode function `{}` has no instructions",
            function.name
        )));
    }
    for ty in function
        .capture_types
        .iter()
        .chain(&function.parameter_types)
        .chain(std::iter::once(&function.result_type))
    {
        verify_type(program, function, ty, 0)?;
    }
    if function.returns_reference
        && !matches!(
            function.result_type,
            ExecutableType::Reference(_) | ExecutableType::Unknown
        )
    {
        return Err(FosterError::runtime(format!(
            "bytecode function `{}` returns a reference but declares a non-reference result",
            function.name
        )));
    }

    for (index, instruction) in function.instructions.iter().enumerate() {
        let mut invalid = None;
        instruction.visit_registers(|register| {
            if register.0 >= function.registers {
                invalid = Some(register.0);
            }
        });
        if let Some(register) = invalid {
            return invalid_instruction(
                function,
                index,
                format!(
                    "references r{register} outside its {}-register frame",
                    function.registers
                ),
            );
        }
        match instruction {
            Instruction::LoadConstant { constant, .. }
                if usize::from(*constant) >= program.constants.len() =>
            {
                return invalid_instruction(
                    function,
                    index,
                    format!("references missing constant {constant}"),
                );
            }
            Instruction::Call {
                function: target,
                specialization,
                arguments,
                ..
            } => {
                verify_specialization(program, specialization)?;
                let target = target_function(program, function, index, *target)?;
                if target.intrinsic_stub
                    || target.captures != 0
                    || arguments.len() != usize::from(target.parameters)
                {
                    return invalid_instruction(
                        function,
                        index,
                        "has an invalid direct-call capture or parameter layout",
                    );
                }
            }
            Instruction::CallMethod {
                function: target,
                specialization,
                arguments,
                ..
            } => {
                verify_specialization(program, specialization)?;
                let target = target_function(program, function, index, *target)?;
                if target.intrinsic_stub
                    || target.captures != 0
                    || arguments.len().saturating_add(1) != usize::from(target.parameters)
                {
                    return invalid_instruction(
                        function,
                        index,
                        "has an invalid method-call parameter layout",
                    );
                }
            }
            Instruction::RemoteCall {
                function: target,
                arguments,
                ..
            } => {
                let target = target_function(program, function, index, *target)?;
                if target.intrinsic_stub
                    || target.captures != 0
                    || arguments.len().saturating_add(1) != usize::from(target.parameters)
                {
                    return invalid_instruction(
                        function,
                        index,
                        "has an invalid remote-call parameter layout",
                    );
                }
            }
            Instruction::CallClosure {
                function: target,
                specialization,
                captures,
                arguments,
                ..
            } => {
                verify_specialization(program, specialization)?;
                let target = target_function(program, function, index, *target)?;
                if target.intrinsic_stub
                    || captures.len() != usize::from(target.captures)
                    || arguments.len() != usize::from(target.parameters)
                {
                    return invalid_instruction(
                        function,
                        index,
                        "has an invalid specialized closure call",
                    );
                }
            }
            Instruction::MakeClosure {
                function: target,
                specialization,
                captures,
                ..
            } => {
                verify_specialization(program, specialization)?;
                let target = target_function(program, function, index, *target)?;
                if target.intrinsic_stub || captures.len() != usize::from(target.captures) {
                    return invalid_instruction(
                        function,
                        index,
                        "constructs a closure with the wrong capture layout",
                    );
                }
            }
            Instruction::Jump { target } | Instruction::JumpIfFalse { target, .. }
                if *target >= function.instructions.len() =>
            {
                return invalid_instruction(
                    function,
                    index,
                    format!("has invalid jump target {target}"),
                );
            }
            Instruction::MakeRecord {
                record,
                type_arguments,
                fields,
                ..
            } => {
                let Some(metadata) = program.records.get(record) else {
                    return invalid_instruction(function, index, "references a missing record");
                };
                if type_arguments.len() != metadata.parameters.len() {
                    return invalid_instruction(
                        function,
                        index,
                        "has the wrong number of record type arguments",
                    );
                }
                for ty in type_arguments {
                    verify_metadata_type(program, ty, 0)?;
                }
                let expected = metadata.layout.names();
                if fields.len() != expected.len()
                    || fields.iter().map(|(name, _)| name).ne(expected.iter())
                {
                    return invalid_instruction(
                        function,
                        index,
                        "constructs a record with an invalid field layout",
                    );
                }
            }
            Instruction::MakeVariant {
                variant,
                type_arguments,
                payload,
                ..
            } => match program.variants.get(variant) {
                None => {
                    return invalid_instruction(function, index, "references a missing variant");
                }
                Some(metadata)
                    if metadata.payload.len() != payload.len()
                        || metadata.parameters.len() != type_arguments.len() =>
                {
                    return invalid_instruction(
                        function,
                        index,
                        "constructs an enum case with an invalid type or payload layout",
                    );
                }
                Some(_) => {
                    for ty in type_arguments {
                        verify_metadata_type(program, ty, 0)?;
                    }
                }
            },
            Instruction::MatchPattern {
                pattern, bindings, ..
            } if pattern_binding_count(pattern) != bindings.len() => {
                return invalid_instruction(
                    function,
                    index,
                    "pattern binding register count does not match the pattern",
                );
            }
            _ => {}
        }
    }
    Ok(())
}

fn verify_type(
    program: &Program,
    function: &BytecodeFunction,
    ty: &ExecutableType,
    depth: usize,
) -> Result<(), FosterError> {
    if depth >= 64 {
        return Err(FosterError::runtime(format!(
            "bytecode function `{}` has excessively nested verification types",
            function.name
        )));
    }
    match ty {
        ExecutableType::List(value)
        | ExecutableType::Reference(value)
        | ExecutableType::Remote(value)
        | ExecutableType::Future(value) => verify_type(program, function, value, depth + 1),
        ExecutableType::Function { parameters, result } => {
            for parameter in parameters {
                verify_type(program, function, &parameter.ty, depth + 1)?;
            }
            verify_type(program, function, result, depth + 1)
        }
        ExecutableType::Intersection(_) | ExecutableType::AliasArguments { .. } => Err(
            FosterError::runtime("native structural metadata is not valid bytecode metadata"),
        ),
        ExecutableType::Alternatives(members) => {
            if !ExecutableType::canonical_alternatives(members) {
                return Err(FosterError::runtime(format!(
                    "bytecode function `{}` has a non-canonical union verification type",
                    function.name
                )));
            }
            for member in members {
                verify_type(program, function, member, depth + 1)?;
            }
            Ok(())
        }
        ExecutableType::Record { record, arguments } => {
            if !program.records.contains_key(record) {
                return Err(FosterError::runtime(format!(
                    "bytecode function `{}` has a verification type for a missing record",
                    function.name
                )));
            }
            for argument in arguments {
                verify_type(program, function, argument, depth + 1)?;
            }
            Ok(())
        }
        ExecutableType::Variant { variant, arguments } => {
            if !program
                .variants
                .values()
                .any(|value| value.parent == *variant)
            {
                return Err(FosterError::runtime(format!(
                    "bytecode function `{}` has a verification type for a missing variant",
                    function.name
                )));
            }
            for argument in arguments {
                verify_type(program, function, argument, depth + 1)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FlowState {
    registers: Vec<Option<ExecutableType>>,
    pending_pattern: Option<PendingPattern>,
    excluded_variants: HashMap<Register, HashSet<VariantId>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingPattern {
    condition: Register,
    bindings: Vec<Register>,
    irrefutable: bool,
    covered_variant: Option<(Register, VariantId)>,
}

fn verify_function_flow(
    program: &Program,
    _id: FunctionId,
    function: &BytecodeFunction,
) -> Result<(), FosterError> {
    analyze_function_flow(program, function).map(|_| ())
}

pub(crate) fn type_states(
    program: &Program,
    function: &BytecodeFunction,
) -> Result<Vec<Option<Vec<Option<ExecutableType>>>>, FosterError> {
    analyze_function_flow(program, function).map(|states| {
        states
            .into_iter()
            .map(|state| state.map(|state| state.registers))
            .collect()
    })
}

fn analyze_function_flow(
    program: &Program,
    function: &BytecodeFunction,
) -> Result<Vec<Option<FlowState>>, FosterError> {
    if function.intrinsic_stub {
        return Ok(vec![None; function.instructions.len()]);
    }
    let mut entry = FlowState {
        registers: vec![None; usize::from(function.registers)],
        pending_pattern: None,
        excluded_variants: HashMap::new(),
    };
    for (index, ty) in function
        .capture_types
        .iter()
        .chain(&function.parameter_types)
        .enumerate()
    {
        entry.registers[index] = Some(ty.clone());
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
    function: &BytecodeFunction,
    index: usize,
    mut state: FlowState,
) -> Result<Vec<(usize, FlowState)>, FosterError> {
    let instruction = &function.instructions[index];
    let next = index + 1;
    // Drop insertion can place cleanup between a pattern test and its conditional branch.
    // Preserve the edge fact until the corresponding condition is consumed.
    let pattern = state.pending_pattern.clone();

    match instruction {
        Instruction::Drop { register } => {
            // Liveness drops are deliberately idempotent. A consuming call can empty a
            // register before the cleanup instruction on that edge executes.
            state.registers[usize::from(register.0)] = None;
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
            constant_type(program, &program.constants[usize::from(*constant)]),
        )?,
        Instruction::Move {
            destination,
            source,
        } => {
            let ty = if matches!(
                state.registers[usize::from(destination.0)],
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
                    variant: program.variants[variant].parent,
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
            target.parameter_types[0].infer_specialization(&receiver, &mut substitutions);
            for (schema, (_, argument)) in target.parameter_types.iter().skip(1).zip(arguments) {
                schema.infer_specialization(
                    &read_type(function, index, &state, *argument)?,
                    &mut substitutions,
                );
            }
            let specialization = substitutions.into();
            let parameter_types = target
                .parameter_types
                .iter()
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
                target.parameter_modes.iter().copied().skip(1),
                parameter_types.iter().skip(1),
            )?;
            write_type(
                function,
                index,
                &mut state,
                *destination,
                ExecutableType::Future(Box::new(
                    program.remote_outcome_type(target.result_type.specialize(&specialization)),
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
                let parent = program.variants[&variant].parent;
                program
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
                && program.variants[variant].parent != parent
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
                condition: *destination,
                bindings: bindings.clone(),
                irrefutable: pattern_irrefutable(pattern) || exhaustive,
                covered_variant,
            });
        }
        Instruction::Jump { target } => return Ok(vec![(*target, state)]),
        Instruction::JumpIfFalse { condition, target } => {
            let found = read_type(function, index, &state, *condition)?;
            require_type(function, index, &found, &ExecutableType::Bool, "condition")?;
            let mut truthy = state.clone();
            state.pending_pattern = None;
            truthy.pending_pattern = None;
            let pattern = pattern.filter(|pattern| pattern.condition == *condition);
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
                if let Some((subject, variant)) = pattern.covered_variant {
                    state
                        .excluded_variants
                        .entry(subject)
                        .or_default()
                        .insert(variant);
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
                .parameter_types
                .iter()
                .map(|ty| ty.specialize(specialization))
                .collect::<Vec<_>>();
            verify_arguments(
                function,
                index,
                &mut state,
                target
                    .parameter_modes
                    .iter()
                    .copied()
                    .zip(arguments.iter().copied()),
                target.parameter_modes.iter().copied(),
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
                .parameter_types
                .iter()
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
                    .parameter_modes
                    .iter()
                    .copied()
                    .skip(1)
                    .zip(arguments.iter().copied()),
                target.parameter_modes.iter().copied().skip(1),
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
            verify_metadata_type(program, result_type, 0)?;
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
                .and_then(|nominal| program.dispatch.get(&(nominal, *slot)))
                .and_then(|target| program.functions.get(target))
            {
                let mut substitutions = std::collections::BTreeMap::new();
                if let Some(parameter) = target.parameter_types.first() {
                    parameter.infer_specialization(&receiver_type, &mut substitutions);
                }
                for (parameter, argument) in target.parameter_types.iter().skip(1).zip(arguments) {
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
                        .parameter_modes
                        .iter()
                        .copied()
                        .skip(1)
                        .zip(arguments.iter().copied()),
                    target.parameter_modes.iter().copied().skip(1),
                    target.parameter_types.iter().skip(1),
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
                .capture_types
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
                .capture_types
                .iter()
                .map(|ty| ty.specialize(specialization))
                .collect::<Vec<_>>();
            let parameter_types = target
                .parameter_types
                .iter()
                .map(|ty| ty.specialize(specialization))
                .collect::<Vec<_>>();
            verify_captures(function, index, &mut state, captures, &capture_types)?;
            verify_arguments(
                function,
                index,
                &mut state,
                target
                    .parameter_modes
                    .iter()
                    .copied()
                    .zip(arguments.iter().copied()),
                target.parameter_modes.iter().copied(),
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
            let metadata = program.records.get(record)?;
            let index = metadata
                .layout
                .names()
                .iter()
                .position(|name| name == field)?;
            let substitutions = metadata
                .parameters
                .iter()
                .cloned()
                .zip(arguments.iter().cloned())
                .collect::<HashMap<_, _>>();
            Some(metadata.field_types.get(index)?.substitute(&substitutions))
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
    function: &BytecodeFunction,
    index: usize,
    state: &mut FlowState,
    actual: impl Iterator<Item = (ParameterMode, Register)>,
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
    function: &BytecodeFunction,
    index: usize,
    state: &mut FlowState,
    captures: &[(CaptureMode, Register)],
    expected: &[ExecutableType],
) -> Result<(), FosterError> {
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
    function: &BytecodeFunction,
    index: usize,
    state: &FlowState,
    register: Register,
) -> Result<ExecutableType, FosterError> {
    state.registers[usize::from(register.0)]
        .clone()
        .ok_or_else(|| {
            FosterError::runtime(format!(
                "bytecode function `{}` instruction {index} reads unavailable r{} in {:?}",
                function.name, register.0, function.instructions[index]
            ))
        })
}

fn read_type(
    function: &BytecodeFunction,
    index: usize,
    state: &FlowState,
    register: Register,
) -> Result<ExecutableType, FosterError> {
    readable_type(
        function,
        index,
        bound_type(function, index, state, register)?,
    )
}

fn readable_type(
    function: &BytecodeFunction,
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
    function: &BytecodeFunction,
    index: usize,
    state: &mut FlowState,
    register: Register,
) -> Result<ExecutableType, FosterError> {
    state.excluded_variants.remove(&register);
    state.registers[usize::from(register.0)]
        .take()
        .ok_or_else(|| {
            FosterError::runtime(format!(
                "bytecode function `{}` instruction {index} consumes unavailable r{} in {:?}",
                function.name, register.0, function.instructions[index]
            ))
        })
}

fn write_type(
    function: &BytecodeFunction,
    index: usize,
    state: &mut FlowState,
    register: Register,
    value: ExecutableType,
) -> Result<(), FosterError> {
    state.excluded_variants.remove(&register);
    let slot = &mut state.registers[usize::from(register.0)];
    if let Some(ExecutableType::Reference(target)) = slot {
        require_type(function, index, &value, target, "reference assignment")?;
    } else {
        *slot = Some(value);
    }
    Ok(())
}

fn merge_state(
    function: &BytecodeFunction,
    index: usize,
    current: &mut FlowState,
    incoming: &FlowState,
) -> Result<bool, FosterError> {
    let mut changed = false;
    for (left, right) in current.registers.iter_mut().zip(&incoming.registers) {
        let merged = match (&*left, right) {
            (Some(left_type), Some(right_type)) => {
                // Register coloring can reuse one physical register for unrelated values on
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
    _function: &BytecodeFunction,
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
    function: &BytecodeFunction,
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

fn compatible(found: &ExecutableType, expected: &ExecutableType) -> bool {
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
    function: &BytecodeFunction,
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
    function: &BytecodeFunction,
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
            .string_record
            .map(nominal_record)
            .unwrap_or(ExecutableType::Unknown),
        Constant::CodePoint(_) => ExecutableType::CodePoint,
        Constant::Symbol(_) => program
            .symbol_record
            .map(nominal_record)
            .unwrap_or(ExecutableType::Unknown),
    }
}

fn record_type(program: &Program, record: crate::hir::RecordId) -> ExecutableType {
    match Some(record) {
        id if id == program.list_record => ExecutableType::List(Box::new(ExecutableType::Unknown)),
        id if id == program.bytes_record => ExecutableType::Bytes,
        _ => nominal_record(record),
    }
}

fn is_foster_byte_buffer(program: &Program, ty: &ExecutableType) -> bool {
    let ExecutableType::Record { record, .. } = ty else {
        return false;
    };
    Some(*record) == program.byte_buffer_record
}

fn indexed_element_type(program: &Program, ty: &ExecutableType) -> Option<ExecutableType> {
    match ty {
        ExecutableType::Reference(pointee) => indexed_element_type(program, pointee),
        _ if is_foster_byte_buffer(program, ty) => Some(ExecutableType::Byte),
        _ => ty.indexed_element(),
    }
}

fn callable_type(function: &BytecodeFunction) -> ExecutableType {
    ExecutableType::Function {
        parameters: crate::types::Parameter::from_parts(
            function.parameter_types.clone(),
            function.parameter_modes.clone(),
        ),
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
            .string_record
            .map(nominal_record)
            .unwrap_or(ExecutableType::Unknown),
        IntrinsicType::ListByte => ExecutableType::List(Box::new(ExecutableType::Byte)),
    }
}

fn target_function<'a>(
    program: &'a Program,
    function: &BytecodeFunction,
    index: usize,
    target: FunctionId,
) -> Result<&'a BytecodeFunction, FosterError> {
    program.functions.get(&target).ok_or_else(|| {
        FosterError::runtime(format!(
            "bytecode function `{}` instruction {index} references a missing function",
            function.name
        ))
    })
}

fn pattern_binding_count(pattern: &crate::hir::Pattern) -> usize {
    match pattern.unspanned() {
        crate::hir::Pattern::Binding(_) => 1,
        crate::hir::Pattern::Variant { fields, .. } => {
            fields.iter().map(pattern_binding_count).sum()
        }
        _ => 0,
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
    function: &BytecodeFunction,
    index: usize,
    message: impl std::fmt::Display,
) -> Result<T, FosterError> {
    Err(FosterError::runtime(format!(
        "bytecode function `{}` instruction {index} {message}",
        function.name
    )))
}

fn type_error<T>(
    function: &BytecodeFunction,
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

#[cfg(test)]
mod type_semantics_tests {
    use super::*;

    #[test]
    fn only_control_flow_alternatives_use_any_member_matching() {
        use ExecutableType as T;
        let alternatives = T::alternatives(vec![T::Bool, T::Integer]);
        let intersection = T::intersection(vec![T::Bool, T::Integer]);
        assert!(compatible(&T::Integer, &alternatives));
        assert!(!compatible(&alternatives, &T::Integer));
        assert!(!compatible(&T::Integer, &intersection));
        assert!(!compatible(&intersection, &T::Integer));
        let alias = T::AliasArguments {
            alias: crate::hir::VariantTypeId::from_raw(la_arena::RawIdx::from_u32(0)),
            arguments: vec![T::Integer],
        };
        assert!(!compatible(&T::Integer, &alias));
        assert!(!compatible(&alias, &T::Integer));
    }

    #[test]
    fn bytecode_rejects_native_metadata_in_signatures_and_aggregate_metadata() {
        use ExecutableType as T;
        let compilation = crate::compile("func main() -> Int { 42 }").unwrap();
        let program = crate::vm::compile(&compilation).unwrap();
        let function = &program.functions[&program.main.unwrap()];
        for view in [
            T::intersection(vec![T::Integer]),
            T::AliasArguments {
                alias: crate::hir::VariantTypeId::from_raw(la_arena::RawIdx::from_u32(0)),
                arguments: vec![T::Integer],
            },
        ] {
            let nested = T::List(Box::new(view));
            assert!(
                verify_type(&program, function, &nested, 0)
                    .unwrap_err()
                    .message
                    .contains("native structural metadata")
            );
            assert!(
                verify_metadata_type(&program, &nested, 0)
                    .unwrap_err()
                    .message
                    .contains("native structural metadata")
            );
        }
    }
}
