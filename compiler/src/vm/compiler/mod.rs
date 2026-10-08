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
pub fn compile_library(compilation: &Compilation) -> Result<Program, FosterError> {
    let mut program = crate::codegen::construction::compile(compilation)?;
    // Validate through the mandatory shared-SSA boundary, but retain logical
    // slots for linking. De-SSA bytecode can assign different branch values to
    // one home; reconstructing type hints from that finalized register stream
    // loses the original construction evidence when consumers seal it again.
    crate::codegen::sealing::seal_program(program.clone())
        .map_err(|e| FosterError::runtime(e.to_string()))?;
    program.metadata.main = None;
    program.metadata.main_arguments = false;
    program.metadata.symbols = crate::symbols::from_compilation(compilation, &program)?;
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

/// Source-local bytecode registers indexed by function and local declaration.
pub type DebugLocalRegisters = std::collections::HashMap<
    crate::hir::FunctionId,
    std::collections::HashMap<crate::hir::LocalId, super::Register>,
>;

/// Generate unoptimized bytecode and the source-local register map used by a debugger.
pub fn compile_debug(
    compilation: &Compilation,
) -> Result<(Program, DebugLocalRegisters), FosterError> {
    let mut locals = std::collections::HashMap::new();
    let shared = crate::codegen::program::compile_with_locals(compilation, Some(&mut locals))?;
    let mut program = crate::codegen::vm::lower_shared_program(shared)
        .map_err(|e| FosterError::runtime(e.to_string()))?;
    super::optimizer::finalize_register_drops(&mut program);
    crate::symbols::link(&mut program)?;
    super::verify(&program)?;
    Ok((program, locals))
}
