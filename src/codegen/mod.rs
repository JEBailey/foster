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
