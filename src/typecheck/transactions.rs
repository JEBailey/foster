//! Body-local undo logs. Mutable access is explicit so writes cannot bypass rollback.
use std::collections::{HashMap, HashSet, hash_map, hash_set};
use std::hash::Hash;
use std::ops::Deref;

use super::*;

#[derive(Clone)]
pub(super) struct JournalMap<K, V> {
    values: HashMap<K, V>,
    undo: Vec<(K, Option<V>)>,
    active: bool,
}

impl<K, V> Default for JournalMap<K, V> {
    fn default() -> Self {
        Self {
            values: HashMap::new(),
            undo: Vec::new(),
            active: false,
        }
    }
}

impl<K, V> Deref for JournalMap<K, V> {
    type Target = HashMap<K, V>;
    fn deref(&self) -> &Self::Target {
        &self.values
    }
}

impl<'a, K, V> IntoIterator for &'a JournalMap<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = hash_map::Iter<'a, K, V>;
    fn into_iter(self) -> Self::IntoIter {
        self.values.iter()
    }
}

impl<K: Clone + Eq + Hash, V: Clone> JournalMap<K, V> {
    pub(super) fn insert(&mut self, key: K, value: V) -> Option<V> {
        if !self.active {
            return self.values.insert(key, value);
        }
        let previous = self.values.insert(key.clone(), value);
        self.undo.push((key, previous.clone()));
        previous
    }

    pub(super) fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        if self.active
            && let Some(value) = self.values.get(key)
        {
            self.undo.push((key.clone(), Some(value.clone())));
        }
        self.values.get_mut(key)
    }

    pub(super) fn entry(&mut self, key: K) -> hash_map::Entry<'_, K, V> {
        if self.active {
            self.undo
                .push((key.clone(), self.values.get(&key).cloned()));
        }
        self.values.entry(key)
    }

    fn begin(&mut self) {
        assert!(!self.active, "nested body transaction");
        self.active = true;
    }

    fn commit(&mut self) {
        self.undo.clear();
        self.active = false;
    }

    fn rollback(&mut self) {
        for (key, value) in self.undo.drain(..).rev() {
            if let Some(value) = value {
                self.values.insert(key, value);
            } else {
                self.values.remove(&key);
            }
        }
        self.active = false;
    }
}

#[derive(Clone)]
pub(super) struct JournalSet<K> {
    values: HashSet<K>,
    undo: Vec<K>,
    active: bool,
}

impl<K> Default for JournalSet<K> {
    fn default() -> Self {
        Self {
            values: HashSet::new(),
            undo: Vec::new(),
            active: false,
        }
    }
}

impl<K> Deref for JournalSet<K> {
    type Target = HashSet<K>;
    fn deref(&self) -> &Self::Target {
        &self.values
    }
}

impl<K> IntoIterator for JournalSet<K> {
    type Item = K;
    type IntoIter = hash_set::IntoIter<K>;
    fn into_iter(self) -> Self::IntoIter {
        self.values.into_iter()
    }
}

impl<K: Clone + Eq + Hash> JournalSet<K> {
    pub(super) fn insert(&mut self, key: K) -> bool {
        if !self.active {
            return self.values.insert(key);
        }
        if self.values.insert(key.clone()) {
            self.undo.push(key);
            true
        } else {
            false
        }
    }

    fn begin(&mut self) {
        assert!(!self.active, "nested body transaction");
        self.active = true;
    }

    fn commit(&mut self) {
        self.undo.clear();
        self.active = false;
    }

    fn rollback(&mut self) {
        for key in self.undo.drain(..) {
            self.values.remove(&key);
        }
        self.active = false;
    }
}

pub(super) struct BodyCheckpoint {
    substitutions: substitutions::Substitutions,
    next_variable: u32,
    body_cacheable: bool,
    dispatch_keys: usize,
    member_constraints: usize,
    diagnostics: usize,
    resolving_aliases: Vec<hir::VariantTypeId>,
}

// Keep the transactional fields together for begin/commit/rollback. HIR and the
// body cache are shared; derived_effects/dependencies are produced after all bodies finish.
// Effect seeds are installed only by top-level body-cache replay, outside transactions.
macro_rules! journals {
    ($checker:expr, $operation:ident) => {
        $checker.record_fields_cache.$operation();
        $checker.record_methods_cache.$operation();
        $checker.checked_requirements.$operation();
        $checker.functions.$operation();
        $checker.constants.$operation();
        $checker.locals.$operation();
        $checker.local_groups.$operation();
        $checker.expressions.$operation();
        $checker.integer_promotions.$operation();
        $checker.member_kinds.$operation();
        $checker.bare_method_members.$operation();
        $checker.resolved_calls.$operation();
        $checker.dispatch_slots.$operation();
    };
}

