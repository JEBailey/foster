//! Execution of validated Foster bytecode, independent of the compiler.
#![allow(clippy::result_large_err)]
pub use foster_bytecode::{
    BinaryError, BytecodeFunction, Constant, FORMAT_VERSION, Instruction, Program, ProgramMetrics,
    Register, RuntimeRecord, RuntimeVariant, decode_program, encode_program, verify,
};
pub use foster_bytecode::{ast, codegen, entry, error, hir, intrinsics, types};
pub(crate) mod builtins;
pub mod debug;
mod entropy;
mod handlers;
mod host;
mod machine;
pub mod operations;
mod patterns;
mod runtime;
pub mod value;
use foster_host::{process, remote};
pub use host::HostContext;
pub use machine::{Machine, release_value};
pub use runtime::Capture;
pub use value::Value;
mod foreign {
    pub use foster_host::foreign as runtime;
}
// Execution modules use a common namespace for VM values and host state.
pub(crate) use crate as vm;
pub fn run(program: &Program) -> Result<Value, error::FosterError> {
    verify(program)?;
    Machine::new(program).run_main()
}
