//! Local call, closure, return, and argument/capture ownership rules.
use super::state::readable_type;
use super::state::{FlowState, bound_type, read_type, take_type, write_type};
use super::types::{
    callable_type, compatible, invalid_instruction, require_type, type_error,
    verification_field_type,
};
use super::{Body, Instruction, Program};
use crate::ast::ParameterMode;
use crate::codegen::{ir::Value, types::ExecutableType};
use crate::error::FosterError;
use crate::hir::CaptureMode;
use crate::types::NominalTypeId;
pub(super) fn transfer(
    program: &Program,
    function: &Body,
    index: usize,
    mut state: FlowState,
) -> Result<Vec<(usize, FlowState)>, FosterError> {
    let next = index + 1;
    match &function.instructions[index] {
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
        _ => unreachable!("non-call operations are handled by transfer"),
    }
    Ok(vec![(next, state)])
}
pub(super) fn verify_arguments<'a>(
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

pub(super) fn verify_captures(
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
