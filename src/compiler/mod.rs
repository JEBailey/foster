//! Foster's checked-compilation facade.
//!
//! This module owns orchestration across the compiler phases. Individual representations such as
//! AST and HIR do not initiate later phases; callers enter the checked pipeline here.

pub(crate) mod cancellation;
mod pipeline;
pub(crate) mod profile;

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
/// A failed body is replaced with a recovery stub while its declaration and signature remain in the
/// package. Independent body errors are collected in one pass before restarting, so unrelated functions receive type information from
/// the current source rather than from the language server's last-good snapshot. Strict compiler
/// entry points continue to reject the original program.
#[cfg(test)]
pub(crate) fn check_recovering(package: Package) -> Result<Compilation, FosterError> {
    check_recovering_cached(package, Default::default())
}

pub(crate) fn check_recovering_cached(
    mut package: Package,
    cache: crate::typecheck::incremental::SharedBodyCache,
) -> Result<Compilation, FosterError> {
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
        cancellation::check()?;
        match pipeline::check_collecting(package.clone(), Some(cache.clone())) {
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
            Err(errors) => {
                for error in errors {
                    let error = package.locate_compiler_error(FosterError::from(error));
                    if cancellation::is_cancellation(&error) {
                        return Err(error);
                    }
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
        !function.body_is_recovery_stub
            && function.span.start <= range.start
            && range.start < function.span.end
    }) {
        function.body = crate::block::Block::new();
        function.body_is_recovery_stub = true;
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

#[cfg(test)]
mod recovery_tests {
    use super::*;

    fn package(source: &str) -> Package {
        let mut package =
            Package::from_program_with_core("main", crate::parse(source).unwrap()).unwrap();
        package.modules.get_mut("main").unwrap().source = Some(source.to_owned());
        package
    }

    #[test]
    fn cached_failures_move_with_source_and_clear_when_repaired() {
        let cache = Default::default();
        let source = "func broken() -> Int { false }\nfunc healthy() -> Int { 1 }";
        check_recovering_cached(package(source), std::rc::Rc::clone(&cache)).unwrap();
        let before = cache.borrow().stats.clone();
        let shifted = format!("// inserted line\n{source}");
        let incremental = check_recovering_cached(package(&shifted), cache.clone()).unwrap();
        assert_eq!(
            incremental.diagnostics,
            check_recovering(package(&shifted)).unwrap().diagnostics
        );
        assert_eq!(
            cache.borrow().stats.checked.get("main::broken#0"),
            before.checked.get("main::broken#0")
        );
        let repaired = shifted.replace("false", "42");
        assert!(
            check_recovering_cached(package(&repaired), cache)
                .unwrap()
                .diagnostics
                .is_empty()
        );
    }

    #[test]
    fn cached_effects_can_grow_and_shrink_through_callers() {
        let cache = Default::default();
        let base = "import core.result as outcomes\ntype Worker = {}\nimpl Worker { func value(self: Worker) -> Int { 1 } }\nfunc wait(worker: Remote<Worker>) -> Int { BODY }\nfunc caller(worker: Remote<Worker>) -> Int { wait(worker) }\nfunc unrelated() -> Int { 3 }";
        for body in ["1", "(await worker.value()).unwrap_or(0)", "1"] {
            let source = base.replace("BODY", body);
            let incremental =
                check_recovering_cached(package(&source), std::rc::Rc::clone(&cache)).unwrap();
            let fresh = check(package(&source)).unwrap();
            assert_eq!(incremental.diagnostics, fresh.diagnostics);
            for (id, function) in incremental.hir.functions.iter() {
                assert_eq!(
                    function.effects, fresh.hir.functions[id].effects,
                    "{}",
                    function.name
                );
                assert_eq!(
                    function.suspends, fresh.hir.functions[id].suspends,
                    "{}",
                    function.name
                );
            }
        }
    }

    #[test]
    fn body_cache_reuses_unaffected_functions_after_arena_offsets_change() {
        let cache: crate::typecheck::incremental::SharedBodyCache = Default::default();
        let source = "func leaf() -> Int { 1 }\nfunc caller() -> Int { leaf() }\nfunc unrelated() -> Int { 3 }";
        check_recovering_cached(package(source), cache.clone()).unwrap();
        let before = cache.borrow().stats.clone();
        let edited = "// move all spans\nfunc leaf() -> Int {\nlet extra = 2\nextra + 1\n}\nfunc caller() -> Int { leaf() }\nfunc unrelated() -> Int { 3 }";
        let result = check_recovering_cached(package(edited), cache.clone()).unwrap();
        assert_eq!(
            result.diagnostics,
            check(package(edited)).unwrap().diagnostics
        );
        let after = &cache.borrow().stats;
        for name in ["main::caller#0", "main::unrelated#0"] {
            assert_eq!(
                before.checked.get(name),
                after.checked.get(name),
                "{name}: {after:?}"
            );
            assert!(after.reused.get(name) > before.reused.get(name));
        }
        assert!(after.checked["main::leaf#0"] > before.checked["main::leaf#0"]);
    }

    #[test]
    fn body_cache_rechecks_callers_after_signature_changes() {
        let cache = Default::default();
        check_recovering_cached(
            package("func leaf() -> Int { 1 }\nfunc caller() -> Int { leaf() }"),
            std::rc::Rc::clone(&cache),
        )
        .unwrap();
        let edited = "func leaf() -> Bool { true }\nfunc caller() -> Int { leaf() }";
        let result = check_recovering_cached(package(edited), cache).unwrap();
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(
            result.diagnostics,
            check_recovering(package(edited)).unwrap().diagnostics
        );
    }

    #[test]
    fn independent_type_errors_are_collected_in_one_pipeline_pass() {
        let mut source = String::from("func healthy() -> Int { 42 }\n");
        for index in 0..20 {
            source.push_str(&format!("func bad{index}() -> Int {{ false }}\n"));
        }
        let errors = pipeline::check_collecting(package(&source), None).unwrap_err();
        assert_eq!(errors.len(), 20);
        let recovered = check_recovering(package(&source)).unwrap();
        assert_eq!(recovered.diagnostics.len(), 20);
        assert!(check(package(&source)).is_err());
    }

    #[test]
    fn independent_ownership_errors_are_collected_in_one_pipeline_pass() {
        let source = "type Item = {}\nfunc first() -> Item {\nlet x = Item {}\nlet y = move x\nx\n}\nfunc second() -> Item {\nlet x = Item {}\nlet y = move x\nx\n}\nfunc healthy() -> Int { 42 }";
        assert_eq!(
            pipeline::check_collecting(package(source), None)
                .unwrap_err()
                .len(),
            2
        );
        assert_eq!(
            check_recovering(package(source)).unwrap().diagnostics.len(),
            2
        );
    }
}
