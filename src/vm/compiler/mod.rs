use super::Program;
use crate::compiler::Compilation;
use crate::error::FosterError;

pub fn compile(compilation: &Compilation) -> Result<Program, FosterError> {
    compile_with_options(compilation, CompileOptions::default())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompileOptions {
    pub optimize: bool,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self { optimize: true }
    }
}

/// Compiled generic library bodies, sealed through SSA but not finalized with register drops.
pub(crate) fn compile_library(compilation: &Compilation) -> Result<Program, FosterError> {
    let construction = crate::codegen::construction::compile(compilation)?;
    let shared = crate::codegen::sealing::seal_program(construction)
        .map_err(|e| FosterError::runtime(e.to_string()))?;
    let mut program = crate::codegen::vm::lower_shared_program(shared)
        .map_err(|e| FosterError::runtime(e.to_string()))?;
    program.metadata.main = None;
    program.metadata.main_arguments = false;
    program.metadata.symbols = crate::symbols::Table::from_compilation(compilation, &program)?;
    super::verifier::verify(&program)?;
    Ok(program)
}

pub fn compile_with_options(
    compilation: &Compilation,
    options: CompileOptions,
) -> Result<Program, FosterError> {
    crate::compiler::profile::compilation("vm", || compile_backend(compilation, options))
}

fn compile_backend(
    compilation: &Compilation,
    options: CompileOptions,
) -> Result<Program, FosterError> {
    use crate::compiler::profile::measure;
    let shared = crate::codegen::compile(compilation)?;
    let shared = if options.optimize {
        shared.optimized()?
    } else {
        shared
    };
    let mut program = measure("vm.lower", || {
        crate::codegen::vm::lower_shared_program(shared)
    })
    .map_err(|error| FosterError::runtime(format!("shared VM lowering failed: {error}")))?;
    if options.optimize {
        measure("vm.optimize", || {
            super::optimizer::finish_backend(&mut program)
        });
    }
    measure("vm.drops", || {
        super::optimizer::finalize_register_drops(&mut program)
    });
    measure("vm.link", || crate::symbols::link(&mut program))?;
    Ok(program)
}
