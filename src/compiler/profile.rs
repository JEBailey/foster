//! Opt-in, request-local frontend telemetry. Never writes to LSP stdout.
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
}

#[derive(Default, serde::Serialize)]
struct Timing {
    calls: usize,
    inclusive_ms: f64,
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
    struct Timer(Option<Instant>, &'static str);
    impl Drop for Timer {
        fn drop(&mut self) {
            if let Some(start) = self.0 {
                ACTIVE.with(|active| {
                    if let Some(report) = active.borrow_mut().as_mut() {
                        let timing = report.phases.entry(self.1).or_default();
                        timing.calls += 1;
                        timing.inclusive_ms += start.elapsed().as_secs_f64() * 1000.0;
                    }
                });
            }
        }
    }
    let _timer = Timer(
        ACTIVE.with(|active| active.borrow().as_ref().map(|_| Instant::now())),
        name,
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
    let result = work();
    let mut report = ACTIVE.with(|active| active.borrow_mut().take().unwrap());
    report.total_ms = start.elapsed().as_secs_f64() * 1000.0;
    let hits = report.counters.get("body.hit").copied().unwrap_or(0)
        + report.counters.get("body.error_hit").copied().unwrap_or(0);
    let attempts = hits + report.counters.get("body.checked").copied().unwrap_or(0);
    report.body_hit_rate = (attempts != 0).then(|| hits as f64 / attempts as f64);
    (result, report)
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
            "outcome": outcome, "analysis": report,
        })
    );
    result
}

#[cfg(test)]
mod tests {
    use super::*;

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