impl Checker<'_> {
    pub(super) fn begin_body(&mut self) -> BodyCheckpoint {
        journals!(self, begin);
        BodyCheckpoint {
            // Substitution pages already use copy-on-write, including speculative overloads.
            substitutions: self.substitutions.clone(),
            next_variable: self.next_variable,
            body_cacheable: self.body_cacheable,
            dispatch_keys: self.dispatch_keys.len(),
            member_constraints: self.member_constraints.len(),
            diagnostics: self.diagnostics.len(),
            resolving_aliases: self.resolving_aliases.clone(),
        }
    }

    pub(super) fn commit_body(&mut self, _checkpoint: BodyCheckpoint) {
        journals!(self, commit);
        crate::compiler::profile::count("body.transaction_commit");
    }

    pub(super) fn rollback_body(&mut self, checkpoint: BodyCheckpoint) {
        journals!(self, rollback);
        self.substitutions = checkpoint.substitutions;
        self.next_variable = checkpoint.next_variable;
        self.body_cacheable = checkpoint.body_cacheable;
        self.dispatch_keys.truncate(checkpoint.dispatch_keys);
        self.member_constraints
            .truncate(checkpoint.member_constraints);
        self.diagnostics.truncate(checkpoint.diagnostics);
        self.resolving_aliases = checkpoint.resolving_aliases;
        crate::compiler::profile::count("body.transaction_rollback");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journals_match_independent_map_and_set_snapshots() {
        let mut map = JournalMap::<usize, Vec<usize>>::default();
        let mut set = JournalSet::default();
        let mut expected = HashMap::new();
        let mut expected_set = HashSet::new();
        for round in 0..80 {
            let before = expected.clone();
            let before_set = expected_set.clone();
            map.begin();
            set.begin();
            for step in 0..30 {
                let key = (round * 7 + step * 13) % 37;
                match step % 3 {
                    0 => {
                        map.insert(key, vec![round]);
                        expected.insert(key, vec![round]);
                    }
                    1 => {
                        map.entry(key).or_default().push(step);
                        expected.entry(key).or_default().push(step);
                    }
                    _ => {
                        if let Some(value) = map.get_mut(&key) {
                            value.push(step);
                        }
                        if let Some(value) = expected.get_mut(&key) {
                            value.push(step);
                        }
                    }
                }
                assert_eq!(set.insert(key), expected_set.insert(key));
                assert_eq!(*map, expected);
                assert_eq!(*set, expected_set);
            }
            if round % 3 == 0 {
                map.commit();
                set.commit();
            } else {
                map.rollback();
                set.rollback();
                expected = before;
                expected_set = before_set;
            }
            assert_eq!(*map, expected);
            assert_eq!(*set, expected_set);
            assert!(map.undo.is_empty() && set.undo.is_empty());
        }
    }

    fn assert_map<K: Eq + Hash + std::fmt::Debug, V: std::fmt::Debug>(
        left: &HashMap<K, V>,
        right: &HashMap<K, V>,
    ) {
        assert_eq!(left.len(), right.len());
        for (key, value) in left {
            assert_eq!(
                format!("{value:?}"),
                format!("{:?}", right.get(key).unwrap()),
                "{key:?}"
            );
        }
    }

    fn assert_checker(left: &Checker<'_>, right: &Checker<'_>, variables: u32) {
        assert_map(&left.record_fields_cache, &right.record_fields_cache);
        assert_map(&left.record_methods_cache, &right.record_methods_cache);
        assert_eq!(*left.checked_requirements, *right.checked_requirements);
        assert_eq!(*left.functions, *right.functions);
        assert_eq!(*left.constants, *right.constants);
        assert_eq!(*left.locals, *right.locals);
        assert_eq!(*left.local_groups, *right.local_groups);
        assert_eq!(*left.expressions, *right.expressions);
        assert_eq!(*left.integer_promotions, *right.integer_promotions);
        assert_eq!(*left.member_kinds, *right.member_kinds);
        assert_eq!(*left.bare_method_members, *right.bare_method_members);
        assert_eq!(*left.resolved_calls, *right.resolved_calls);
        assert_eq!(*left.dispatch_slots, *right.dispatch_slots);
        assert_eq!(left.dispatch_keys, right.dispatch_keys);
        assert_eq!(
            format!("{:?}", left.member_constraints),
            format!("{:?}", right.member_constraints)
        );
        assert_eq!(left.diagnostics, right.diagnostics);
        assert_eq!(left.derived_effects, right.derived_effects);
        assert_eq!(left.resolving_aliases, right.resolving_aliases);
        assert_eq!(left.next_variable, right.next_variable);
        assert_eq!(left.body_cacheable, right.body_cacheable);
        for variable in 0..variables {
            assert_eq!(
                left.substitutions.get(&variable),
                right.substitutions.get(&variable)
            );
        }
    }

    #[test]
    fn failed_bodies_restore_the_same_checker_as_full_cloning() {
        let source = r#"
func choose(value: Int) -> Int { value }
func choose(value: Bool) -> Bool { value }
func healthy() -> Int { choose(1) }
func bad_overload() -> Int {
    let selected = choose(true)
    selected
}
func bad_capture() -> Int {
    let count = 0
    let increment = [ref count] () -> {
        count = count + 1
        count
    }
    increment()
    false
}
func after() -> Int { choose(2) }
"#;
        let package =
            crate::package::Package::from_program_with_core("main", crate::parse(source).unwrap())
                .unwrap();
        let hir = hir::PackageHir::lower(&package).unwrap();
        let mut checker = Checker::new(&hir);
        checker.prepare().unwrap();
        let mut reference = checker.clone();
        let main = hir.module_named("main").unwrap();
        let mut failures = 0;
        for (function, _) in hir
            .functions
            .iter()
            .filter(|(_, f)| f.module == main && !f.name.contains('$'))
        {
            checker.body_cacheable = true;
            reference.body_cacheable = true;
            let saved = reference.clone();
            let checkpoint = checker.begin_body();
            let result = checker.check_function(function);
            let expected = reference.check_function(function);
            assert_eq!(result, expected);
            let variables = checker.next_variable.max(reference.next_variable) + 1;
            if result.is_err() {
                failures += 1;
                checker.rollback_body(checkpoint);
                reference = saved;
            } else {
                checker.commit_body(checkpoint);
            }
            assert_checker(&checker, &reference, variables);
        }
        assert_eq!(failures, 2);
    }
}
