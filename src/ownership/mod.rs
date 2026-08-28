mod check;
pub mod diagnostics;
mod effects;
mod lower;
mod mir;
#[cfg(test)]
mod model;
mod regions;

pub use mir::{
    BasicBlock, BlockId, BorrowValue, Function, InvalidationKind, LoanDefinition, LoanId, MirPoint,
    Operation, Program, ProvenanceAnalysis, ProvenanceState, RequiredUse, RequirementAnalysis,
    RequirementState, ResultProvenance, ReturnKind, Terminator, UseMode,
};

/// Source-language compatibility level. Increment for intentional breaking
/// syntax or type-system changes.
pub const LANGUAGE_VERSION: u16 = 7;

/// Normative ownership-contract compatibility level. Increment whenever a
/// previously accepted safe program is intentionally rejected or an ownership
/// guarantee changes meaning.
pub const MODEL_VERSION: u16 = 1;

use crate::error::FosterError;
use crate::hir::PackageHir;
use crate::types::TypeInformation;

pub(crate) fn build_and_check(
    hir: &PackageHir,
    types: &TypeInformation,
) -> Result<Program, FosterError> {
    let mut program = lower::lower(hir, types);
    program.provenance = regions::analyze(&program);
    regions::populate_reborrow_parents(&mut program);
    regions::infer_result_provenance(hir, &mut program);
    program.requirements = regions::analyze_requirements(&program);
    check::check(hir, types, &program)?;
    regions::validate(hir, &program)?;
    Ok(program)
}
