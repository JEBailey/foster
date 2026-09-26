use crate::{
    ast,
    codegen::storage::Program,
    compiler::Compilation,
    error::FosterError,
    hir::{FunctionId, ModuleId},
    types::{Type, TypeId},
};
pub use foster_bytecode::symbols::*;
use std::collections::{BTreeMap, BTreeSet};
mod lowering;
pub use lowering::from_compilation;
#[cfg(test)]
mod tests;

fn error(message: impl Into<String>) -> FosterError {
    FosterError::runtime(format!("symbolic linkage: {}", message.into()))
}
fn function_id(id: u32) -> FunctionId {
    FunctionId::from_raw(la_arena::RawIdx::from_u32(id))
}
