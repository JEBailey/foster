use super::schema::logical_schema;
use crate::ast::ParameterMode;
use crate::error::FosterError;
use crate::hir::FunctionId;
use crate::types::NominalTypeId;

use super::{Function, Instruction, Program};
use crate::codegen::types::ExecutableType;

pub fn verify(program: &Program) -> Result<(), FosterError> {
    verify_program_metadata(program)?;
    for (id, function) in &program.functions {
        verify_function_structure(program, *id, function)?;
    }
    let schemas = program
        .functions
        .iter()
        .map(|(id, f)| (*id, logical_schema(f)))
        .collect();
    for function in program.functions.values() {
        type_states(program, function, &schemas)?;
    }
    program.metadata.symbols.validate(program)?;
    Ok(())
}

fn verify_program_metadata(program: &Program) -> Result<(), FosterError> {
    if let Some(main) = program.metadata.main {
        let main = program
            .functions
            .get(&main)
            .ok_or_else(|| FosterError::runtime("bytecode references a missing `main` function"))?;
        let expected = u16::from(program.metadata.main_arguments);
        if main.parameters != expected || main.captures != 0 {
            return Err(FosterError::runtime(format!(
                "bytecode `main` must have {expected} parameter(s) and no captures"
            )));
        }
    } else if program.metadata.main_arguments {
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
    if remote_used
        || program.metadata.remote_result.is_some()
        || program.metadata.remote_error.is_some()
    {
        let invalid = || FosterError::runtime("bytecode has invalid remote outcome metadata");
        let result = program.metadata.remote_result.ok_or_else(invalid)?;
        let error = program.metadata.remote_error.ok_or_else(invalid)?;
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
                .metadata
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
                        record: program.metadata.string_record.ok_or_else(invalid)?,
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
        program.metadata.string_record,
        program.metadata.symbol_record,
        program.metadata.list_record,
        program.metadata.bytes_record,
        program.metadata.byte_buffer_record,
    ]
    .into_iter()
    .flatten()
    {
        if !program.metadata.records.contains_key(&record) {
            return Err(FosterError::runtime(
                "bytecode wrapper metadata references a missing record",
            ));
        }
    }
    for record in program.metadata.records.values() {
        for field in record.fields() {
            verify_metadata_type(program, &field.ty, 0)?;
        }
    }
    for variant in program.metadata.variants.values() {
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
    for ((nominal, slot), target) in &program.metadata.dispatch {
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
            NominalTypeId::Record(record) => program.metadata.records.contains_key(record),
            NominalTypeId::Variant(variant) => program
                .metadata
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
            if !program.metadata.records.contains_key(record) {
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
                .metadata
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
    function: &Function,
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
            Instruction::CallContractMethod { result_type, .. } => {
                verify_metadata_type(program, result_type, 0)?
            }
            Instruction::LoadConstant { constant, .. }
                if usize::from(*constant) >= program.metadata.constants.len() =>
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
                let Some(metadata) = program.metadata.records.get(record) else {
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
                let expected = metadata.layout().names();
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
            } => match program.metadata.variants.get(variant) {
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
            Instruction::MatchPattern { pattern, .. } => {
                if let crate::hir::Pattern::IsType {
                    target,
                    source,
                    conforming,
                    ..
                } = pattern.unspanned()
                {
                    verify_metadata_type(program, target, 0)?;
                    verify_metadata_type(program, source, 0)?;
                    for witness in conforming {
                        verify_metadata_type(program, witness, 0)?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn verify_type(
    program: &Program,
    function: &Function,
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
            if !program.metadata.records.contains_key(record) {
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
                .metadata
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

fn target_function<'a>(
    program: &'a Program,
    function: &Function,
    index: usize,
    target: FunctionId,
) -> Result<&'a Function, FosterError> {
    program.functions.get(&target).ok_or_else(|| {
        FosterError::runtime(format!(
            "bytecode function `{}` instruction {index} references a missing function",
            function.name
        ))
    })
}

fn pattern_binding_count(pattern: &crate::hir::Pattern) -> usize {
    match pattern.unspanned() {
        crate::hir::Pattern::Binding(_)
        | crate::hir::Pattern::IsType {
            binding: Some(_), ..
        } => 1,
        crate::hir::Pattern::Variant { fields, .. } => {
            fields.iter().map(pattern_binding_count).sum()
        }
        _ => 0,
    }
}

fn invalid_instruction<T>(
    function: &Function,
    index: usize,
    message: impl std::fmt::Display,
) -> Result<T, FosterError> {
    Err(FosterError::runtime(format!(
        "bytecode function `{}` instruction {index} {message}",
        function.name
    )))
}

use crate::codegen::flow::{self, engine};
use crate::codegen::ir::Value;
pub(crate) fn type_states(
    program: &Program,
    function: &Function,
    schemas: &std::collections::HashMap<FunctionId, flow::FunctionSchema>,
) -> Result<Vec<Option<Vec<Option<ExecutableType>>>>, FosterError> {
    let schema = logical_schema(function);
    let body = engine::Body {
        write_bindings: &Default::default(),
        schema: &schema,
        instructions: function.instructions.iter().map(slot_instruction).collect(),
        value_count: usize::from(function.registers),
        entry_values: vec![],
        storage_policy: engine::StoragePolicy::ConsumingSlots,
    };
    engine::analyze_function_flow(
        &engine::Program {
            metadata: &program.metadata,
            functions: schemas,
        },
        &body,
    )
    .map(|states| states.into_iter().map(|s| s.map(|s| s.bindings)).collect())
}
pub(crate) fn slot_instruction(
    instruction: &crate::codegen::storage::Instruction,
) -> engine::Instruction {
    match instruction {
        crate::codegen::storage::Instruction::Drop { register } => engine::Instruction::Drop {
            register: Value(u32::from(register.0)),
        },
        crate::codegen::storage::Instruction::LoadConstant {
            destination,
            constant,
        } => engine::Instruction::LoadConstant {
            destination: Value(u32::from(destination.0)),
            constant: *constant,
        },
        crate::codegen::storage::Instruction::Move {
            destination,
            source,
        } => engine::Instruction::Move {
            destination: Value(u32::from(destination.0)),
            source: Value(u32::from(source.0)),
        },
        crate::codegen::storage::Instruction::Unary {
            destination,
            operator,
            operand,
        } => engine::Instruction::Unary {
            destination: Value(u32::from(destination.0)),
            operator: *operator,
            operand: Value(u32::from(operand.0)),
        },
        crate::codegen::storage::Instruction::Binary {
            destination,
            operator,
            left,
            right,
        } => engine::Instruction::Binary {
            destination: Value(u32::from(destination.0)),
            operator: *operator,
            left: Value(u32::from(left.0)),
            right: Value(u32::from(right.0)),
        },
        crate::codegen::storage::Instruction::MakeList {
            destination,
            element_type,
            elements,
        } => engine::Instruction::MakeList {
            destination: Value(u32::from(destination.0)),
            element_type: element_type.clone(),
            elements: elements.iter().map(|r| Value(u32::from(r.0))).collect(),
        },
        crate::codegen::storage::Instruction::Index {
            destination,
            object,
            index,
        } => engine::Instruction::Index {
            destination: Value(u32::from(destination.0)),
            object: Value(u32::from(object.0)),
            index: Value(u32::from(index.0)),
        },
        crate::codegen::storage::Instruction::MakeRecord {
            destination,
            record,
            type_arguments: _,
            fields,
        } => engine::Instruction::MakeRecord {
            destination: Value(u32::from(destination.0)),
            record: *record,
            fields: fields
                .iter()
                .map(|(a, r)| (a.clone(), Value(u32::from(r.0))))
                .collect(),
        },
        crate::codegen::storage::Instruction::MakeVariant {
            destination,
            variant,
            type_arguments,
            payload,
        } => engine::Instruction::MakeVariant {
            destination: Value(u32::from(destination.0)),
            variant: *variant,
            type_arguments: type_arguments.clone(),
            payload: payload.iter().map(|r| Value(u32::from(r.0))).collect(),
        },
        crate::codegen::storage::Instruction::LoadField {
            destination,
            object,
            field: _,
            by_reference,
        } => engine::Instruction::LoadField {
            destination: Value(u32::from(destination.0)),
            object: Value(u32::from(object.0)),
            by_reference: *by_reference,
        },
        crate::codegen::storage::Instruction::StoreField {
            object,
            field: _,
            source,
        } => engine::Instruction::StoreField {
            object: Value(u32::from(object.0)),
            source: Value(u32::from(source.0)),
        },
        crate::codegen::storage::Instruction::StoreIndex {
            object,
            index,
            source,
        } => engine::Instruction::StoreIndex {
            object: Value(u32::from(object.0)),
            index: Value(u32::from(index.0)),
            source: Value(u32::from(source.0)),
        },
        crate::codegen::storage::Instruction::MakeReference {
            destination,
            pointee_type,
            object,
            index,
        } => engine::Instruction::MakeReference {
            destination: Value(u32::from(destination.0)),
            pointee_type: pointee_type.clone(),
            object: Value(u32::from(object.0)),
            index: Value(u32::from(index.0)),
        },
        crate::codegen::storage::Instruction::MakeWholeReference {
            destination,
            pointee_type,
            object,
        } => engine::Instruction::MakeWholeReference {
            destination: Value(u32::from(destination.0)),
            pointee_type: pointee_type.clone(),
            object: Value(u32::from(object.0)),
        },
        crate::codegen::storage::Instruction::MakeFieldReference {
            destination,
            pointee_type,
            object,
            field,
        } => engine::Instruction::MakeFieldReference {
            destination: Value(u32::from(destination.0)),
            pointee_type: pointee_type.clone(),
            object: Value(u32::from(object.0)),
            field: field.clone(),
        },
        crate::codegen::storage::Instruction::MoveOut {
            by_reference,
            destination,
            source,
        } => engine::Instruction::MoveOut {
            by_reference: *by_reference,
            destination: Value(u32::from(destination.0)),
            source: Value(u32::from(source.0)),
        },
        crate::codegen::storage::Instruction::Push {
            destination,
            object,
            value,
        } => engine::Instruction::Push {
            destination: Value(u32::from(destination.0)),
            object: Value(u32::from(object.0)),
            value: Value(u32::from(value.0)),
        },
        crate::codegen::storage::Instruction::Append {
            destination,
            object,
            value,
        } => engine::Instruction::Append {
            destination: Value(u32::from(destination.0)),
            object: Value(u32::from(object.0)),
            value: Value(u32::from(value.0)),
        },
        crate::codegen::storage::Instruction::Contains {
            destination,
            value,
            candidates,
        } => engine::Instruction::Contains {
            destination: Value(u32::from(destination.0)),
            value: Value(u32::from(value.0)),
            candidates: candidates.iter().map(|r| Value(u32::from(r.0))).collect(),
        },
        crate::codegen::storage::Instruction::Builtin {
            destination,
            builtin,
            arguments,
        } => engine::Instruction::Builtin {
            destination: Value(u32::from(destination.0)),
            builtin: *builtin,
            arguments: arguments.iter().map(|r| Value(u32::from(r.0))).collect(),
        },
        crate::codegen::storage::Instruction::SpawnRemote { destination, value } => {
            engine::Instruction::SpawnRemote {
                destination: Value(u32::from(destination.0)),
                value: Value(u32::from(value.0)),
            }
        }
        crate::codegen::storage::Instruction::SpawnRemoteBorrow {
            destination,
            source,
        } => engine::Instruction::SpawnRemoteBorrow {
            destination: Value(u32::from(destination.0)),
            source: Value(u32::from(source.0)),
        },
        crate::codegen::storage::Instruction::RemoteCall {
            destination,
            remote,
            function,
            arguments,
        } => engine::Instruction::RemoteCall {
            destination: Value(u32::from(destination.0)),
            remote: Value(u32::from(remote.0)),
            function: *function,
            arguments: arguments
                .iter()
                .map(|(a, r)| (*a, Value(u32::from(r.0))))
                .collect(),
        },
        crate::codegen::storage::Instruction::Await {
            destination,
            future,
        } => engine::Instruction::Await {
            destination: Value(u32::from(destination.0)),
            future: Value(u32::from(future.0)),
        },
        crate::codegen::storage::Instruction::MatchPattern {
            destination,
            subject,
            pattern,
            bindings,
        } => engine::Instruction::MatchPattern {
            destination: Value(u32::from(destination.0)),
            subject: Value(u32::from(subject.0)),
            pattern: pattern.clone(),
            bindings: bindings.iter().map(|r| Value(u32::from(r.0))).collect(),
        },
        crate::codegen::storage::Instruction::Jump { target } => {
            engine::Instruction::Jump { target: *target }
        }
        crate::codegen::storage::Instruction::JumpIfFalse { condition, target } => {
            engine::Instruction::JumpIfFalse {
                condition: Value(u32::from(condition.0)),
                target: *target,
            }
        }
        crate::codegen::storage::Instruction::Assert { condition, message } => {
            engine::Instruction::Assert {
                condition: Value(u32::from(condition.0)),
                message: message.map(|r| Value(u32::from(r.0))),
            }
        }
        crate::codegen::storage::Instruction::Call {
            destination,
            function,
            specialization,
            arguments,
        } => engine::Instruction::Call {
            destination: Value(u32::from(destination.0)),
            function: *function,
            specialization: specialization.clone(),
            arguments: arguments.iter().map(|r| Value(u32::from(r.0))).collect(),
        },
        crate::codegen::storage::Instruction::CallMethod {
            destination,
            receiver,
            function,
            specialization,
            arguments,
        } => engine::Instruction::CallMethod {
            destination: Value(u32::from(destination.0)),
            receiver: Value(u32::from(receiver.0)),
            function: *function,
            specialization: specialization.clone(),
            arguments: arguments.iter().map(|r| Value(u32::from(r.0))).collect(),
        },
        crate::codegen::storage::Instruction::CallContractMethod {
            destination,
            receiver,
            slot,
            name,
            arguments,
            result_type,
        } => engine::Instruction::CallContractMethod {
            destination: Value(u32::from(destination.0)),
            receiver: Value(u32::from(receiver.0)),
            slot: *slot,
            name: name.clone(),
            arguments: arguments.iter().map(|r| Value(u32::from(r.0))).collect(),
            result_type: result_type.clone(),
        },
        crate::codegen::storage::Instruction::MakeClosure {
            destination,
            function,
            specialization,
            captures,
        } => engine::Instruction::MakeClosure {
            destination: Value(u32::from(destination.0)),
            function: *function,
            specialization: specialization.clone(),
            captures: captures
                .iter()
                .map(|(a, r)| (*a, Value(u32::from(r.0))))
                .collect(),
        },
        crate::codegen::storage::Instruction::CallValue {
            destination,
            callee,
            arguments,
        } => engine::Instruction::CallValue {
            destination: Value(u32::from(destination.0)),
            callee: Value(u32::from(callee.0)),
            arguments: arguments.iter().map(|r| Value(u32::from(r.0))).collect(),
        },
        crate::codegen::storage::Instruction::CallClosure {
            destination,
            function,
            specialization,
            captures,
            arguments,
        } => engine::Instruction::CallClosure {
            destination: Value(u32::from(destination.0)),
            function: *function,
            specialization: specialization.clone(),
            captures: captures
                .iter()
                .map(|(a, r)| (*a, Value(u32::from(r.0))))
                .collect(),
            arguments: arguments.iter().map(|r| Value(u32::from(r.0))).collect(),
        },
        crate::codegen::storage::Instruction::Return { source } => engine::Instruction::Return {
            source: Value(u32::from(source.0)),
        },
    }
}

#[cfg(test)]
mod type_semantics_tests {
    use super::*;
    use crate::codegen::flow::engine::compatible;

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
        let function = &program.functions[&program.metadata.main.unwrap()];
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
