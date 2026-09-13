//! Availability, reference assignment, branch evidence, and state joins.
use super::types::{merge_types, require_type};
use super::{Body, StoragePolicy};
use crate::codegen::{ir::Value, types::ExecutableType};
use crate::error::FosterError;
use crate::hir::VariantId;
use std::collections::{HashMap, HashSet};
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FlowState {
    pub(crate) bindings: Vec<Option<ExecutableType>>,
    pub(super) pending_pattern: Option<PendingPattern>,
    pub(super) excluded_variants: HashMap<Value, HashSet<VariantId>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PendingPattern {
    pub(super) conditions: Vec<Value>,
    pub(super) bindings: Vec<Value>,
    pub(super) irrefutable: bool,
    pub(super) covered_variant: Option<(Vec<Value>, VariantId)>,
}

pub(super) fn bound_type(
    function: &Body,
    index: usize,
    state: &FlowState,
    register: Value,
) -> Result<ExecutableType, FosterError> {
    state.bindings[register.index()].clone().ok_or_else(|| {
        FosterError::runtime(format!(
            "bytecode function `{}` instruction {index} reads unavailable r{} in {:?}",
            function.name, register.0, function.instructions[index]
        ))
    })
}

pub(super) fn read_type(
    function: &Body,
    index: usize,
    state: &FlowState,
    register: Value,
) -> Result<ExecutableType, FosterError> {
    readable_type(
        function,
        index,
        bound_type(function, index, state, register)?,
    )
}

pub(super) fn readable_type(
    function: &Body,
    index: usize,
    ty: ExecutableType,
) -> Result<ExecutableType, FosterError> {
    match ty {
        ExecutableType::Reference(value) => Ok(*value),
        ExecutableType::Alternatives(members) => {
            let mut members = members.into_iter();
            let Some(first) = members.next() else {
                return Ok(ExecutableType::Unknown);
            };
            let mut result = readable_type(function, index, first)?;
            for member in members {
                result = merge_types(
                    function,
                    index,
                    &result,
                    &readable_type(function, index, member)?,
                )?;
            }
            Ok(result)
        }
        value => Ok(value),
    }
}

pub(super) fn take_type(
    function: &Body,
    index: usize,
    state: &mut FlowState,
    register: Value,
) -> Result<ExecutableType, FosterError> {
    if function.storage_policy == StoragePolicy::ImmutableValues {
        return bound_type(function, index, state, register);
    }
    state.excluded_variants.remove(&register);
    state.bindings[register.index()].take().ok_or_else(|| {
        FosterError::runtime(format!(
            "bytecode function `{}` instruction {index} consumes unavailable r{} in {:?}",
            function.name, register.0, function.instructions[index]
        ))
    })
}

pub(super) fn assignment_binding<'a>(
    function: &Body,
    state: &'a FlowState,
    destination: Value,
) -> Option<&'a ExecutableType> {
    let previous = if function.storage_policy == StoragePolicy::ImmutableValues {
        *function.write_bindings.get(&destination)?
    } else {
        destination
    };
    state.bindings[previous.index()].as_ref()
}

pub(super) fn write_type(
    function: &Body,
    index: usize,
    state: &mut FlowState,
    register: Value,
    value: ExecutableType,
) -> Result<(), FosterError> {
    state.excluded_variants.remove(&register);
    let previous = assignment_binding(function, state, register).cloned();
    if let Some(ExecutableType::Reference(target)) = previous {
        require_type(function, index, &value, &target, "reference assignment")?;
        state.bindings[register.index()] = Some(ExecutableType::Reference(target));
    } else {
        state.bindings[register.index()] = Some(value);
    }
    Ok(())
}

pub(super) fn merge_state(
    function: &Body,
    index: usize,
    current: &mut FlowState,
    incoming: &FlowState,
) -> Result<bool, FosterError> {
    let mut changed = false;
    for (left, right) in current.bindings.iter_mut().zip(&incoming.bindings) {
        let merged = match (&*left, right) {
            (Some(left_type), Some(right_type)) => {
                // Value coloring can reuse one physical register for unrelated values on
                // disjoint predecessors. An incompatible join becomes unavailable; any later
                // read is then rejected by definite-initialization checking.
                merge_types(function, index, left_type, right_type).ok()
            }
            _ => None,
        };
        if *left != merged {
            *left = merged;
            changed = true;
        }
    }
    let pending = (current.pending_pattern == incoming.pending_pattern)
        .then(|| current.pending_pattern.clone())
        .flatten();
    if current.pending_pattern != pending {
        current.pending_pattern = pending;
        changed = true;
    }
    let keys = current
        .excluded_variants
        .keys()
        .copied()
        .collect::<Vec<_>>();
    for register in keys {
        let Some(incoming) = incoming.excluded_variants.get(&register) else {
            current.excluded_variants.remove(&register);
            changed = true;
            continue;
        };
        let (before, after) = {
            let excluded = current.excluded_variants.get_mut(&register).unwrap();
            let before = excluded.len();
            excluded.retain(|variant| incoming.contains(variant));
            (before, excluded.len())
        };
        if after == 0 {
            current.excluded_variants.remove(&register);
        }
        if after != before {
            changed = true;
        }
    }
    Ok(changed)
}
