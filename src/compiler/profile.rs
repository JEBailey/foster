//! Opt-in, thread-local compiler telemetry. Reports go to stderr only.
#[cfg(feature = "compiler-profile")]
mod allocations;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::time::Instant;

use crate::error::FosterError;

#[derive(Default, serde::Serialize)]
pub(crate) struct Report {
    total_ms: f64,
    phases: BTreeMap<&'static str, Timing>,
    counters: BTreeMap<&'static str, usize>,
    body_hit_rate: Option<f64>,
    #[cfg(feature = "compiler-profile")]
    allocations: allocations::Allocations,
}

#[derive(Default, serde::Serialize)]
struct Timing {
    calls: usize,
    inclusive_ms: f64,
    #[cfg(feature = "compiler-profile")]
    allocations: allocations::Allocations,
}

thread_local! {
    static ACTIVE: RefCell<Option<Report>> = const { RefCell::new(None) };
}

pub(crate) fn count(name: &'static str) {
    ACTIVE.with(|active| {
        if let Some(report) = active.borrow_mut().as_mut() {
            *report.counters.entry(name).or_default() += 1;
        }
    });
}

pub(crate) fn measure<T>(name: &'static str, work: impl FnOnce() -> T) -> T {
    struct Timer(
        Option<Instant>,
        &'static str,
        #[cfg(feature = "compiler-profile")] allocations::Allocations,
    );
    impl Drop for Timer {
        fn drop(&mut self) {
            if let Some(start) = self.0 {
                #[cfg(feature = "compiler-profile")]
                let allocated = allocations::snapshot().since(self.2);
                ACTIVE.with(|active| {
                    if let Some(report) = active.borrow_mut().as_mut() {
                        let timing = report.phases.entry(self.1).or_default();
                        timing.calls += 1;
                        timing.inclusive_ms += start.elapsed().as_secs_f64() * 1000.0;
                        #[cfg(feature = "compiler-profile")]
                        {
                            timing.allocations.calls += allocated.calls;
                            timing.allocations.bytes += allocated.bytes;
                        }
                    }
                });
            }
        }
    }
    let _timer = Timer(
        ACTIVE.with(|active| active.borrow().as_ref().map(|_| Instant::now())),
        name,
        #[cfg(feature = "compiler-profile")]
        allocations::snapshot(),
    );
    work()
}

