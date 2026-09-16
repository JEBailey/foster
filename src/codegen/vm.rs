//! VM lowering and transactional slot/SSA adapters.

mod emission;

mod instructions;
mod program;
#[cfg(test)]
mod tests;

pub use emission::{FunctionMetadata, lower_function};

pub use program::lower_program_through_shared_ir;
pub(crate) use program::lower_shared_program;

pub use crate::codegen::LowerError;
