mod callables;
mod check;
pub mod diagnostics;
mod effects;
mod lower;
mod mir;
#[cfg(test)]
mod model;
mod regions;
mod remote;

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
    build_and_check_collecting(hir, types, false)
        .map_err(|errors| errors.into_iter().next().unwrap())
}

pub(crate) fn build_and_check_collecting(
    hir: &PackageHir,
    types: &TypeInformation,
    collect: bool,
) -> Result<Program, Vec<FosterError>> {
    // Start with no assumed origins. Each pass adds provenance proven by a reachable MIR return,
    // so direct-call chains and recursive call graphs converge on the least fixed-point summary.
    let mut summaries = hir
        .functions
        .iter()
        .map(|(id, _)| (id, ResultProvenance::default()))
        .collect::<std::collections::HashMap<_, _>>();
    loop {
        crate::compiler::cancellation::check().map_err(|e| vec![e])?;
        crate::compiler::profile::count("ownership.iterations");
        let program = lower_and_infer(hir, types, &summaries).map_err(|e| vec![e])?;
        crate::compiler::cancellation::check().map_err(|e| vec![e])?;
        let inferred = program
            .functions
            .iter()
            .map(|(id, function)| (*id, function.result_provenance.clone()))
            .collect::<std::collections::HashMap<_, _>>();
        if inferred == summaries {
            break crate::compiler::profile::measure("ownership.validate", || {
                finish_check(hir, types, program, collect)
            });
        }
        summaries = inferred;
    }
}

fn finish_check(
    hir: &PackageHir,
    types: &TypeInformation,
    mut program: Program,
    collect: bool,
) -> Result<Program, Vec<FosterError>> {
    program.requirements = crate::compiler::profile::measure("ownership.requirements", || {
        regions::analyze_requirements(&program)
    });
    if collect {
        let mut errors = Vec::new();
        let mut functions = program.functions.keys().copied().collect::<Vec<_>>();
        functions.sort();
        for function in functions {
            crate::compiler::cancellation::check().map_err(|e| vec![e])?;
            let result = check::check_function(hir, types, function, &program.functions[&function])
                .and_then(|_| regions::validate_function(hir, types, &program, function))
                .and_then(|_| remote::check_function(hir, types, &program, function));
            if let Err(error) = result {
                errors.push(error);
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }
    } else {
        check::check(hir, types, &program).map_err(|e| vec![e])?;
        regions::validate(hir, types, &program).map_err(|e| vec![e])?;
        remote::check(hir, types, &program).map_err(|e| vec![e])?;
    }
    Ok(program)
}

fn lower_and_infer(
    hir: &PackageHir,
    types: &TypeInformation,
    result_provenance: &std::collections::HashMap<crate::hir::FunctionId, ResultProvenance>,
) -> Result<Program, FosterError> {
    let mut program = crate::compiler::profile::measure("ownership.lower", || {
        lower::lower(hir, types, result_provenance)
    })?;
    program.provenance =
        crate::compiler::profile::measure("ownership.provenance", || regions::analyze(&program));
    crate::compiler::profile::measure("ownership.reborrow", || {
        regions::populate_reborrow_parents(&mut program)
    });
    crate::compiler::profile::measure("ownership.summary", || {
        regions::infer_result_provenance(hir, &mut program)
    });
    Ok(program)
}
