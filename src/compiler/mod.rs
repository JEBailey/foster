//! Foster's checked-compilation facade.
//!
//! This module owns orchestration across the compiler phases. Individual representations such as
//! AST and HIR do not initiate later phases; callers enter the checked pipeline here.

mod pipeline;

use crate::error::FosterError;
use crate::package::Package;

/// A package after lowering, type checking, effect validation, and ownership analysis.
#[derive(Debug)]
pub struct Compilation {
    pub package: Package,
    pub hir: crate::hir::PackageHir,
    pub types: crate::types::TypeInformation,
    pub diagnostics: Vec<crate::diagnostic::Diagnostic>,
    pub ownership: crate::ownership::Program,
}

/// Stateless facade for running Foster's checked front-end pipeline.
///
/// The type provides a stable home for compiler configuration and reusable source state as those
/// facilities are introduced; phase ordering remains private to this module.
#[derive(Debug, Default)]
pub struct Compiler {
    _private: (),
}

impl Compiler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn check(&self, package: Package) -> Result<Compilation, FosterError> {
        let diagnostic_package = package.clone();
        pipeline::check(package)
            .map_err(|error| diagnostic_package.locate_compiler_error(FosterError::from(error)))
    }
}

/// Check a fully loaded package through every front-end analysis phase.
pub fn check(package: Package) -> Result<Compilation, FosterError> {
    Compiler::new().check(package)
}
