//! Shared, target-independent executable IR for code-generation backends.

pub mod flow;
pub mod ir;
pub mod layout;
pub mod metadata;
pub(crate) mod optimizer;
pub mod program;
pub mod shared;
pub(crate) mod type_conversion;
pub mod types;
pub mod vm;

pub use program::compile;

pub(crate) mod construction;
pub mod sealing;
pub mod storage;

use std::fmt;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LowerError(pub(crate) String);

impl fmt::Display for LowerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for LowerError {}
