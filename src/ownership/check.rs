use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;

use crate::error::FosterError;
use crate::hir::{LocalId, PackageHir};
use crate::types::TypeInformation;

use super::{Function, Operation, Program, UseMode};

#[derive(Clone, Debug, PartialEq, Eq)]
struct State {
    initialized: HashSet<LocalId>,
    moved: HashMap<crate::hir::Place, Range<usize>>,
    last_move: HashMap<LocalId, Range<usize>>,
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
        moved: HashMap::new(),
        last_move: HashMap::new(),
    });
    let mut work = VecDeque::from([mir.entry]);

    while let Some(block_id) = work.pop_front() {
        let Some(mut state) = entries[block_id].clone() else {
            continue;
        };
        for operation in &mir.blocks[block_id].operations {
            match operation {
                Operation::Use { place, mode, span } => {
                    if !place_is_usable(&state, place) {
                        let local = &hir.locals[place.root];
                        let definition = &hir.functions[function];
                        let mut error = FosterError::runtime(format!(
                            "in `{}.{}`: value `{}` is used after it was moved or before it was initialized",
                            hir.modules[definition.module].name,
                            definition.name,
                            local.name
                        ))
                        .with_code("E0382")
                        .with_source_module(hir.modules[definition.module].name.clone())
                        .with_primary_label(span.clone(), format!("`{}` is not usable here", local.name))
                        .with_label(local.span.clone(), "value is declared here");
                        if let Some(moved_at) = move_origin(&state, place) {
                            error = error
                                .with_label(moved_at, "ownership was moved from this place")
                                .with_help(format!(
                                    "borrow `{}` instead, or move it only after its final use",
                                    local.name
                                ));
                        } else {
                            error = error.with_help(format!(
                                "initialize `{}` on every control-flow path before using it",
                                local.name
                            ));
                        }
                        return Err(error);
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
                            ))
                            .with_code("E0507")
                            .with_source_module(hir.modules[hir.functions[function].module].name.clone())
                            .with_primary_label(span.clone(), format!("ownership of borrowed parameter `{name}` is taken here"))
                            .with_label(hir.locals[place.root].span.clone(), "this parameter borrows by default")
                            .with_help(format!("add `consume {name}` to the function contract and pass existing values with `move`")));
                        }
                        if place.projections.is_empty() {
                            state.initialized.remove(&place.root);
                            state.moved.retain(|moved, _| moved.root != place.root);
                            state.last_move.insert(place.root, span.clone());
                        } else {
                            state.moved.insert(place.clone(), span.clone());
                        }
                    }
                }
                Operation::Initialize { local, .. } => {
                    state.initialized.insert(*local);
                    state.moved.retain(|place, _| place.root != *local);
                    state.last_move.remove(local);
                }
                Operation::StoreBorrower { .. }
                | Operation::ReturnBorrower { .. }
                | Operation::Invalidate { .. }
                | Operation::Suspend { .. } => {}
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
                    let mut moved = existing.moved.clone();
                    for (place, span) in &state.moved {
                        moved.entry(place.clone()).or_insert_with(|| span.clone());
                    }
                    let mut last_move = existing.last_move.clone();
                    for (local, span) in &state.last_move {
                        last_move.entry(*local).or_insert_with(|| span.clone());
                    }
                    if merged == existing.initialized
                        && moved == existing.moved
                        && last_move == existing.last_move
                    {
                        false
                    } else {
                        existing.initialized = merged;
                        existing.moved = moved;
                        existing.last_move = last_move;
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
        && !state.moved.keys().any(|moved| places_overlap(moved, place))
}

fn move_origin(state: &State, place: &crate::hir::Place) -> Option<Range<usize>> {
    if !state.initialized.contains(&place.root) {
        return state.last_move.get(&place.root).cloned();
    }
    state
        .moved
        .iter()
        .find(|(moved, _)| places_overlap(moved, place))
        .map(|(_, span)| span.clone())
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
