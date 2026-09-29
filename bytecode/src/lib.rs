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
        Ok(foster_vm::Machine::new(&program.into_verified()?).run_main()?)
    }
}

/// Immutable executable whose metadata, instructions, and types have been verified.
/// ```compile_fail
/// fn invalidate(mut program: foster_bytecode::VerifiedProgram) {
///     program.functions.clear();
/// }
/// ```
#[derive(Debug, Clone)]
pub struct VerifiedProgram(std::sync::Arc<Program>);
impl VerifiedProgram {
    /// Decode and link once; decoding already performs complete verification.
    pub fn decode(bytes: &[u8]) -> Result<Self, BinaryError> {
        decode_program(bytes).map(|program| Self(std::sync::Arc::new(program)))
    }

    pub fn new(program: Program) -> Result<Self, error::FosterError> {
        verify(&program)?;
        Ok(Self(std::sync::Arc::new(program)))
    }
}
impl std::ops::Deref for VerifiedProgram {
    type Target = Program;
    fn deref(&self) -> &Program {
        &self.0
    }
}
impl Program {
    pub fn into_verified(self) -> Result<VerifiedProgram, error::FosterError> {
        VerifiedProgram::new(self)
    }
}
#[cfg(test)]
mod verified_tests {
    use super::*;
    #[test]
    fn verification_rejects_a_missing_entry_and_shares_valid_programs() {
        let mut invalid = Program::default();
        invalid.metadata.main = Some(hir::FunctionId::from_raw(la_arena::RawIdx::from_u32(0)));
        assert!(invalid.into_verified().is_err());
        let checked = Program::default().into_verified().unwrap();
        let copy = checked.clone();
        assert!(std::ptr::eq(&*checked, &*copy));
        let bytes = encode_program(&checked).unwrap();
        assert_eq!(*VerifiedProgram::decode(&bytes).unwrap(), *checked);
    }
}
