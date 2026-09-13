//! Shared, target-independent executable IR for code-generation backends.

pub mod ir;
pub mod layout;
pub mod metadata;
pub(crate) mod type_conversion;
pub mod types;
pub mod vm;
