use crate::{
    ast,
    hir::{RecordId, VariantTypeId},
};
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// A callable parameter whose type and ownership mode travel together through compiler phases.
/// Pairing enforces structural consistency; it does not itself establish type conformance.
pub struct Parameter<T> {
    pub ty: T,
    pub mode: ast::ParameterMode,
}

impl<T> Parameter<T> {
    /// Validate a boundary that carries types and ownership modes separately.
    pub fn try_from_parts(
        types: Vec<T>,
        modes: Vec<ast::ParameterMode>,
    ) -> Result<Vec<Self>, ParameterCountMismatch> {
        if types.len() != modes.len() {
            return Err(ParameterCountMismatch);
        }
        Ok(types
            .into_iter()
            .zip(modes)
            .map(|(ty, mode)| Self { ty, mode })
            .collect())
    }

    pub fn from_parts(types: Vec<T>, modes: Vec<ast::ParameterMode>) -> Vec<Self> {
        Self::try_from_parts(types, modes).expect("parameter types and modes must align")
    }

    pub fn map<U>(self, map: impl FnOnce(T) -> U) -> Parameter<U> {
        Parameter {
            ty: map(self.ty),
            mode: self.mode,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParameterCountMismatch;

impl std::fmt::Display for ParameterCountMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("parameter types and modes must align")
    }
}
impl std::error::Error for ParameterCountMismatch {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DispatchSlot(pub u32);

/// Compiler-owned capability dispatch; source method slots occupy the lower range.
pub const COPY_SLOT: DispatchSlot = DispatchSlot(u32::MAX);
pub const CAN_COPY_SLOT: DispatchSlot = DispatchSlot(u32::MAX - 1);
pub const DEINIT_SLOT: DispatchSlot = DispatchSlot(u32::MAX - 2);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NominalTypeId {
    Record(RecordId),
    Variant(VariantTypeId),
}
