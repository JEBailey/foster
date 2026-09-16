//! Backend-neutral executable declarations and the authoritative SSA bodies.
use super::{ir, metadata::ProgramMetadata, types::ExecutableType};
use crate::ast::ParameterMode;
use crate::hir::FunctionId;
use std::collections::HashMap;

/// Callable metadata has no instructions or register allocation state.
#[derive(Debug, Clone)]
pub struct FunctionDeclaration {
    pub name: String,
    pub intrinsic_stub: bool,
    pub parameters: u16,
    pub parameter_types: Vec<ExecutableType>,
    pub parameter_modes: Vec<ParameterMode>,
    pub mutable_parameters: Vec<bool>,
    pub returns_reference: bool,
    pub captures: u16,
    pub capture_types: Vec<ExecutableType>,
    pub result_type: ExecutableType,
}

impl FunctionDeclaration {
    pub(crate) fn from_construction(body: &super::storage::Function) -> Self {
        Self {
            name: body.name.clone(),
            intrinsic_stub: body.intrinsic_stub,
            parameters: body.parameters,
            parameter_types: body.parameter_types.clone(),
            parameter_modes: body.parameter_modes.clone(),
            mutable_parameters: body.mutable_parameters.clone(),
            returns_reference: body.returns_reference,
            captures: body.captures,
            capture_types: body.capture_types.clone(),
            result_type: body.result_type.clone(),
        }
    }
    pub(crate) fn schema(&self) -> super::flow::FunctionSchema {
        super::flow::FunctionSchema {
            name: self.name.clone(),
            parameters: crate::types::Parameter::from_parts(
                self.parameter_types.clone(),
                self.parameter_modes.clone(),
            ),
            captures: self.capture_types.clone(),
            result_type: self.result_type.clone(),
            returns_reference: self.returns_reference,
            intrinsic_stub: self.intrinsic_stub,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Program {
    pub metadata: ProgramMetadata,
    pub functions: HashMap<FunctionId, FunctionDeclaration>,
    pub bodies: HashMap<FunctionId, ir::Function>,
}

impl Program {
    pub(crate) fn instructions(
        &self,
        function: FunctionId,
    ) -> impl Iterator<Item = &ir::PortableInstruction> {
        self.bodies
            .get(&function)
            .into_iter()
            .flat_map(|body| &body.blocks)
            .flat_map(|block| &block.instructions)
            .filter_map(|entry| match &entry.instruction {
                ir::Instruction::Portable(instruction) => Some(instruction),
                _ => None,
            })
    }
}
/// Compile directly to the typed shared-SSA boundary without de-SSA bytecode lowering.
pub fn compile(
    compilation: &crate::compiler::Compilation,
) -> Result<super::shared::SharedProgram, crate::error::FosterError> {
    use crate::compiler::profile::measure;
    let mut program = measure("shared.construction", || {
        super::construction::compile(compilation)
    })?;

    // Shared lifetime lowering emits ownership releases over logical slots.
    // Sealing preserves those releases and storage identities in SSA for both backends.
    measure("shared.drops", || {
        super::storage::lifetimes::insert(&mut program)
    });
    program.metadata.symbols = crate::symbols::Table::from_compilation(compilation, &program)?;
    crate::symbols::link(&mut program)?;
    measure("shared.seal", || super::sealing::seal_program(program)).map_err(|error| {
        crate::error::FosterError::runtime(format!("shared SSA sealing failed: {error}"))
    })
}
