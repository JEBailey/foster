//! Foster executable contracts, binary encoding, linkage, and validation.
#![allow(clippy::result_large_err)]
pub mod ast;
mod binary;
pub mod codegen;
pub mod entry;
pub mod error;
pub mod hir;
pub mod intrinsics;
pub mod native;
pub mod symbols;
pub mod types;
pub use binary::{BinaryError, FORMAT_VERSION, decode_program, encode_program};
pub use codegen::metadata::{Constant, RuntimeRecord, RuntimeVariant};
pub use codegen::storage::verification::verify;
pub use codegen::storage::{
    Function as BytecodeFunction, Instruction, Program, ProgramMetrics, Slot as Register,
};

#[cfg(test)]
mod test_support {
    use super::*;
    pub use foster_compiler::vm::CompileOptions;
    // A unit-test build has its own crate identity. Cross the actual wire boundary
    // when importing compiler fixtures, exactly as an independent bytecode consumer does.
    pub fn import(program: foster_compiler::vm::Program) -> Program {
        decode_program(&foster_compiler::vm::encode_program(&program).unwrap()).unwrap()
    }
    pub fn compile_with_options(
        compilation: &foster_compiler::compiler::Compilation,
        options: CompileOptions,
    ) -> Result<Program, Box<dyn std::error::Error>> {
        Ok(import(foster_compiler::vm::compile_with_options(
            compilation,
            options,
        )?))
    }
    pub fn compile(
        compilation: &foster_compiler::compiler::Compilation,
    ) -> Result<Program, Box<dyn std::error::Error>> {
        compile_with_options(compilation, CompileOptions::default())
    }
    pub fn run(program: &Program) -> Result<foster_vm::Value, Box<dyn std::error::Error>> {
        let program = foster_compiler::vm::decode_program(&encode_program(program)?)?;
        Ok(foster_vm::Machine::new(&program).run_main()?)
    }
}
