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

/// Check a package for interactive tooling, recovering from failures confined to function bodies.
///
/// A failed body is replaced with an empty body while its declaration and signature remain in the
/// package. The pipeline is then restarted, so unrelated functions receive type information from
/// the current source rather than from the language server's last-good snapshot. Strict compiler
/// entry points continue to reject the original program.
pub(crate) fn check_recovering(mut package: Package) -> Result<Compilation, FosterError> {
    #[derive(Clone)]
    struct RecoveredBody {
        module: String,
        span: std::ops::Range<usize>,
        error: FosterError,
    }

    let recoverable_bodies = package
        .modules
        .values()
        .filter_map(|module| module.program.as_ref())
        .map(|program| program.functions.len() + program.tests.len())
        .sum::<usize>();
    let mut recovered = Vec::<RecoveredBody>::new();

    for _ in 0..=recoverable_bodies {
        match check(package.clone()) {
            Ok(mut compilation) => {
                compilation.diagnostics.retain(|diagnostic| {
                    !recovered.iter().any(|body| {
                        diagnostic.source_module.as_deref() == Some(body.module.as_str())
                            && diagnostic.labels.iter().any(|label| {
                                body.span.start <= label.range.start
                                    && label.range.start < body.span.end
                            })
                    })
                });
                compilation.diagnostics.extend(recovered.iter().map(|body| {
                    let source = compilation
                        .package
                        .modules
                        .get(&body.module)
                        .and_then(|module| module.source.as_deref())
                        .unwrap_or_default();
                    crate::diagnostic::Diagnostic::from_source_error(source, &body.error)
                }));
                return Ok(compilation);
            }
            Err(error) => {
                let Some((module, span)) = recover_function_body(&mut package, &error) else {
                    return Err(error);
                };
                recovered.push(RecoveredBody {
                    module,
                    span,
                    error,
                });
            }
        }
    }

    unreachable!("semantic recovery makes progress or returns the unrecoverable error")
}

fn recover_function_body(
    package: &mut Package,
    error: &FosterError,
) -> Option<(String, std::ops::Range<usize>)> {
    let module_name = error.source_module.as_deref()?;
    let range = error
        .labels
        .iter()
        .find(|label| label.primary)
        .or_else(|| error.labels.first())?
        .range
        .clone();
    let module = package.modules.get_mut(module_name)?;
    if module.origin != crate::package::ModuleOrigin::Input {
        return None;
    }
    let program = module.program.as_mut()?;

    if let Some(function) = program.functions.iter_mut().find(|function| {
        !function.body.is_empty()
            && function.span.start <= range.start
            && range.start < function.span.end
    }) {
        function.body = crate::block::Block::new();
        return Some((module_name.to_owned(), function.span.clone()));
    }
    if let Some(test) = program.tests.iter_mut().find(|test| {
        !test.body.is_empty() && test.span.start <= range.start && range.start < test.span.end
    }) {
        test.body = crate::block::Block::new();
        return Some((module_name.to_owned(), test.span.clone()));
    }
    None
}
