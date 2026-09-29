//! Toolchain orchestration joining compilation and VM execution.
pub use foster_compiler::vm::{
    CompileOptions, compile, compile_debug, compile_with_options, optimize,
};
pub use foster_vm::*;
pub fn run(compilation: &crate::compiler::Compilation) -> Result<Value, crate::error::FosterError> {
    run_with_options(compilation, CompileOptions::default())
}

pub fn run_with_options(
    compilation: &crate::compiler::Compilation,
    options: CompileOptions,
) -> Result<Value, crate::error::FosterError> {
    let program = compile_with_options(compilation, options)?;
    let program = program.into_verified()?;
    Machine::new(&program).run_main()
}

pub fn run_with_arguments(
    compilation: &crate::compiler::Compilation,
    options: CompileOptions,
    arguments: &crate::entry::CommandArguments,
) -> Result<Value, crate::error::FosterError> {
    let program = compile_with_options(compilation, options)?;
    let program = program.into_verified()?;
    Machine::new(&program).run_main_with_arguments(arguments)
}