fn collect<T>(work: impl FnOnce() -> T) -> (T, Report) {
    struct Restore(Option<Report>);
    impl Drop for Restore {
        fn drop(&mut self) {
            ACTIVE.with(|active| *active.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(ACTIVE.with(|active| active.replace(Some(Report::default()))));
    let start = Instant::now();
    #[cfg(feature = "compiler-profile")]
    let allocated = allocations::snapshot();
    let result = work();
    #[cfg(feature = "compiler-profile")]
    let allocated = allocations::snapshot().since(allocated);
    let mut report = ACTIVE.with(|active| active.borrow_mut().take().unwrap());
    report.total_ms = start.elapsed().as_secs_f64() * 1000.0;
    #[cfg(feature = "compiler-profile")]
    {
        report.allocations = allocated;
    }
    let hits = report.counters.get("body.hit").copied().unwrap_or(0)
        + report.counters.get("body.error_hit").copied().unwrap_or(0);
    let attempts = hits + report.counters.get("body.checked").copied().unwrap_or(0);
    report.body_hit_rate = (attempts != 0).then(|| hits as f64 / attempts as f64);
    (result, report)
}

/// Profile a compiler API call; nested calls contribute to their outer report.
pub(crate) fn compilation<T>(
    operation: &str,
    work: impl FnOnce() -> Result<T, FosterError>,
) -> Result<T, FosterError> {
    if std::env::var("FOSTER_COMPILER_PROFILE").as_deref() != Ok("1")
        || ACTIVE.with(|active| active.borrow().is_some())
    {
        return work();
    }
    let (result, report) = collect(work);
    eprintln!(
        "FOSTER_COMPILER_PROFILE {}",
        serde_json::json!({
            "schema": 1, "operation": operation,
            "allocation_tracking": cfg!(feature = "compiler-profile"),
            "outcome": if result.is_ok() { "ok" } else { "error" },
            "analysis": report,
        })
    );
    result
}

pub(crate) fn request<T>(
    operation: &str,
    document: &str,
    work: impl FnOnce() -> Result<T, FosterError>,
) -> Result<T, FosterError> {
    if std::env::var("FOSTER_LSP_PROFILE").as_deref() != Ok("1")
        || ACTIVE.with(|active| active.borrow().is_some())
    {
        return work();
    }
    let (result, report) = collect(work);
    let outcome = match &result {
        Ok(_) => "ok",
        Err(error) if super::cancellation::is_cancellation(error) => "cancelled",
        Err(_) => "error",
    };
    eprintln!(
        "FOSTER_LSP_PROFILE {}",
        serde_json::json!({
            "schema": 1, "operation": operation, "document": document,
            "allocation_tracking": cfg!(feature = "compiler-profile"),
            "outcome": outcome, "analysis": report,
        })
    );
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_inference_finishes_without_a_second_pipeline_typecheck() {
        let (compilation, report) = collect(|| crate::compile("func main() -> Int { 42 }"));
        compilation.unwrap();
        assert_eq!(report.counters["types.reused_initial"], 1);
        assert!(!report.phases.contains_key("types.final"));

        let (compilation, report) = collect(|| {
            crate::compile(
                "func main() -> Int { let factor = 7\nlet scale = (value: Int) -> value * factor\nscale(6) }",
            )
        });
        let compilation = compilation.unwrap();
        assert!(!report.phases.contains_key("types.final"));
        assert_eq!(report.counters["types.reused_initial"], 1);
        assert_eq!(
            crate::vm::run(&compilation).unwrap(),
            crate::vm::Value::Integer(42)
        );

        let (compilation, report) =
            collect(|| crate::compile(include_str!("../../tests/fixtures/programs/closures.fos")));
        let mut compilation = compilation.unwrap();
        assert!(!report.phases.contains_key("types.final"));
        let effects = compilation
            .hir
            .functions
            .iter()
            .map(|(_, function)| (function.effects.clone(), function.suspends))
            .collect::<Vec<_>>();
        // Rechecking finalized Copy/Move HIR must not discover different contracts.
        crate::typecheck::check(&mut compilation.hir).unwrap();
        assert_eq!(
            effects,
            compilation
                .hir
                .functions
                .iter()
                .map(|(_, function)| (function.effects.clone(), function.suspends))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            crate::vm::run(&compilation).unwrap(),
            crate::vm::Value::Integer(36)
        );
    }

    #[test]
    fn backend_reports_skip_redundant_cleanup_for_a_constant_program() {
        let compilation = crate::compile("func main() -> Int { 42 }").unwrap();
        let (program, report) = collect(|| crate::vm::compile(&compilation));
        crate::vm::verify(&program.unwrap()).unwrap();
        assert_eq!(report.phases["vm.dead_writes"].calls, 1);
        assert_eq!(report.phases["vm.registers"].calls, 1);
        for phase in [
            "shared.construction",
            "shared.seal",
            "shared.optimize",
            "vm.lower",
            "vm.drops",
        ] {
            assert!(report.phases.contains_key(phase), "missing {phase}");
        }
    }

    #[cfg(feature = "compiler-profile")]
    #[test]
    fn allocation_traffic_is_inclusive_and_thread_local() {
        let (_, report) = collect(|| {
            measure("outer", || {
                measure("inner", || std::hint::black_box(vec![42u8; 8192]));
            })
        });
        assert!(report.phases["inner"].allocations.bytes >= 8192);
        assert!(report.phases["inner"].allocations.calls >= 1);
        assert!(
            report.phases["outer"].allocations.bytes >= report.phases["inner"].allocations.bytes
        );
        assert!(report.allocations.bytes >= report.phases["outer"].allocations.bytes);
        std::thread::spawn(|| {
            assert!(ACTIVE.with(|active| active.borrow().is_none()));
            let before = allocations::snapshot();
            std::hint::black_box(vec![0u8; 4096]);
            assert!(allocations::snapshot().since(before).bytes >= 4096);
        })
        .join()
        .unwrap();
        assert!(ACTIVE.with(|active| active.borrow().is_none()));
    }

    #[test]
    fn native_preparation_analyzes_each_body_once_across_specializations() {
        let compilation = crate::compile("func identity<T>(value: T) -> T { value }\nfunc main() -> Int { assert(identity(true))\nassert(identity(1.5) == 1.5)\nidentity(42) }").unwrap();
        let (prepared, report) = collect(|| crate::native::prepare(&compilation));
        let prepared = prepared.unwrap();
        let bodies = prepared
            .functions()
            .iter()
            .map(|function| function.source_function())
            .collect::<std::collections::HashSet<_>>();
        assert!(prepared.functions().len() > bodies.len());
        assert!(!report.counters.contains_key("native.flow_analysis"));
        let shared = crate::vm::compile_shared(&compilation).unwrap();
        assert_eq!(
            report.counters["shared.flow_analysis"],
            shared.functions().len()
        );
    }

    #[test]
    fn compiler_reports_reuse_and_recovery_per_analysis() {
        let source = "func healthy() -> Int { 42 }\nfunc broken() -> Int { false }";
        let mut package =
            crate::package::Package::from_program_with_core("main", crate::parse(source).unwrap())
                .unwrap();
        package.modules.get_mut("main").unwrap().source = Some(source.to_owned());
        let cache = Default::default();
        let (first, first_report) = collect(|| {
            super::super::check_recovering_cached(package.clone(), std::rc::Rc::clone(&cache))
        });
        assert_eq!(first.unwrap().diagnostics.len(), 1);
        assert_eq!(first_report.counters["pipeline.runs"], 2);
        assert!(first_report.counters["body.checked"] > 0);
        assert!(first_report.phases.contains_key("types.checkpoint"));
        let (second, report) = collect(|| super::super::check_recovering_cached(package, cache));
        assert_eq!(second.unwrap().diagnostics.len(), 1);
        assert_eq!(report.counters["body.error_hit"], 1);
        assert!(report.counters["body.hit"] > 0);
        assert!(report.phases.contains_key("ownership.lower"));
    }

    #[test]
    fn measures_failed_phases_and_counts_hits_without_leaking_between_requests() {
        let (result, report) = collect(|| {
            count("body.hit");
            count("body.error_hit");
            count("body.checked");
            count("body.checked");
            measure("outer", || measure("inner", || Err::<(), _>("failed")))
        });
        assert!(result.is_err());
        assert_eq!(report.body_hit_rate, Some(0.5));
        assert_eq!(report.phases["inner"].calls, 1);
        assert!(report.phases["outer"].inclusive_ms >= report.phases["inner"].inclusive_ms);
        let (_, next) = collect(|| count("compilation.hit"));
        assert_eq!(next.body_hit_rate, None);
        assert_eq!(next.counters.len(), 1);
        assert!(ACTIVE.with(|active| active.borrow().is_none()));
    }
}
