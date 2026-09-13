//! Shared intersection identity and matching for local and portable dispatch signatures.
//!
//! Only intersection children are unordered. Generic renaming spans the entire parameter
//! sequence, including nested callables. Multiplicity is preserved.

use std::collections::BTreeMap;

/// A dispatch-only tree view. Children follow structural comparison order; rebuilding
/// retains constructor identity and parameter modes, but may erase non-dispatch metadata.
pub(crate) trait Tree: Clone + Ord {
    fn generic(&self) -> Option<u32>;
    fn renamed_generic(index: u32) -> Self;
    fn unordered(&self) -> bool;
    fn children(&self) -> Vec<&Self>;
    fn with_children(&self, children: Vec<Self>) -> Self;

    fn same_head(&self, other: &Self) -> bool {
        let mask = |value: &Self| {
            value.with_children(vec![Self::renamed_generic(0); value.children().len()])
        };
        mask(self) == mask(other)
    }
}

type Names = BTreeMap<u32, u32>;

/// Lexicographically minimal intersection ordering and first-use generic numbering.
/// Tied prefixes retain their distinct name maps until later parameters disambiguate them.
pub(crate) fn canonical<T: Tree>(parameters: &[T]) -> Vec<T> {
    fn has_intersection<T: Tree>(value: &T) -> bool {
        value.unordered() || value.children().into_iter().any(has_intersection)
    }
    fn ordered<T: Tree>(value: &T, names: &mut Names) -> T {
        if let Some(index) = value.generic() {
            let next = names.len() as u32;
            return T::renamed_generic(*names.entry(index).or_insert(next));
        }
        value.with_children(
            value
                .children()
                .into_iter()
                .map(|child| ordered(child, names))
                .collect(),
        )
    }
    // Most method signatures need only a linear traversal, with no permutation state.
    if !parameters.iter().any(has_intersection) {
        let mut names = Names::new();
        return parameters
            .iter()
            .map(|value| ordered(value, &mut names))
            .collect();
    }
    sequence(parameters.iter().collect(), false, Names::new())
        .into_iter()
        .next()
        .expect("canonicalization always has a candidate")
        .0
}

fn node<T: Tree>(value: &T, mut names: Names) -> Vec<(T, Names)> {
    if let Some(index) = value.generic() {
        let next = names.len() as u32;
        let index = *names.entry(index).or_insert(next);
        return vec![(T::renamed_generic(index), names)];
    }
    sequence(value.children(), value.unordered(), names)
        .into_iter()
        .map(|(children, names)| (value.with_children(children), names))
        .collect()
}

// Discard larger prefixes immediately. Distinct nominal heads avoid factorial enumeration;
// symmetric generic members can still require multiple branches to preserve later constraints.
fn sequence<T: Tree>(values: Vec<&T>, unordered: bool, names: Names) -> Vec<(Vec<T>, Names)> {
    let count = values.len();
    let mut states = vec![(Vec::<T>::new(), values, names)];
    for _ in 0..count {
        let mut next = Vec::new();
        let mut best: Option<Vec<T>> = None;
        for (prefix, remaining, names) in states {
            let choices = if unordered { remaining.len() } else { 1 };
            for index in 0..choices {
                // Identical members need not generate identical permutation branches.
                if unordered && remaining[..index].contains(&remaining[index]) {
                    continue;
                }
                for (value, names) in node(remaining[index], names.clone()) {
                    let mut prefix = prefix.clone();
                    prefix.push(value);
                    match best.as_ref().map(|best| prefix.cmp(best)) {
                        Some(std::cmp::Ordering::Greater) => continue,
                        None | Some(std::cmp::Ordering::Less) => {
                            best = Some(prefix.clone());
                            next.clear();
                        }
                        Some(std::cmp::Ordering::Equal) => {}
                    }
                    let mut rest = remaining.clone();
                    rest.remove(index);
                    let candidate = (prefix, rest, names);
                    if !next.contains(&candidate) {
                        next.push(candidate);
                    }
                }
            }
        }
        states = next;
    }
    states
        .into_iter()
        .map(|(values, _, names)| (values, names))
        .collect()
}

#[derive(Clone)]
enum Task<'a, T> {
    Pair(&'a T, &'a T),
    Intersection(Vec<&'a T>, Vec<&'a T>),
}

/// Match a generic implementation against a required signature. The continuation includes
/// later parameters, so an intersection pairing is committed only when the whole match succeeds.
pub(crate) fn matches<T: Tree>(pattern: &[T], actual: &[T]) -> bool {
    pattern.len() == actual.len()
        && solve(
            pattern
                .iter()
                .zip(actual)
                .rev()
                .map(|(a, b)| Task::Pair(a, b))
                .collect(),
            BTreeMap::new(),
            true,
        )
}

fn solve<'a, T: Tree>(
    mut tasks: Vec<Task<'a, T>>,
    mut bindings: BTreeMap<u32, &'a T>,
    bind_generics: bool,
) -> bool {
    while let Some(task) = tasks.pop() {
        match task {
            Task::Pair(pattern, actual) => {
                if let Some(index) = pattern.generic() {
                    if !bind_generics {
                        if pattern != actual {
                            return false;
                        }
                    } else if let Some(bound) = bindings.get(&index) {
                        // Do not alpha-rename actual generic identities independently here.
                        if !solve(vec![Task::Pair(*bound, actual)], BTreeMap::new(), false) {
                            return false;
                        }
                    } else {
                        bindings.insert(index, actual);
                    }
                    continue;
                }
                if !pattern.same_head(actual) {
                    return false;
                }
                let left = pattern.children();
                let right = actual.children();
                if pattern.unordered() {
                    tasks.push(Task::Intersection(left, right));
                } else {
                    tasks.extend(
                        left.into_iter()
                            .zip(right)
                            .rev()
                            .map(|(a, b)| Task::Pair(a, b)),
                    );
                }
            }
            Task::Intersection(left, right) => {
                let Some((first, rest)) = left.split_first() else {
                    continue;
                };
                for (index, candidate) in right.iter().enumerate() {
                    if right[..index].contains(candidate) {
                        continue;
                    }
                    let mut remaining = right.clone();
                    remaining.remove(index);
                    let mut branch = tasks.clone();
                    branch.push(Task::Intersection(rest.to_vec(), remaining));
                    branch.push(Task::Pair(first, candidate));
                    if solve(branch, bindings.clone(), bind_generics) {
                        return true;
                    }
                }
                return false;
            }
        }
    }
    true
}
