mod check;
pub mod diagnostics;
mod effects;
mod lower;
mod mir;
#[cfg(test)]
mod model;
mod regions;

pub use mir::{
    BasicBlock, BlockId, BorrowValue, Comparison, ComparisonKind, ComparisonOperand, Function,
    InvalidationKind, LoanDefinition, LoanId, MirPoint, Operation, Place, PlaceRoot, Program,
    ProvenanceAnalysis, ProvenanceState, RequiredUse, RequirementAnalysis, RequirementState,
    ResultProvenance, ReturnKind, TemporaryId, Terminator, UseMode,
};

/// Current source-language revision; does not select older semantics.
pub const LANGUAGE_VERSION: u16 = 7;

/// Current ownership-contract revision; does not select older semantics.
pub const MODEL_VERSION: u16 = 3;

use crate::error::FosterError;
use crate::hir::PackageHir;
use crate::types::TypeInformation;

pub(crate) fn build_and_check(
    hir: &PackageHir,
    types: &TypeInformation,
) -> Result<Program, FosterError> {
    // Start with no assumed origins. Each pass adds provenance proven by a reachable MIR return,
    // so direct-call chains and recursive call graphs converge on the least fixed-point summary.
    let mut summaries = hir
        .functions
        .iter()
        .map(|(id, _)| (id, ResultProvenance::default()))
        .collect::<std::collections::HashMap<_, _>>();
    loop {
        let program = lower_and_infer(hir, types, &summaries);
        let inferred = program
            .functions
            .iter()
            .map(|(id, function)| (*id, function.result_provenance.clone()))
            .collect::<std::collections::HashMap<_, _>>();
        if inferred == summaries {
            break finish_check(hir, types, program);
        }
        summaries = inferred;
    }
}

fn finish_check(
    hir: &PackageHir,
    types: &TypeInformation,
    mut program: Program,
) -> Result<Program, FosterError> {
    program.requirements = regions::analyze_requirements(&program);
    check::check(hir, types, &program)?;
    regions::validate(hir, types, &program)?;
    Ok(program)
}

fn lower_and_infer(
    hir: &PackageHir,
    types: &TypeInformation,
    result_provenance: &std::collections::HashMap<crate::hir::FunctionId, ResultProvenance>,
) -> Program {
    let mut program = lower::lower(hir, types, result_provenance);
    program.provenance = regions::analyze(&program);
    regions::populate_reborrow_parents(&mut program);
    regions::infer_result_provenance(hir, &mut program);
    program
}
