use std::collections::{HashSet, VecDeque};

use crate::error::FosterError;
use crate::hir::{LocalId, PackageHir};
use crate::types::TypeInformation;

use super::{Function, Operation, Program, UseMode};

#[derive(Clone, Debug, PartialEq, Eq)]
struct State {
    initialized: HashSet<LocalId>,
    moved: HashSet<crate::hir::Place>,
}

pub(super) fn check(
    hir: &PackageHir,
    types: &TypeInformation,
    program: &Program,
) -> Result<(), FosterError> {
    for (function, mir) in &program.functions {
        check_function(hir, types, *function, mir)?;
    }
    Ok(())
}

fn check_function(
    hir: &PackageHir,
    types: &TypeInformation,
    function: crate::hir::FunctionId,
    mir: &Function,
) -> Result<(), FosterError> {
    let mut entries = vec![None::<State>; mir.blocks.len()];
    entries[mir.entry] = Some(State {
        initialized: HashSet::new(),
        moved: HashSet::new(),
    });
    let mut work = VecDeque::from([mir.entry]);

    while let Some(block_id) = work.pop_front() {
        let Some(mut state) = entries[block_id].clone() else {
            continue;
        };
        for operation in &mir.blocks[block_id].operations {
            match operation {
                Operation::Use { place, mode, .. } => {
                    if !place_is_usable(&state, place) {
                        let local = &hir.locals[place.root];
                        return Err(FosterError::runtime(format!(
                            "in `{}.{}`: value `{}` is used after it was moved or before it was initialized",
                            hir.modules[hir.functions[function].module].name,
                            hir.functions[function].name,
                            local.name
                        )));
                    }
                    if *mode == UseMode::Move {
                        if hir.functions[function].parameters.contains(&place.root)
                            && !parameter_can_be_consumed(hir, types, function, place.root)
                            && types
                                .local_type(place.root)
                                .is_none_or(|ty| !types.is_copy(ty))
                        {
                            let name = &hir.locals[place.root].name;
                            return Err(FosterError::runtime(format!(
                                "in `{}.{}`: borrowed parameter `{name}` is consumed; add `consume {name}` to the function contract",
                                hir.modules[hir.functions[function].module].name,
                                hir.functions[function].name,
                            )));
                        }
                        if place.projections.is_empty() {
                            state.initialized.remove(&place.root);
                            state.moved.retain(|moved| moved.root != place.root);
                        } else {
                            state.moved.insert(place.clone());
                        }
                    }
                }
                Operation::Initialize { local, .. } => {
                    state.initialized.insert(*local);
                    state.moved.retain(|place| place.root != *local);
                }
            }
        }

        for successor in mir.blocks[block_id].terminator.successors() {
            let changed = match &mut entries[*successor] {
                None => {
                    entries[*successor] = Some(state.clone());
                    true
                }
                Some(existing) => {
                    let merged = existing
                        .initialized
                        .intersection(&state.initialized)
                        .copied()
                        .collect::<HashSet<_>>();
                    let moved = existing
                        .moved
                        .union(&state.moved)
                        .cloned()
                        .collect::<HashSet<_>>();
                    if merged == existing.initialized && moved == existing.moved {
                        false
                    } else {
                        existing.initialized = merged;
                        existing.moved = moved;
                        true
                    }
                }
            };
            if changed {
                work.push_back(*successor);
            }
        }
    }
    Ok(())
}

fn parameter_can_be_consumed(
    hir: &PackageHir,
    types: &TypeInformation,
    function: crate::hir::FunctionId,
    parameter: LocalId,
) -> bool {
    let definition = &hir.functions[function];
    let index = definition
        .parameters
        .iter()
        .position(|candidate| *candidate == parameter)
        .expect("parameter belongs to its function");
    let name = &hir.locals[parameter].name;
    if types.function_type(function).is_some_and(|signature| {
        signature.parameter_modes.get(index) == Some(&crate::ast::ParameterMode::Consume)
    }) {
        return true;
    }
    let reference_group = definition.parameter_types[index]
        .as_ref()
        .and_then(|annotation| match annotation {
            crate::ast::TypeExpr::Reference { group, .. } => Some(group.as_str()),
            _ => None,
        });
    definition.effects.iter().any(|effect| {
        effect.kind == crate::ast::EffectKind::Consume
            && (effect.target.root == *name || reference_group == Some(effect.target.root.as_str()))
    })
}

fn place_is_usable(state: &State, place: &crate::hir::Place) -> bool {
    state.initialized.contains(&place.root)
        && !state.moved.iter().any(|moved| places_overlap(moved, place))
}

fn places_overlap(moved: &crate::hir::Place, used: &crate::hir::Place) -> bool {
    if moved.root != used.root {
        return false;
    }
    let shared = moved
        .projections
        .iter()
        .zip(&used.projections)
        .take_while(|(left, right)| left == right)
        .count();
    shared == moved.projections.len() || shared == used.projections.len()
}
