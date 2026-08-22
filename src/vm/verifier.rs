use crate::error::FosterError;

use super::{Instruction, Program};

pub fn verify(program: &Program) -> Result<(), FosterError> {
    for (id, function) in &program.functions {
        if function.parameter_modes.len() != usize::from(function.parameters)
            || function.mutable_parameters.len() != usize::from(function.parameters)
        {
            return Err(FosterError::runtime(format!(
                "bytecode function `{}` has invalid parameter metadata",
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
                "bytecode function {:?} has mismatched instruction and span tables",
                id.into_raw()
            )));
        }
        for instruction in &function.instructions {
            let mut invalid = None;
            instruction.visit_registers(|register| {
                if register.0 >= function.registers {
                    invalid = Some(register.0);
                }
            });
            if let Some(register) = invalid {
                return Err(FosterError::runtime(format!(
                    "bytecode function `{}` references r{register} outside its {}-register frame",
                    function.name, function.registers
                )));
            }
            match instruction {
                Instruction::LoadConstant { constant, .. }
                    if usize::from(*constant) >= program.constants.len() =>
                {
                    return Err(FosterError::runtime(format!(
                        "bytecode function `{}` references missing constant {constant}",
                        function.name
                    )));
                }
                Instruction::Call {
                    function: target, ..
                }
                | Instruction::CallMethod {
                    function: target, ..
                }
                | Instruction::RemoteCall {
                    function: target, ..
                } if !program.functions.contains_key(target) => {
                    return Err(FosterError::runtime(format!(
                        "bytecode function `{}` calls a missing function",
                        function.name
                    )));
                }
                Instruction::CallClosure {
                    function: target,
                    captures,
                    arguments,
                    ..
                } if !program.functions.contains_key(target)
                    || captures.len() != usize::from(program.functions[target].captures)
                    || arguments.len() != usize::from(program.functions[target].parameters) =>
                {
                    return Err(FosterError::runtime(format!(
                        "bytecode function `{}` has an invalid specialized closure call",
                        function.name
                    )));
                }
                Instruction::MakeClosure {
                    function: target,
                    captures,
                    ..
                } if !program.functions.contains_key(target) => {
                    return Err(FosterError::runtime(format!(
                        "bytecode function `{}` closes over a missing function",
                        function.name
                    )));
                }
                Instruction::MakeClosure {
                    function: target,
                    captures,
                    ..
                } if captures.len() != usize::from(program.functions[target].captures) => {
                    return Err(FosterError::runtime(format!(
                        "bytecode function `{}` constructs a closure with the wrong capture layout",
                        function.name
                    )));
                }
                Instruction::Call {
                    function: target,
                    arguments,
                    ..
                } if arguments.len() != usize::from(program.functions[target].parameters) => {
                    return Err(FosterError::runtime(format!(
                        "bytecode function `{}` calls a function with the wrong arity",
                        function.name
                    )));
                }
                Instruction::Jump { target } | Instruction::JumpIfFalse { target, .. }
                    if *target >= function.instructions.len() =>
                {
                    return Err(FosterError::runtime(format!(
                        "bytecode function `{}` has invalid jump target {target}",
                        function.name
                    )));
                }
                Instruction::MakeRecord { record, .. } if !program.records.contains_key(record) => {
                    return Err(FosterError::runtime("bytecode references a missing record"));
                }
                Instruction::MakeVariant { variant, .. }
                    if !program.variants.contains_key(variant) =>
                {
                    return Err(FosterError::runtime(
                        "bytecode references a missing variant",
                    ));
                }
                _ => {}
            }
        }
    }
    Ok(())
}
