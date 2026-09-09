//! A scoped cancellation probe for synchronous frontend work on the LSP worker.
//! Strict command-line compilation has no probe and follows the same checking path.
use std::cell::RefCell;

use crate::error::FosterError;

type Probe = Box<dyn Fn() -> bool>;
thread_local! {
    static PROBE: RefCell<Option<Probe>> = RefCell::new(None);
}

pub(crate) fn is_cancelled() -> bool {
    PROBE.with(|probe| probe.borrow().as_ref().is_some_and(|probe| probe()))
}

pub(crate) fn check() -> Result<(), FosterError> {
    if is_cancelled() {
        Err(FosterError::runtime("analysis cancelled").with_code("analysis-cancelled"))
    } else {
        Ok(())
    }
}

pub(crate) fn is_cancellation(error: &FosterError) -> bool {
    error.code.as_deref() == Some("analysis-cancelled")
}

pub(crate) fn scope<T>(probe: impl Fn() -> bool + 'static, work: impl FnOnce() -> T) -> T {
    struct Restore(Option<Probe>);
    impl Drop for Restore {
        fn drop(&mut self) {
            PROBE.with(|probe| *probe.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(PROBE.with(|slot| slot.replace(Some(Box::new(probe)))));
    work()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_body_analysis_can_resume_with_the_same_cache() {
        let source = (0..30)
            .map(|i| format!("func value{i}() -> Int {{ {i} }}\n"))
            .collect::<String>();
        let mut package =
            crate::package::Package::from_program_with_core("main", crate::parse(&source).unwrap())
                .unwrap();
        package.modules.get_mut("main").unwrap().source = Some(source);
        let cache = Default::default();
        let polls = std::rc::Rc::new(std::cell::Cell::new(0));
        let probe_polls = polls.clone();
        let error = scope(
            move || {
                probe_polls.set(probe_polls.get() + 1);
                probe_polls.get() > 40
            },
            || super::super::check_recovering_cached(package.clone(), std::rc::Rc::clone(&cache)),
        )
        .unwrap_err();
        assert!(is_cancellation(&error));
        assert!(cache.borrow().stats.checked.values().sum::<usize>() > 0);
        let resumed = super::super::check_recovering_cached(package.clone(), cache).unwrap();
        assert_eq!(
            resumed.diagnostics,
            super::super::check(package).unwrap().diagnostics
        );
    }

    #[test]
    fn cancellation_is_scoped_and_never_becomes_a_body_diagnostic() {
        scope(
            || true,
            || {
                let package = crate::package::Package::from_program_with_core(
                    "main",
                    crate::parse("func main() -> Int { 1 }").unwrap(),
                )
                .unwrap();
                let error = super::super::check_recovering(package).unwrap_err();
                assert!(is_cancellation(&error));
            },
        );
        assert!(check().is_ok());
    }
}
