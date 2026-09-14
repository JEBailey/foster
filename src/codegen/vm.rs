//! Shared program sealing and VM/SSA adapters.
mod construction;
mod emission;
#[cfg(test)]
mod evidence;
mod instructions;
mod program;
#[cfg(test)]
mod tests;

pub use construction::seal_function;
pub use emission::{FunctionMetadata, lower_function};

pub(crate) use program::lower_shared_program;
pub use program::{SharedProgram, lower_program_through_shared_ir, seal_program};

use std::fmt;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LowerError(String);

impl fmt::Display for LowerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for LowerError {}
