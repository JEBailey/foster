//! Immutable shared-program boundary and transactional backend orchestration.
use super::LowerError;
use super::construction::seal_function_with_evidence;
use super::emission::{FunctionMetadata, lower_function};
#[cfg(test)]
use super::evidence::compare_flow;
use crate::codegen::flow::{FunctionFacts, FunctionSchema};
use crate::codegen::ir::{self};
use crate::hir::FunctionId;

pub use crate::codegen::shared::SharedProgram;
use crate::vm::{self};
use std::collections::HashMap;
/// Retain the compiler's first SSA graph instead of immediately de-SSA lowering it to bytecode.
pub fn seal_program(mut construction: vm::Program) -> Result<SharedProgram, LowerError> {
    let layouts = crate::codegen::layout::legalize(&mut construction)
        .map_err(|e| LowerError(e.to_string()))?;
    let SealedFunctions {
        functions,
        signatures,
        schemas,
        facts,
        write_bindings,
    } = seal_construction(&construction)?;
    Ok(SharedProgram {
        program: crate::codegen::program::Program {
            metadata: construction.metadata,
            functions: construction
                .functions
                .iter()
                .map(|(id, body)| {
                    (
                        *id,
                        crate::codegen::program::FunctionDeclaration::from_construction(body),
                    )
                })
                .collect(),
            bodies: functions,
        },
        layouts,
        drops_inserted: construction.drops_inserted,
        signatures,
        schemas,
        facts,
        write_bindings,
    })
}

/// Backend-only register assignment. Logical analyses have already been sealed.
pub(crate) fn lower_shared_program(shared: SharedProgram) -> Result<vm::Program, LowerError> {
    let mut program = vm::Program {
        metadata: shared.program.metadata,
        functions: HashMap::new(),
        drops_inserted: shared.drops_inserted,
    };
    let mut ids = shared.program.functions.keys().copied().collect::<Vec<_>>();
    ids.sort();
    for id in ids {
        let declaration = &shared.program.functions[&id];
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
                &shared.program.bodies[&id],
                &shared.signatures,
                &mut program.metadata.constants,
                FunctionMetadata::from_declaration(declaration),
            )?
        };
        program.functions.insert(id, function);
    }
    Ok(program)
}

/// Both program consumers receive the same complete, verified SSA graph and signatures.
struct SealedFunctions {
    write_bindings: HashMap<FunctionId, HashMap<ir::Value, ir::Value>>,
    schemas: HashMap<FunctionId, FunctionSchema>,
    facts: HashMap<FunctionId, FunctionFacts>,
    functions: HashMap<FunctionId, ir::Function>,
    signatures: HashMap<FunctionId, ir::Signature>,
}

fn seal_construction(construction: &vm::Program) -> Result<SealedFunctions, LowerError> {
    vm::verify(construction)
        .map_err(|error| LowerError(format!("invalid construction metadata: {error}")))?;
    let result_types = construction
        .functions
        .iter()
        .map(|(id, function)| (*id, function.result_type.clone()))
        .collect::<HashMap<_, _>>();
    let schemas = construction
        .functions
        .iter()
        .map(|(id, body)| {
            (
                *id,
                crate::codegen::program::FunctionDeclaration::from_construction(body).schema(),
            )
        })
        .collect();
    let mut facts = HashMap::new();
    let mut source_maps = HashMap::new();
    let mut functions = HashMap::with_capacity(construction.functions.len());
    for (id, function) in &construction.functions {
        if function.intrinsic_stub {
            continue;
        }
        let (shared, _sources) =
            seal_function_with_evidence(&construction.metadata.constants, &result_types, function)?;
        source_maps.insert(*id, _sources);
        functions.insert(*id, shared);
    }
    let signatures = functions
        .iter()
        .map(|(id, function)| (*id, function.signature.clone()))
        .collect::<HashMap<_, _>>();
    for function in functions.values() {
        function
            .verify(&signatures)
            .map_err(|error| LowerError(format!("invalid shared IR: {error}")))?;
    }
    for (id, shared) in &functions {
        let analyzed = crate::codegen::flow::analyze(
            &construction.metadata,
            &schemas,
            &schemas[id],
            shared,
            &source_maps[id].write_bindings,
        )
        .map_err(|e| LowerError(e.to_string()))?;
        #[cfg(test)]
        compare_flow(
            construction,
            &construction.functions[id],
            shared,
            &source_maps[id],
            &analyzed,
            &schemas,
        )?;
        facts.insert(*id, analyzed);
    }
    Ok(SealedFunctions {
        write_bindings: source_maps
            .into_iter()
            .map(|(id, evidence)| (id, evidence.write_bindings))
            .collect(),
        functions,
        signatures,
        schemas,
        facts,
    })
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
