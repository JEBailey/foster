mod check;
mod lower;
mod mir;

pub use mir::{BasicBlock, BlockId, Function, Operation, Program, Terminator, UseMode};

use crate::error::FosterError;
use crate::hir::PackageHir;
use crate::types::TypeInformation;

pub(crate) fn build_and_check(
    hir: &PackageHir,
    types: &TypeInformation,
) -> Result<Program, FosterError> {
    let program = lower::lower(hir, types);
    check::check(hir, types, &program)?;
    Ok(program)
}
