use super::LowerError;
use super::emission::{FunctionMetadata, lower_function};
use crate::codegen::sealing::seal_program;
use crate::codegen::shared::SharedProgram;
use crate::vm;
use std::collections::HashMap;
/// Backend-only register assignment. Logical analyses have already been sealed.
pub(crate) fn lower_shared_program(shared: SharedProgram) -> Result<vm::Program, LowerError> {
    let source = std::sync::Arc::unwrap_or_clone(shared.program);
    let mut program = vm::Program {
        metadata: source.metadata,
        functions: HashMap::new(),
        drops_inserted: shared.drops_inserted,
    };
    let mut ids = source.functions.keys().copied().collect::<Vec<_>>();
    ids.sort();
    for id in ids {
        let declaration = &source.functions[&id];
        let function = if declaration.intrinsic_stub {
            // Stubs are declarations in shared SSA. The existing bytecode
            // format requires a structural body, although calls to it are forbidden.
            let constant = if let Some(index) = program
                .metadata
                .constants
                .iter()
                .position(|value| *value == vm::Constant::Unit)
            {
                u16::try_from(index).map_err(|_| LowerError("too many VM constants".into()))?
            } else {
                let index = u16::try_from(program.metadata.constants.len())
                    .map_err(|_| LowerError("too many VM constants".into()))?;
                program.metadata.constants.push(vm::Constant::Unit);
                index
            };
            let result = declaration
                .parameters
                .checked_add(declaration.captures)
                .ok_or_else(|| LowerError("intrinsic register prefix overflow".into()))?;
            vm::BytecodeFunction {
                name: declaration.name.clone(),
                intrinsic_stub: true,
                parameters: declaration.parameters,
                parameter_types: declaration.parameter_types.clone(),
                parameter_modes: declaration.parameter_modes.clone(),
                mutable_parameters: declaration.mutable_parameters.clone(),
                returns_reference: declaration.returns_reference,
                captures: declaration.captures,
                capture_types: declaration.capture_types.clone(),
                result_type: declaration.result_type.clone(),
                registers: result
                    .checked_add(1)
                    .ok_or_else(|| LowerError("intrinsic register prefix overflow".into()))?,
                instructions: vec![
                    vm::Instruction::LoadConstant {
                        destination: vm::Register(result),
                        constant,
                    },
                    vm::Instruction::Return {
                        source: vm::Register(result),
                    },
                ],
                instruction_spans: vec![0..0, 0..0],
            }
        } else {
            lower_function(
                &source.bodies[&id],
                &shared.signatures,
                &mut program.metadata.constants,
                FunctionMetadata::from_declaration(declaration),
            )?
        };
        program.functions.insert(id, function);
    }
    Ok(program)
}

/// Make shared SSA the mandatory backend boundary for every executable VM function.
pub fn lower_program_through_shared_ir(program: &mut vm::Program) -> Result<(), LowerError> {
    lower_program_through_shared_ir_with_options(program, false)
}

pub(crate) fn lower_program_through_shared_ir_with_options(
    program: &mut vm::Program,
    optimize: bool,
) -> Result<(), LowerError> {
    // Publish only after sealing, optimization and lowering all succeed.
    let shared = seal_program(program.clone())?;
    let shared = if optimize {
        shared
            .optimized()
            .map_err(|error| LowerError(error.to_string()))?
    } else {
        shared
    };
    *program = lower_shared_program(shared)?;
    Ok(())
}
