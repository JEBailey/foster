//! Request obligations follow values through the ownership CFG, independently of loans.
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::ops::Range;

use super::mir::place_contains;
use super::{BorrowValue, MirPoint, Operation, Place, Program, Terminator};
use crate::{error::FosterError, hir, types::TypeInformation};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Owner {
    Local(MirPoint),
    Parameter(hir::LocalId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Identity {
    Owner(Owner),
    Request(MirPoint),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Request {
    predecessors: BTreeSet<MirPoint>,
    owners: BTreeSet<Owner>,
    repeated: bool,
    span: Range<usize>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct State {
    contents: HashMap<Place, BTreeSet<Identity>>,
    pending: HashMap<MirPoint, Request>,
    transferred: BTreeSet<Owner>,
    returned: BTreeSet<MirPoint>,
}

impl State {
    fn read(&self, place: &Place) -> Vec<(Vec<hir::Projection>, BTreeSet<Identity>)> {
        self.contents
            .iter()
            .filter_map(|(stored, identities)| {
                if place_contains(place, stored) {
                    Some((
                        stored.projections[place.projections.len()..].to_vec(),
                        identities.clone(),
                    ))
                } else if place_contains(stored, place) {
                    Some((vec![], identities.clone()))
                } else {
                    None
                }
            })
            .collect()
    }

    fn remove(&mut self, place: &Place) {
        self.contents
            .retain(|stored, _| !place_contains(place, stored));
    }

    fn value(&mut self, value: &BorrowValue) -> Vec<(Vec<hir::Projection>, BTreeSet<Identity>)> {
        match value {
            BorrowValue::Callable { environment, .. } => self.value(environment),
            BorrowValue::Invocation {
                callee, arguments, ..
            } => {
                let mut values = self.value(callee);
                values.extend(arguments.iter().flat_map(|argument| self.value(argument)));
                values
            }
            BorrowValue::Tracked { place, .. } => self.read(place),
            BorrowValue::Place(place) => self.read(place),
            BorrowValue::MovePlace(place) => {
                let result = self.read(place);
                self.remove(place);
                result
            }
            BorrowValue::Merge(values) => {
                values.iter().flat_map(|value| self.value(value)).collect()
            }
            BorrowValue::Fields(fields) => fields
                .iter()
                .flat_map(|(prefix, value)| {
                    self.value(value)
                        .into_iter()
                        .map(|(suffix, identities)| {
                            (prefix.iter().cloned().chain(suffix).collect(), identities)
                        })
                        .collect::<Vec<_>>()
                })
                .collect(),
            BorrowValue::Reborrow { origin, .. } => self.read(origin),
            BorrowValue::Empty | BorrowValue::Loan(_) => vec![],
        }
    }

    fn identities(&mut self, value: &BorrowValue) -> BTreeSet<Identity> {
        self.value(value)
            .into_iter()
            .flat_map(|(_, identities)| identities)
            .collect()
    }

    fn live_owners(&self) -> BTreeSet<Owner> {
        self.contents
            .values()
            .flatten()
            .filter_map(|identity| match identity {
                Identity::Owner(owner) => Some(*owner),
                _ => None,
            })
            .chain(self.transferred.iter().copied())
            .collect()
    }

    fn missing_owner(&self) -> Option<&Request> {
        let live = self.live_owners();
        self.pending
            .values()
            .filter(|request| request.owners.iter().any(|owner| !live.contains(owner)))
            .min_by_key(|request| request.span.start)
    }
}

fn diagnostic(
    hir: &hir::PackageHir,
    function: hir::FunctionId,
    request: &Request,
    span: Range<usize>,
) -> FosterError {
    FosterError::runtime("remote owner leaves scope while a request may still be pending")
        .with_code(super::diagnostics::PENDING_REMOTE)
        .with_source_module(hir.modules[hir.functions[function].module].name.clone())
        .with_primary_label(span, "remote owner leaves scope here")
        .with_label(request.span.clone(), "request created here")
        .with_help("await the request before leaving this scope, or transfer the remote owner to a longer-lived scope")
}

pub(super) fn check(
    hir: &hir::PackageHir,
    types: &TypeInformation,
    program: &Program,
) -> Result<(), FosterError> {
    let mut functions = program.functions.keys().copied().collect::<Vec<_>>();
    functions.sort();
    for function in functions {
        let mir = &program.functions[&function];
        if !mir
            .blocks
            .iter()
            .flat_map(|block| &block.operations)
            .any(|op| matches!(op, Operation::RemoteRequest { .. }))
        {
            continue;
        }
        let mut initial = State::default();
        for parameter in &hir.functions[function].parameters {
            if types
                .local_type(*parameter)
                .is_some_and(|ty| matches!(types.types[ty], crate::types::Type::Remote(_)))
            {
                initial.contents.insert(
                    Place::local(*parameter),
                    BTreeSet::from([Identity::Owner(Owner::Parameter(*parameter))]),
                );
            }
        }
        // Keep alternatives distinct: joining possible aliases must never invent an owner
        // that keeps a request alive on a different branch.
        let mut seen = vec![Vec::<State>::new(); mir.blocks.len()];
        let mut work = VecDeque::from([(mir.entry, initial)]);
        while let Some((block_id, mut state)) = work.pop_front() {
            if seen[block_id].contains(&state) {
                continue;
            }
            if seen[block_id].len() >= 256 {
                return Err(FosterError::runtime("remote request lifetime analysis exceeded its path limit; simplify control flow or await requests earlier")
                    .with_code(super::diagnostics::PENDING_REMOTE)
                    .with_source_module(hir.modules[hir.functions[function].module].name.clone()));
            }
            seen[block_id].push(state.clone());
            let block = &mir.blocks[block_id];
            // Exceptional exits are handled by runtime shutdown, not by pretending they
            // successfully complete requests or prohibiting all fallible code.
            if matches!(block.terminator, Terminator::Fail) {
                continue;
            }
            for (index, operation) in block.operations.iter().enumerate() {
                let point = MirPoint {
                    block: block_id,
                    operation: index,
                };
                let boundary = match operation {
                    Operation::RemoteScopeEnd { places, span } => {
                        for place in places {
                            state.remove(place);
                        }
                        Some(span)
                    }
                    Operation::RemoteConsume { value, span } => {
                        let identities = state.clone().identities(value);
                        if let Some(request) = state.pending.values().find(|request| {
                            request
                                .owners
                                .iter()
                                .any(|owner| identities.contains(&Identity::Owner(*owner)))
                        }) {
                            return Err(diagnostic(hir, function, request, span.clone()));
                        }
                        None
                    }
                    Operation::RemoteOwner { destination, .. } => {
                        state
                            .contents
                            .entry(destination.clone())
                            .or_default()
                            .insert(Identity::Owner(Owner::Local(point)));
                        None
                    }
                    Operation::RemoteRequest {
                        destination,
                        owner,
                        span,
                    } => {
                        let identities = state.identities(owner);
                        state.remove(destination);
                        let owners = identities
                            .iter()
                            .filter_map(|identity| match identity {
                                Identity::Owner(owner) => Some(*owner),
                                _ => None,
                            })
                            .collect::<BTreeSet<_>>();
                        if !owners.is_empty() {
                            let repeated = state.pending.contains_key(&point);
                            state.pending.insert(
                                point,
                                Request {
                                    predecessors: state
                                        .pending
                                        .iter()
                                        .filter(|(_, request)| {
                                            request.owners == owners && !request.repeated
                                        })
                                        .map(|(id, _)| *id)
                                        .collect(),
                                    owners,
                                    repeated,
                                    span: span.clone(),
                                },
                            );
                            state
                                .contents
                                .entry(destination.clone())
                                .or_default()
                                .insert(Identity::Request(point));
                        } else {
                            // A future-returning forwarding helper may return an existing request.
                            state
                                .contents
                                .entry(destination.clone())
                                .or_default()
                                .extend(identities);
                        }
                        None
                    }
                    Operation::RemoteComplete { future, .. } => {
                        let identities = state.identities(future);
                        let requests = identities
                            .iter()
                            .filter_map(|identity| match identity {
                                Identity::Request(request) => Some(*request),
                                _ => None,
                            })
                            .collect::<Vec<_>>();
                        if let [request] = requests.as_slice()
                            && state
                                .pending
                                .get(request)
                                .is_some_and(|request| !request.repeated)
                            && let Some(completed) = state.pending.remove(request)
                        {
                            for previous in completed.predecessors {
                                if state
                                    .pending
                                    .get(&previous)
                                    .is_some_and(|request| !request.repeated)
                                {
                                    state.pending.remove(&previous);
                                }
                            }
                        }
                        None
                    }
                    Operation::StoreBorrower {
                        destination,
                        value,
                        span,
                    } => {
                        let fields = state.value(value);
                        state.remove(destination);
                        for (suffix, identities) in fields {
                            let mut place = destination.clone();
                            place.projections.extend(suffix);
                            state.contents.entry(place).or_default().extend(identities);
                        }
                        Some(span)
                    }
                    Operation::Destroy { place, span } => {
                        state.remove(place);
                        Some(span)
                    }
                    Operation::ReturnBorrower { value, .. } => {
                        for identity in state.identities(value) {
                            match identity {
                                Identity::Owner(owner) => {
                                    state.transferred.insert(owner);
                                }
                                Identity::Request(request) => {
                                    state.returned.insert(request);
                                }
                            }
                        }
                        None
                    }
                    _ => None,
                };
                if let Some(span) = boundary
                    && let Some(request) = state.missing_owner()
                {
                    return Err(diagnostic(hir, function, request, span.clone()));
                }
            }
            if matches!(block.terminator, Terminator::Return) {
                for (id, request) in &state.pending {
                    let direct_future = types.function_type(function).is_some_and(|signature| {
                        matches!(types.types[signature.result], crate::types::Type::Future(_))
                    });
                    // Returning pending work is supported only as a direct future tied to
                    // a borrowed owner parameter. Pending aggregate/owner transfers need
                    // a richer interprocedural relationship than a plain record type.
                    if !direct_future
                        || !state.returned.contains(id)
                        || request.owners.iter().any(|owner| match owner {
                            Owner::Local(_) => true,
                            Owner::Parameter(parameter) => hir.functions[function]
                                .parameters
                                .iter()
                                .position(|local| local == parameter)
                                .and_then(|index| {
                                    types
                                        .function_type(function)
                                        .map(|signature| signature.parameter_modes[index])
                                })
                                .is_some_and(|mode| mode == crate::ast::ParameterMode::Consume),
                        })
                    {
                        return Err(diagnostic(
                            hir,
                            function,
                            request,
                            hir.functions[function].span.clone(),
                        ));
                    }
                }
            }
            for successor in block.terminator.successors() {
                work.push_back((*successor, state.clone()));
            }
        }
    }
    Ok(())
}
