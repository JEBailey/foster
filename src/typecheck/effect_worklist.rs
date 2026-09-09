//! Propagate effects over observed direct-call edges before repeating type inference.
use std::collections::VecDeque;

use super::*;

#[derive(Clone, Debug)]
pub(super) struct EffectSummary {
    pub(super) row: (Vec<crate::ast::Effect>, bool),
    pub(super) dependencies: HashSet<FunctionId>,
}

impl Checker<'_> {
    pub(super) fn derive_effects(&mut self) -> Result<(), FosterError> {
        #[cfg(test)]
        if tests::REFERENCE.with(|reference| reference.get()) {
            for (function, definition) in self.hir.functions.iter() {
                if definition.intrinsic.is_some() {
                    continue;
                }
                let mut derivation = EffectDerivation::new(self, function);
                derivation.walk_statements(&definition.body);
                let row = (derivation.effects(), derivation.suspends);
                let dependencies = std::mem::take(&mut derivation.dependencies);
                drop(derivation);
                self.derived_effects.insert(function, row);
                self.effect_dependencies.insert(function, dependencies);
            }
            return Ok(());
        }
        let mut summaries = DerivedEffects::new();
        let mut dependencies = HashMap::<FunctionId, HashSet<FunctionId>>::new();
        let mut callers = HashMap::<FunctionId, HashSet<FunctionId>>::new();
        let mut cached = HashSet::new();
        let mut pending = HashSet::new();
        let mut processed = HashSet::new();
        let mut queue = VecDeque::new();

        for (id, definition) in self.hir.functions.iter() {
            if definition.intrinsic.is_some() {
                continue;
            }
            if let Some(seed) = self.effect_seeds.get(&id) {
                summaries.insert(id, seed.row.clone());
                dependencies.insert(id, seed.dependencies.clone());
                for target in &seed.dependencies {
                    callers.entry(*target).or_default().insert(id);
                }
                cached.insert(id);
            } else {
                // Dirty recursive components have no old summary to sustain a removed effect.
                // Explicit callees still expose their declared bounds, never these empty rows.
                summaries.insert(id, (Vec::new(), false));
                pending.insert(id);
                queue.push_back(id);
            }
        }

        while let Some(function) = queue.pop_front() {
            crate::compiler::cancellation::check()?;
            pending.remove(&function);
            let first = processed.insert(function);
            cached.remove(&function);
            crate::compiler::profile::count("effects.derived");
            if let Some(cache) = &self.body_cache {
                cache.borrow_mut().note_effect(function, false);
            }
            let definition = &self.hir.functions[function];
            let mut derivation = EffectDerivation::with_summaries(self, function, &summaries);
            derivation.walk_statements(&definition.body);
            let row = (derivation.effects(), derivation.suspends);
            let targets = std::mem::take(&mut derivation.dependencies);
            drop(derivation);
            if let Some(previous) = dependencies.insert(function, targets.clone()) {
                for target in previous {
                    callers.get_mut(&target).unwrap().remove(&function);
                }
            }
            for target in targets {
                callers.entry(target).or_default().insert(function);
            }
            let changed = summaries[&function] != row;
            summaries.insert(function, row);
            // A first derivation can remove the old published row while remaining equal to
            // the temporary empty row. Cached consumers must hear about that removal too.
            let published_changed =
                summaries[&function] != (definition.effects.clone(), definition.suspends);
            if !definition.effects_explicit && (changed || (first && published_changed)) {
                crate::compiler::profile::count("effects.summary_changed");
                let mut affected = callers
                    .get(&function)
                    .into_iter()
                    .flatten()
                    .copied()
                    .collect::<Vec<_>>();
                affected.sort();
                for caller in affected {
                    // A cached body observed the incoming HIR contract, not the temporary empty
                    // row used to recompute a dirty callee. Preserve it if that contract held.
                    if cached.contains(&caller)
                        && summaries[&function] == (definition.effects.clone(), definition.suspends)
                    {
                        continue;
                    }
                    if pending.insert(caller) {
                        crate::compiler::profile::count("effects.caller_enqueued");
                        queue.push_back(caller);
                    }
                }
            }
        }
        for function in cached {
            crate::compiler::profile::count("effects.reused");
            if let Some(cache) = &self.body_cache {
                cache.borrow_mut().note_effect(function, true);
            }
        }
        self.effect_dependencies = dependencies;
        self.derived_effects = summaries;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    thread_local! { pub(super) static REFERENCE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }

    fn reference<T>(work: impl FnOnce() -> T) -> T {
        struct Restore;
        impl Drop for Restore {
            fn drop(&mut self) {
                REFERENCE.with(|value| value.set(false));
            }
        }
        REFERENCE.with(|value| value.set(true));
        let _restore = Restore;
        work()
    }

    fn package(source: &str) -> crate::package::Package {
        let mut package =
            crate::package::Package::from_program_with_core("main", crate::parse(source).unwrap())
                .unwrap();
        package.modules.get_mut("main").unwrap().source = Some(source.to_owned());
        package
    }

    fn compare(source: &str, cache: incremental::SharedBodyCache) {
        let incremental = crate::compiler::check_recovering_cached(package(source), cache).unwrap();
        let fresh = reference(|| crate::compiler::check_recovering(package(source))).unwrap();
        assert_eq!(incremental.diagnostics, fresh.diagnostics);
        for (id, function) in incremental.hir.functions.iter() {
            let mut actual = function
                .effects
                .iter()
                .map(|effect| format!("{effect:?}"))
                .collect::<Vec<_>>();
            let mut expected = fresh.hir.functions[id]
                .effects
                .iter()
                .map(|effect| format!("{effect:?}"))
                .collect::<Vec<_>>();
            actual.sort();
            expected.sort();
            assert_eq!(actual, expected, "{}", function.name);
            assert_eq!(
                function.suspends, fresh.hir.functions[id].suspends,
                "{}",
                function.name
            );
            assert_eq!(
                incremental.types.function_type(id).unwrap().parameter_modes,
                fresh.types.function_type(id).unwrap().parameter_modes
            );
        }
    }

    #[test]
    fn unchanged_contract_does_not_rederive_cached_callers() {
        let source = "func leaf() -> Int { 1 }\nfunc caller() -> Int { leaf() }\nfunc unrelated() -> Int { 2 }";
        let cache: incremental::SharedBodyCache = Default::default();
        compare(source, cache.clone());
        let before = cache.borrow().stats.clone();
        compare(&source.replace("{ 1 }", "{ 3 }"), cache.clone());
        let after = &cache.borrow().stats;
        for key in ["main::caller#0", "main::unrelated#0"] {
            assert_eq!(
                before.effect_derived.get(key),
                after.effect_derived.get(key)
            );
            assert!(after.effect_reused.get(key) > before.effect_reused.get(key));
        }
    }

    #[test]
    fn recursive_effects_grow_shrink_and_report_bounds_like_synchronous_inference() {
        let base = r#"
func first[g: group Int](value: ref[g] Int, stop: Bool) -> Int {
    return value if stop
    second(ref value, true)
}
func second[g: group Int](value: ref[g] Int, stop: Bool) -> Int {
    BODY
    first(ref value, stop)
}
func caller[g: group Int](value: ref[g] Int) -> Int [read g] { first(ref value, true) }
"#;
        let cache = Default::default();
        for body in ["()", "value = value + 1", "()"] {
            compare(&base.replace("BODY", body), std::rc::Rc::clone(&cache));
        }
    }

    #[test]
    fn captured_callable_effects_match_synchronous_inference() {
        let base = r#"
func make[g: group Int](value: ref[g] Int) -> func() -> Int [mut g] {
    [ref value] () -> [mut g] {
        BODY
        value
    }
}
func main() -> Int {
    let count = 0
    let callback = make(ref count)
    callback()
}
"#;
        let cache = Default::default();
        for body in ["()", "value = value + 1", "()"] {
            compare(&base.replace("BODY", body), std::rc::Rc::clone(&cache));
        }
    }
}
