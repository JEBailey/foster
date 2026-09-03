use std::collections::{HashMap, HashSet, VecDeque};

use crate::ast::{BinaryOp, ParameterMode, UnaryOp};
use crate::error::FosterError;
use crate::hir::{CaptureMode, FunctionId, VariantId};
use crate::intrinsics::{IntrinsicArgumentMode, IntrinsicType};
use crate::types::NominalTypeId;

use super::{BytecodeFunction, Constant, Instruction, Program, Register, VerificationType};

pub fn verify(program: &Program) -> Result<(), FosterError> {
    verify_program_metadata(program)?;
    for (id, function) in &program.functions {
        verify_function_structure(program, *id, function)?;
    }
    for (id, function) in &program.functions {
        verify_function_flow(program, *id, function)?;
    }
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
    for record in [program.string_record, program.symbol_record]
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
    for ((nominal, _), target) in &program.dispatch {
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
    ty: &VerificationType,
    depth: usize,
) -> Result<(), FosterError> {
    if depth >= 64 {
        return Err(FosterError::runtime(
            "bytecode aggregate metadata has excessively nested verification types",
        ));
    }
    let nested = |ty| verify_metadata_type(program, ty, depth + 1);
    match ty {
        VerificationType::List(value)
        | VerificationType::Reference(value)
        | VerificationType::Remote(value)
        | VerificationType::Future(value) => nested(value),
        VerificationType::Function {
            parameters,
            parameter_modes,
            result,
        } => {
            if parameters.len() != parameter_modes.len() {
                return Err(FosterError::runtime(
                    "bytecode aggregate metadata has an invalid callable type",
                ));
            }
            for parameter in parameters {
                nested(parameter)?;
            }
            nested(result)
        }
        VerificationType::Union(members) => {
            if members.len() < 2 || members.windows(2).any(|pair| pair[0] >= pair[1]) {
                return Err(FosterError::runtime(
                    "bytecode aggregate metadata has a non-canonical union type",
                ));
            }
            for member in members {
                nested(member)?;
            }
            Ok(())
        }
        VerificationType::Record { record, arguments } => {
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
        VerificationType::Variant { variant, arguments } => {
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
    function: &BytecodeFunction,
    instruction: usize,
    specialization: &crate::vm::Specialization,
) -> Result<(), FosterError> {
    if specialization.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
        return invalid_instruction(
            function,
            instruction,
            "has unsorted or duplicate generic substitutions",
        );
    }
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
            VerificationType::Reference(_) | VerificationType::Unknown
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
                verify_specialization(program, function, index, specialization)?;
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
                verify_specialization(program, function, index, specialization)?;
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
                verify_specialization(program, function, index, specialization)?;
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
                verify_specialization(program, function, index, specialization)?;
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
    ty: &VerificationType,
    depth: usize,
) -> Result<(), FosterError> {
    if depth >= 64 {
        return Err(FosterError::runtime(format!(
            "bytecode function `{}` has excessively nested verification types",
            function.name
        )));
    }
    match ty {
        VerificationType::List(value)
        | VerificationType::Reference(value)
        | VerificationType::Remote(value)
        | VerificationType::Future(value) => verify_type(program, function, value, depth + 1),
        VerificationType::Function {
            parameters,
            parameter_modes,
            result,
        } => {
            if parameters.len() != parameter_modes.len() {
                return Err(FosterError::runtime(format!(
                    "bytecode function `{}` has an invalid callable verification type",
                    function.name
                )));
            }
            for parameter in parameters {
                verify_type(program, function, parameter, depth + 1)?;
            }
            verify_type(program, function, result, depth + 1)
        }
        VerificationType::Union(members) => {
            if members.len() < 2 {
                return Err(FosterError::runtime(format!(
                    "bytecode function `{}` has a non-canonical union verification type",
                    function.name
                )));
            }
            for member in members {
                verify_type(program, function, member, depth + 1)?;
            }
            if members.windows(2).any(|pair| pair[0] >= pair[1]) {
                return Err(FosterError::runtime(format!(
                    "bytecode function `{}` has an unsorted or duplicate union verification type",
                    function.name
                )));
            }
            Ok(())
        }
        VerificationType::Record { record, arguments } => {
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
        VerificationType::Variant { variant, arguments } => {
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
    registers: Vec<Option<VerificationType>>,
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
    if function.intrinsic_stub {
        return Ok(());
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
    Ok(())
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
                Some(VerificationType::Reference(_))
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
                VerificationType::List(Box::new(element)),
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
                &VerificationType::Integer,
                "index",
            )?;
            let result = match read_type(function, index, &state, *object)? {
                VerificationType::List(element) => *element,
                VerificationType::Bytes | VerificationType::ByteBuffer => VerificationType::Byte,
                VerificationType::Unknown => VerificationType::Unknown,
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
                VerificationType::Variant {
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
                VerificationType::Reference(Box::new(VerificationType::Unknown))
            } else {
                VerificationType::Unknown
            };
            write_type(function, index, &mut state, *destination, ty)?;
        }
        Instruction::StoreField { object, source, .. } => {
            let object_type = read_type(function, index, &state, *object)?;
            if !matches!(
                object_type,
                VerificationType::Record { .. }
                    | VerificationType::List(_)
                    | VerificationType::Bytes
                    | VerificationType::ByteBuffer
                    | VerificationType::Unknown
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
                &VerificationType::Integer,
                "index",
            )?;
            let source_type = read_type(function, index, &state, *source)?;
            match read_type(function, index, &state, *object)? {
                VerificationType::List(element) => {
                    require_type(function, index, &source_type, &element, "list element")?
                }
                VerificationType::ByteBuffer => require_type(
                    function,
                    index,
                    &source_type,
                    &VerificationType::Byte,
                    "byte-buffer element",
                )?,
                VerificationType::Unknown => {}
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
                &VerificationType::Integer,
                "index",
            )?;
            let object_type = read_type(function, index, &state, *object)?;
            let inferred = object_type.indexed_element().ok_or_else(|| {
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
                VerificationType::Reference(Box::new(pointee_type.clone())),
            )?;
        }
        Instruction::MakeWholeReference {
            destination,
            pointee_type,
            object,
        } => {
            let value = read_type(function, index, &state, *object)?;
            let inferred = match value {
                VerificationType::Reference(pointee) => *pointee,
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
                VerificationType::Reference(Box::new(pointee_type.clone())),
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
                VerificationType::Reference(Box::new(pointee_type.clone())),
            )?;
        }
        Instruction::MoveOut {
            destination,
            source,
        } => {
            let ty = take_type(function, index, &mut state, *source)?;
            write_type(function, index, &mut state, *destination, ty)?;
        }
        Instruction::Push {
            destination,
            object,
            value,
        } => {
            let value = read_type(function, index, &state, *value)?;
            match read_type(function, index, &state, *object)? {
                VerificationType::List(element) => {
                    require_type(function, index, &value, &element, "list element")?
                }
                VerificationType::ByteBuffer => require_type(
                    function,
                    index,
                    &value,
                    &VerificationType::Byte,
                    "byte-buffer element",
                )?,
                VerificationType::Unknown => {}
                found => return type_error(function, index, "List or ByteBuffer", &found),
            }
            write_type(
                function,
                index,
                &mut state,
                *destination,
                VerificationType::Unit,
            )?;
        }
        Instruction::Append {
            destination,
            object,
            value,
        } => {
            let value = read_type(function, index, &state, *value)?;
            let result = match read_type(function, index, &state, *object)? {
                VerificationType::List(element) => {
                    require_type(function, index, &value, &element, "list element")?;
                    VerificationType::List(element)
                }
                VerificationType::Unknown => VerificationType::Unknown,
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
                VerificationType::Bool,
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
                VerificationType::Remote(Box::new(value)),
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
                VerificationType::Remote(Box::new(value)),
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
                VerificationType::Remote(value) => *value,
                VerificationType::Unknown => VerificationType::Unknown,
                found => return type_error(function, index, "Remote", &found),
            };
            require_type(
                function,
                index,
                &receiver,
                &target.parameter_types[0],
                "remote receiver",
            )?;
            verify_arguments(
                function,
                index,
                &mut state,
                arguments.iter().map(|(mode, register)| (*mode, *register)),
                target.parameter_modes.iter().copied().skip(1),
                target.parameter_types.iter().skip(1),
            )?;
            write_type(
                function,
                index,
                &mut state,
                *destination,
                VerificationType::Future(Box::new(target.result_type.clone())),
            )?;
        }
        Instruction::Await {
            destination,
            future,
        } => {
            let result = match read_type(function, index, &state, *future)? {
                VerificationType::Future(value) => *value,
                VerificationType::Unknown => VerificationType::Unknown,
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
                && let VerificationType::Variant {
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
                VerificationType::Bool,
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
            require_type(
                function,
                index,
                &found,
                &VerificationType::Bool,
                "condition",
            )?;
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
                        VerificationType::Unknown,
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
            require_type(
                function,
                index,
                &found,
                &VerificationType::Bool,
                "condition",
            )?;
            if let Some(message) = message {
                let expected = program
                    .string_record
                    .map(nominal_record)
                    .unwrap_or(VerificationType::Unknown);
                let found = read_type(function, index, &state, *message)?;
                require_type(function, index, &found, &expected, "assertion message")?;
            }
        }
        Instruction::Call {
            destination,
            function: target,
            arguments,
            ..
        } => {
            let target = &program.functions[target];
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
                target.parameter_types.iter(),
            )?;
            write_type(
                function,
                index,
                &mut state,
                *destination,
                target.result_type.clone(),
            )?;
        }
        Instruction::CallMethod {
            destination,
            receiver,
            function: target,
            arguments,
            ..
        } => {
            let target = &program.functions[target];
            let receiver = read_type(function, index, &state, *receiver)?;
            require_type(
                function,
                index,
                &receiver,
                &target.parameter_types[0],
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
                target.parameter_types.iter().skip(1),
            )?;
            write_type(
                function,
                index,
                &mut state,
                *destination,
                target.result_type.clone(),
            )?;
        }
        Instruction::CallContractMethod {
            destination,
            receiver,
            slot,
            arguments,
            ..
        } => {
            let receiver_type = read_type(function, index, &state, *receiver)?;
            let nominal = match receiver_type {
                VerificationType::Record { record, .. } => Some(NominalTypeId::Record(record)),
                VerificationType::Variant { variant, .. } => Some(NominalTypeId::Variant(variant)),
                _ => None,
            };
            if let Some(target) = nominal
                .and_then(|nominal| program.dispatch.get(&(nominal, *slot)))
                .and_then(|target| program.functions.get(target))
            {
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
                    target.result_type.clone(),
                )?;
            } else {
                for argument in arguments {
                    read_type(function, index, &state, *argument)?;
                }
                write_type(
                    function,
                    index,
                    &mut state,
                    *destination,
                    VerificationType::Unknown,
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
            let VerificationType::Function {
                parameters,
                parameter_modes,
                result,
            } = callee
            else {
                if callee == VerificationType::Unknown {
                    for argument in arguments {
                        read_type(function, index, &state, *argument)?;
                    }
                    write_type(
                        function,
                        index,
                        &mut state,
                        *destination,
                        VerificationType::Unknown,
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
                parameter_modes
                    .iter()
                    .copied()
                    .zip(arguments.iter().copied()),
                parameter_modes.iter().copied(),
                parameters.iter(),
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
    receiver: &VerificationType,
    field: &str,
) -> Option<VerificationType> {
    match receiver {
        VerificationType::Reference(pointee) => verification_field_type(program, pointee, field),
        VerificationType::Record { record, arguments } => {
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
        VerificationType::List(element) => match field {
            "empty?" => Some(VerificationType::Bool),
            "length" => Some(VerificationType::Integer),
            "head" => Some((**element).clone()),
            "rest" => Some(receiver.clone()),
            _ => None,
        },
        VerificationType::Unknown => Some(VerificationType::Unknown),
        _ => None,
    }
}

fn verify_arguments<'a>(
    function: &BytecodeFunction,
    index: usize,
    state: &mut FlowState,
    actual: impl Iterator<Item = (ParameterMode, Register)>,
    expected_modes: impl Iterator<Item = ParameterMode>,
    expected_types: impl Iterator<Item = &'a VerificationType>,
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
            ParameterMode::Borrow if matches!(expected_type, VerificationType::Reference(value) if compatible(&bound, value)) =>
            {
                // Whole-place reference construction may be optimized into passing the
                // underlying register; call-frame promotion recreates the same place binding.
                VerificationType::Reference(Box::new(bound))
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
    expected: &[VerificationType],
) -> Result<(), FosterError> {
    for ((mode, register), expected) in captures.iter().zip(expected) {
        let found = match mode {
            CaptureMode::Move => bound_type(function, index, state, *register)?,
            CaptureMode::Copy | CaptureMode::Pending => {
                read_type(function, index, state, *register)?
            }
            CaptureMode::Ref => match bound_type(function, index, state, *register)? {
                reference @ VerificationType::Reference(_) => reference,
                value => VerificationType::Reference(Box::new(value)),
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
) -> Result<VerificationType, FosterError> {
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
) -> Result<VerificationType, FosterError> {
    readable_type(
        function,
        index,
        bound_type(function, index, state, register)?,
    )
}

fn readable_type(
    function: &BytecodeFunction,
    index: usize,
    ty: VerificationType,
) -> Result<VerificationType, FosterError> {
    match ty {
        VerificationType::Reference(value) => Ok(*value),
        VerificationType::Union(members) => {
            let mut members = members.into_iter();
            let Some(first) = members.next() else {
                return Ok(VerificationType::Unknown);
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
) -> Result<VerificationType, FosterError> {
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
    value: VerificationType,
) -> Result<(), FosterError> {
    state.excluded_variants.remove(&register);
    let slot = &mut state.registers[usize::from(register.0)];
    if let Some(VerificationType::Reference(target)) = slot {
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
    left: &VerificationType,
    right: &VerificationType,
) -> Result<VerificationType, FosterError> {
    if left == right {
        return Ok(left.clone());
    }
    if matches!(left, VerificationType::Unknown) || matches!(right, VerificationType::Unknown) {
        return Ok(VerificationType::Unknown);
    }
    match (left, right) {
        (VerificationType::List(left), VerificationType::List(right)) => Ok(
            VerificationType::List(Box::new(merge_types(_function, _index, left, right)?)),
        ),
        (VerificationType::Reference(left), VerificationType::Reference(right)) => Ok(
            VerificationType::Reference(Box::new(merge_types(_function, _index, left, right)?)),
        ),
        (VerificationType::Remote(left), VerificationType::Remote(right)) => Ok(
            VerificationType::Remote(Box::new(merge_types(_function, _index, left, right)?)),
        ),
        (VerificationType::Future(left), VerificationType::Future(right)) => Ok(
            VerificationType::Future(Box::new(merge_types(_function, _index, left, right)?)),
        ),
        _ => {
            let mut members = Vec::new();
            for ty in [left, right] {
                if let VerificationType::Union(nested) = ty {
                    members.extend(nested.iter().cloned());
                } else {
                    members.push(ty.clone());
                }
            }
            members.sort();
            members.dedup();
            Ok(VerificationType::Union(members))
        }
    }
}

fn require_type(
    function: &BytecodeFunction,
    index: usize,
    found: &VerificationType,
    expected: &VerificationType,
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

fn compatible(found: &VerificationType, expected: &VerificationType) -> bool {
    if found == expected
        || matches!(
            found,
            VerificationType::Unknown | VerificationType::Generic(_)
        )
        || matches!(
            expected,
            VerificationType::Unknown | VerificationType::Generic(_)
        )
    {
        return true;
    }
    match (found, expected) {
        (VerificationType::Union(found), expected) => {
            found.iter().all(|found| compatible(found, expected))
        }
        (found, VerificationType::Union(expected)) => {
            expected.iter().any(|expected| compatible(found, expected))
        }
        (VerificationType::CodePoint | VerificationType::Byte, VerificationType::Integer) => true,
        // Structural record and variant conformance is resolved before bytecode lowering. The
        // verification vocabulary retains runtime representation, not the source contract proof.
        (VerificationType::Record { .. }, VerificationType::Record { .. })
        | (VerificationType::Variant { .. }, VerificationType::Variant { .. }) => true,
        (VerificationType::List(found), VerificationType::List(expected))
        | (VerificationType::Reference(found), VerificationType::Reference(expected))
        | (VerificationType::Remote(found), VerificationType::Remote(expected))
        | (VerificationType::Future(found), VerificationType::Future(expected)) => {
            compatible(found, expected)
        }
        (
            VerificationType::Function {
                parameters: found_parameters,
                parameter_modes: found_modes,
                result: found_result,
            },
            VerificationType::Function {
                parameters: expected_parameters,
                parameter_modes: expected_modes,
                result: expected_result,
            },
        ) => {
            // Callable representation erasure can adapt source ownership modes while the
            // closure still carries its concrete callee modes for execution.
            found_modes.len() == expected_modes.len()
                && found_parameters.len() == expected_parameters.len()
                && found_parameters
                    .iter()
                    .zip(expected_parameters)
                    .all(|(found, expected)| compatible(found, expected))
                && compatible(found_result, expected_result)
        }
        _ => false,
    }
}

fn unary_type(
    function: &BytecodeFunction,
    index: usize,
    operator: UnaryOp,
    operand: VerificationType,
) -> Result<VerificationType, FosterError> {
    Ok(match (operator, operand) {
        (UnaryOp::Negate, VerificationType::Float) => VerificationType::Float,
        (
            UnaryOp::Negate,
            VerificationType::Integer | VerificationType::CodePoint | VerificationType::Byte,
        ) => VerificationType::Integer,
        (UnaryOp::Not, VerificationType::Bool) => VerificationType::Bool,
        (UnaryOp::BitNot, VerificationType::Byte) => VerificationType::Byte,
        (_, VerificationType::Unknown) => VerificationType::Unknown,
        (_, found) => return type_error(function, index, "valid unary operand", &found),
    })
}

fn binary_type(
    function: &BytecodeFunction,
    index: usize,
    operator: BinaryOp,
    left: VerificationType,
    right: VerificationType,
) -> Result<VerificationType, FosterError> {
    use BinaryOp::*;
    if matches!(operator, Equal | NotEqual) {
        if !(is_integer_like(&left) && is_integer_like(&right)) {
            require_type(function, index, &left, &right, "binary operand")?;
        }
        return Ok(VerificationType::Bool);
    }
    if left == VerificationType::Unknown || right == VerificationType::Unknown {
        return Ok(VerificationType::Unknown);
    }
    if matches!(operator, BitAnd | BitOr | BitXor)
        && left == VerificationType::Byte
        && right == VerificationType::Byte
    {
        return Ok(VerificationType::Byte);
    }
    if matches!(operator, ShiftLeft | ShiftRight)
        && left == VerificationType::Byte
        && right == VerificationType::Integer
    {
        return Ok(VerificationType::Byte);
    }
    if is_integer_like(&left) && is_integer_like(&right) {
        return Ok(
            if matches!(operator, Less | LessEqual | Greater | GreaterEqual) {
                VerificationType::Bool
            } else if matches!(operator, Add | Subtract | Multiply | Divide) {
                VerificationType::Integer
            } else {
                return type_error(function, index, "valid integer operation", &left);
            },
        );
    }
    if left == VerificationType::Float && right == VerificationType::Float {
        return Ok(
            if matches!(operator, Less | LessEqual | Greater | GreaterEqual) {
                VerificationType::Bool
            } else if matches!(operator, Add | Subtract | Multiply | Divide) {
                VerificationType::Float
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

fn is_integer_like(ty: &VerificationType) -> bool {
    matches!(
        ty,
        VerificationType::Integer | VerificationType::CodePoint | VerificationType::Byte
    )
}

fn nominal_record(record: crate::hir::RecordId) -> VerificationType {
    VerificationType::Record {
        record,
        arguments: Vec::new(),
    }
}

fn constant_type(program: &Program, constant: &Constant) -> VerificationType {
    match constant {
        Constant::Unit => VerificationType::Unit,
        Constant::Bool(_) => VerificationType::Bool,
        Constant::Integer(_) => VerificationType::Integer,
        Constant::Float(_) => VerificationType::Float,
        Constant::String(_) => program
            .string_record
            .map(nominal_record)
            .unwrap_or(VerificationType::Unknown),
        Constant::CodePoint(_) => VerificationType::CodePoint,
        Constant::Symbol(_) => program
            .symbol_record
            .map(nominal_record)
            .unwrap_or(VerificationType::Unknown),
    }
}

fn record_type(program: &Program, record: crate::hir::RecordId) -> VerificationType {
    match program
        .records
        .get(&record)
        .map(|metadata| metadata.name.as_str())
    {
        Some("List") => VerificationType::List(Box::new(VerificationType::Unknown)),
        Some("Bytes") => VerificationType::Bytes,
        Some("ByteBuffer") => VerificationType::ByteBuffer,
        _ => nominal_record(record),
    }
}

fn callable_type(function: &BytecodeFunction) -> VerificationType {
    VerificationType::Function {
        parameters: function.parameter_types.clone(),
        parameter_modes: function.parameter_modes.clone(),
        result: Box::new(function.result_type.clone()),
    }
}

fn intrinsic_verification_type(program: &Program, ty: IntrinsicType) -> VerificationType {
    match ty {
        IntrinsicType::Any => VerificationType::Unknown,
        IntrinsicType::Unit => VerificationType::Unit,
        IntrinsicType::Bool => VerificationType::Bool,
        IntrinsicType::Integer => VerificationType::Integer,
        IntrinsicType::Float => VerificationType::Float,
        IntrinsicType::CodePoint => VerificationType::CodePoint,
        IntrinsicType::Byte => VerificationType::Byte,
        IntrinsicType::Bytes => VerificationType::Bytes,
        IntrinsicType::ByteBuffer => VerificationType::ByteBuffer,
        IntrinsicType::String => program
            .string_record
            .map(nominal_record)
            .unwrap_or(VerificationType::Unknown),
        IntrinsicType::ListByte => VerificationType::List(Box::new(VerificationType::Byte)),
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
    found: &VerificationType,
) -> Result<T, FosterError> {
    invalid_instruction(
        function,
        index,
        format!("requires {expected}, found {found:?}"),
    )
}
