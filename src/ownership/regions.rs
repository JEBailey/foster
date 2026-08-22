use std::collections::{HashSet, VecDeque};

use crate::error::FosterError;
use crate::hir::{FunctionId, PackageHir, Place, Projection};

use super::{
    BorrowValue, Function, InvalidationKind, LoanId, Operation, Program, ProvenanceAnalysis,
    ProvenanceState, RequirementAnalysis, RequirementState,
};

pub(super) fn analyze(
    program: &Program,
) -> std::collections::HashMap<FunctionId, ProvenanceAnalysis> {
    program
        .functions
        .iter()
        .map(|(id, function)| (*id, analyze_function(function)))
        .collect()
}

pub(super) fn analyze_requirements(
    program: &Program,
) -> std::collections::HashMap<FunctionId, RequirementAnalysis> {
    program
        .functions
        .iter()
        .map(|(id, function)| {
            (
                *id,
                analyze_function_requirements(function, &program.provenance[id]),
            )
        })
        .collect()
}

fn analyze_function_requirements(
    function: &Function,
    provenance: &ProvenanceAnalysis,
) -> RequirementAnalysis {
    let mut entries = vec![Some(RequirementState::default()); function.blocks.len()];
    let mut exits = vec![Some(RequirementState::default()); function.blocks.len()];
    let mut points = vec![None::<Vec<RequirementState>>; function.blocks.len()];
    let mut predecessors = vec![Vec::new(); function.blocks.len()];
    for (block, definition) in function.blocks.iter().enumerate() {
        for successor in definition.terminator.successors() {
            predecessors[*successor].push(block);
        }
    }
    let mut work = (0..function.blocks.len()).rev().collect::<VecDeque<_>>();

    while let Some(block) = work.pop_front() {
        if provenance.entries[block].is_none() {
            entries[block] = None;
            exits[block] = None;
            points[block] = None;
            continue;
        }
        let mut state = join_successors(function, block, &entries);
        exits[block] = Some(state.clone());
        let mut reverse_points = Vec::with_capacity(function.blocks[block].operations.len() + 1);
        reverse_points.push(state.clone());
        for (operation_index, operation) in
            function.blocks[block].operations.iter().enumerate().rev()
        {
            transfer_requirement(
                function,
                operation,
                block,
                operation_index,
                provenance,
                &mut state,
            );
            reverse_points.push(state.clone());
        }
        reverse_points.reverse();
        points[block] = Some(reverse_points);
        if entries[block].as_ref() != Some(&state) {
            entries[block] = Some(state);
            work.extend(predecessors[block].iter().copied());
        }
    }

    RequirementAnalysis {
        entries,
        exits,
        points,
    }
}

fn join_successors(
    function: &Function,
    block: usize,
    entries: &[Option<RequirementState>],
) -> RequirementState {
    let mut result = RequirementState::default();
    for successor in function.blocks[block].terminator.successors() {
        if let Some(incoming) = &entries[*successor] {
            join_requirements(&mut result, incoming);
        }
    }
    result
}

fn transfer_requirement(
    function: &Function,
    operation: &Operation,
    block: usize,
    operation_index: usize,
    provenance: &ProvenanceAnalysis,
    state: &mut RequirementState,
) {
    match operation {
        Operation::Use {
            place: _,
            mode: super::UseMode::Write,
            ..
        } => {}
        Operation::Use { place, mode, span } => {
            let before =
                &provenance.points[block].as_ref().expect("reachable block")[operation_index];
            for loan in contents_at(before, place) {
                state
                    .loans
                    .entry(loan)
                    .or_insert_with(|| super::RequiredUse {
                        place: place.clone(),
                        mode: *mode,
                        span: span.clone(),
                    });
            }
        }
        Operation::ReturnBorrower { value, span, .. } => {
            let before =
                &provenance.points[block].as_ref().expect("reachable block")[operation_index];
            let mut temporary = before.clone();
            for loan in evaluate(value, &mut temporary) {
                state
                    .loans
                    .entry(loan)
                    .or_insert_with(|| super::RequiredUse {
                        place: function.loans[loan.0].origin.clone(),
                        mode: super::UseMode::Move,
                        span: span.clone(),
                    });
            }
            remove_issued_at(function, block, operation_index, state);
        }
        Operation::StoreBorrower { .. } => {
            remove_issued_at(function, block, operation_index, state);
        }
        Operation::Initialize { .. } | Operation::Invalidate { .. } | Operation::Suspend { .. } => {
        }
    }
}

fn remove_issued_at(
    function: &Function,
    block: usize,
    operation: usize,
    state: &mut RequirementState,
) {
    for loan in function
        .loans
        .iter()
        .filter(|loan| loan.issued_at.block == block && loan.issued_at.operation == operation)
    {
        state.loans.remove(&loan.id);
    }
}

fn contents_at(state: &ProvenanceState, place: &Place) -> HashSet<LoanId> {
    state
        .contents
        .iter()
        .filter(|(stored, _)| places_overlap(stored, place))
        .flat_map(|(_, loans)| loans.iter().copied())
        .collect()
}

fn join_requirements(existing: &mut RequirementState, incoming: &RequirementState) {
    for (loan, required_use) in &incoming.loans {
        existing
            .loans
            .entry(*loan)
            .or_insert_with(|| required_use.clone());
    }
}

pub(super) fn validate(hir: &PackageHir, program: &Program) -> Result<(), FosterError> {
    for (function_id, function) in &program.functions {
        validate_storage_and_escape(
            hir,
            *function_id,
            function,
            &program.provenance[function_id],
        )?;
        let requirements = &program.requirements[function_id];
        if let Some(conflict) = find_conflict(function, requirements) {
            let definition = &hir.functions[*function_id];
            let origin_name = &hir.locals[conflict.loan.origin.root].name;
            let borrower_name = &hir.locals[conflict.required_use.place.root].name;
            let is_call = conflict.required_use.mode == super::UseMode::Call;
            let message = if is_call {
                format!(
                    "in `{}.{}`: closure `{borrower_name}` is no longer callable; structural mutation invalidated its captured reference into `{origin_name}`",
                    hir.modules[definition.module].name, definition.name
                )
            } else {
                format!(
                    "in `{}.{}`: borrowed value `{borrower_name}` is no longer usable; its reference into `{origin_name}` was invalidated",
                    hir.modules[definition.module].name, definition.name
                )
            };
            return Err(FosterError::runtime(message)
            .with_code("E0401")
            .with_source_module(hir.modules[definition.module].name.clone())
            .with_primary_label(
                conflict.required_use.span,
                if is_call {
                    "this call uses an invalidated captured reference"
                } else {
                    "this invalidated borrow is used here"
                },
            )
            .with_label(
                conflict.loan.span,
                format!("loan from `{origin_name}` is issued here"),
            )
            .with_label(
                conflict.invalidated_at,
                match conflict.kind {
                    InvalidationKind::Reshape => {
                        format!("this operation reshaped `{origin_name}`")
                    }
                    InvalidationKind::Consume => {
                        format!("this operation consumes `{origin_name}`")
                    }
                },
            )
            .with_help("use or propagate the borrower before this invalidating operation, or reacquire it afterward"));
        }
    }
    Ok(())
}

fn validate_storage_and_escape(
    hir: &PackageHir,
    function_id: FunctionId,
    function: &Function,
    provenance: &ProvenanceAnalysis,
) -> Result<(), FosterError> {
    let definition = &hir.functions[function_id];
    for (block, basic_block) in function.blocks.iter().enumerate() {
        let Some(points) = &provenance.points[block] else {
            continue;
        };
        for (operation_index, operation) in basic_block.operations.iter().enumerate() {
            let before = &points[operation_index];
            match operation {
                Operation::StoreBorrower {
                    destination,
                    value,
                    span,
                } => {
                    let mut temporary = before.clone();
                    let loans = evaluate(value, &mut temporary);
                    if let Some(loan) = loans
                        .iter()
                        .map(|loan| &function.loans[loan.0])
                        .filter(|loan| places_overlap(destination, &loan.origin))
                        .filter(|loan| {
                            !(definition.parameters.contains(&destination.root)
                                && loan.origin == *destination
                                && loan.issued_at.block == block
                                && loan.issued_at.operation == operation_index)
                        })
                        .min_by_key(|loan| loan.id)
                    {
                        let name = &hir.locals[destination.root].name;
                        return Err(FosterError::runtime(format!(
                            "in `{}.{}`: cannot store a value borrowing `{name}` into its own origin",
                            hir.modules[definition.module].name, definition.name
                        ))
                        .with_code("E0403")
                        .with_source_module(hir.modules[definition.module].name.clone())
                        .with_primary_label(span.clone(), "this value borrows from its destination")
                        .with_label(loan.span.clone(), "the stored borrower originates here")
                        .with_help("store the borrower outside its origin, or capture the required value by ownership"));
                    }
                }
                Operation::ReturnBorrower { value, kind, span } => {
                    let mut temporary = before.clone();
                    let loans = evaluate(value, &mut temporary);
                    let mut returned = loans
                        .iter()
                        .map(|loan| &function.loans[loan.0])
                        .collect::<Vec<_>>();
                    returned.sort_by_key(|loan| loan.id);
                    returned.dedup_by_key(|loan| loan.id);
                    for loan in returned {
                        validate_returned_loan(hir, definition, function_id, loan, *kind, span)?;
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn validate_returned_loan(
    hir: &PackageHir,
    function: &crate::hir::Function,
    function_id: FunctionId,
    loan: &super::LoanDefinition,
    kind: super::ReturnKind,
    returned_at: &std::ops::Range<usize>,
) -> Result<(), FosterError> {
    let module = &hir.modules[function.module].name;
    let name = &hir.locals[loan.origin.root].name;
    let Some(parameter) = function
        .parameters
        .iter()
        .position(|parameter| *parameter == loan.origin.root)
    else {
        let noun = match kind {
            super::ReturnKind::Closure => "closure",
            super::ReturnKind::Reference => "reference",
            super::ReturnKind::Aggregate => "value",
        };
        return Err(FosterError::runtime(format!(
            "in `{module}.{}`: returned {noun} borrows local `{name}`",
            function.name
        ))
        .with_code("E0402")
        .with_source_module(module.clone())
        .with_primary_label(
            returned_at.clone(),
            "this returned value contains a reference to frame-local storage",
        )
        .with_label(hir.locals[loan.origin.root].span.clone(), "borrowed local is declared here")
        .with_help("return an owned value, or borrow from a reference parameter whose group appears in the result type"));
    };
    let Some(crate::ast::TypeExpr::Reference { group, .. }) =
        function.parameter_types[parameter].as_ref()
    else {
        return Err(FosterError::runtime(format!(
            "in `{module}.{}`: returned reference borrows parameter `{name}` without an exposed group",
            function.name
        ))
        .with_code("E0402")
        .with_source_module(module.clone())
        .with_primary_label(returned_at.clone(), "this return exposes a borrow without a named result group"));
    };
    if !crate::hir::queries::type_exposes_group(function.return_type.as_ref(), group) {
        return Err(FosterError::runtime(format!(
            "in `{module}.{}`: returned reference group `{group}` is absent from the result type",
            function.name
        ))
        .with_code("E0402")
        .with_source_module(module.clone())
        .with_primary_label(
            returned_at.clone(),
            format!("returned borrow belongs to group `{group}`"),
        )
        .with_label(
            hir.functions[function_id].span.clone(),
            "function result contract does not expose this group",
        )
        .with_help(format!(
            "include group `{group}` in the declared result type"
        )));
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct InvalidationConflict {
    loan: super::LoanDefinition,
    kind: InvalidationKind,
    invalidated_at: std::ops::Range<usize>,
    required_use: super::RequiredUse,
}

fn find_conflict(
    function: &Function,
    requirements: &RequirementAnalysis,
) -> Option<InvalidationConflict> {
    for (block, definition) in function.blocks.iter().enumerate() {
        let Some(points) = &requirements.points[block] else {
            continue;
        };
        for (operation_index, operation) in definition.operations.iter().enumerate() {
            let Operation::Invalidate { place, kind, span } = operation else {
                continue;
            };
            let required_after = &points[operation_index + 1];
            if let Some((loan, required_use)) = required_after
                .loans
                .iter()
                .filter_map(|(id, use_span)| {
                    let loan = &function.loans[id.0];
                    invalidates(place, *kind, &loan.origin).then_some((loan, use_span))
                })
                .min_by_key(|(loan, _)| loan.id)
            {
                return Some(InvalidationConflict {
                    loan: loan.clone(),
                    kind: *kind,
                    invalidated_at: span.clone(),
                    required_use: required_use.clone(),
                });
            }
        }
    }
    None
}

fn invalidates(invalidated: &Place, kind: InvalidationKind, origin: &Place) -> bool {
    places_overlap(invalidated, origin)
        && (kind == InvalidationKind::Consume
            || origin
                .projections
                .iter()
                .any(|projection| matches!(projection, Projection::Index(_))))
}

fn analyze_function(function: &Function) -> ProvenanceAnalysis {
    let mut entries = vec![None::<ProvenanceState>; function.blocks.len()];
    let mut exits = vec![None::<ProvenanceState>; function.blocks.len()];
    let mut points = vec![None::<Vec<ProvenanceState>>; function.blocks.len()];
    let mut predecessors = vec![Vec::new(); function.blocks.len()];
    for (block, definition) in function.blocks.iter().enumerate() {
        for successor in definition.terminator.successors() {
            predecessors[*successor].push(block);
        }
    }
    entries[function.entry] = Some(ProvenanceState::default());
    let mut work = VecDeque::from([function.entry]);

    while let Some(block) = work.pop_front() {
        let incoming = join_predecessors(block, function.entry, &predecessors, &exits);
        if incoming != entries[block] {
            entries[block] = incoming;
        }
        let Some(mut state) = entries[block].clone() else {
            continue;
        };
        let mut block_points = Vec::with_capacity(function.blocks[block].operations.len() + 1);
        for operation in &function.blocks[block].operations {
            block_points.push(state.clone());
            if let Operation::StoreBorrower {
                destination, value, ..
            } = operation
            {
                store(destination, value, &mut state);
            }
        }
        block_points.push(state.clone());
        points[block] = Some(block_points);
        if exits[block].as_ref() != Some(&state) {
            exits[block] = Some(state);
            for successor in function.blocks[block].terminator.successors() {
                work.push_back(*successor);
            }
        }
    }

    ProvenanceAnalysis {
        entries,
        exits,
        points,
    }
}

fn join_predecessors(
    block: usize,
    entry: usize,
    predecessors: &[Vec<usize>],
    exits: &[Option<ProvenanceState>],
) -> Option<ProvenanceState> {
    let mut states = predecessors[block]
        .iter()
        .filter_map(|predecessor| exits[*predecessor].as_ref());
    let mut result = if block == entry {
        ProvenanceState::default()
    } else {
        states.next()?.clone()
    };
    for state in states {
        join(&mut result, state);
    }
    Some(result)
}

fn store(destination: &Place, value: &BorrowValue, state: &mut ProvenanceState) {
    match value {
        BorrowValue::Fields(fields) => {
            replace(destination, HashSet::new(), state);
            for (projections, value) in fields {
                let mut field = destination.clone();
                field.projections.extend(projections.iter().cloned());
                store(&field, value, state);
            }
        }
        _ => {
            let loans = evaluate(value, state);
            replace(destination, loans, state);
        }
    }
}

fn evaluate(value: &BorrowValue, state: &mut ProvenanceState) -> HashSet<LoanId> {
    match value {
        BorrowValue::Empty => HashSet::new(),
        BorrowValue::Loan(loan) => HashSet::from([*loan]),
        BorrowValue::Reborrow { loan, origin } => {
            let inherited = contents_at(state, origin);
            if inherited.is_empty() {
                HashSet::from([*loan])
            } else {
                inherited
            }
        }
        BorrowValue::Place(place) => state
            .contents
            .iter()
            .filter(|(stored, _)| places_overlap(stored, place))
            .flat_map(|(_, loans)| loans.iter().copied())
            .collect(),
        BorrowValue::MovePlace(place) => {
            let loans = state
                .contents
                .iter()
                .filter(|(stored, _)| places_overlap(stored, place))
                .flat_map(|(_, loans)| loans.iter().copied())
                .collect();
            state
                .contents
                .retain(|stored, _| !place_contains(place, stored));
            loans
        }
        BorrowValue::Merge(values) => values
            .iter()
            .flat_map(|value| evaluate(value, state))
            .collect(),
        BorrowValue::Fields(fields) => fields
            .iter()
            .flat_map(|(_, value)| evaluate(value, state))
            .collect(),
    }
}

fn replace(destination: &Place, loans: HashSet<LoanId>, state: &mut ProvenanceState) {
    state
        .contents
        .retain(|stored, _| !place_contains(destination, stored));
    if !loans.is_empty() {
        state.contents.insert(destination.clone(), loans);
    }
}

fn join(existing: &mut ProvenanceState, incoming: &ProvenanceState) -> bool {
    let mut changed = false;
    for (place, loans) in &incoming.contents {
        let entry = existing.contents.entry(place.clone()).or_default();
        let old = entry.len();
        entry.extend(loans);
        changed |= entry.len() != old;
    }
    changed
}

fn place_contains(parent: &Place, child: &Place) -> bool {
    parent.root == child.root
        && parent.projections.len() <= child.projections.len()
        && parent
            .projections
            .iter()
            .zip(&child.projections)
            .all(|(left, right)| left == right)
}

fn places_overlap(left: &Place, right: &Place) -> bool {
    if left.root != right.root {
        return false;
    }
    for (left, right) in left.projections.iter().zip(&right.projections) {
        match (left, right) {
            (Projection::Field(left), Projection::Field(right)) if left != right => return false,
            (Projection::Field(_), Projection::Field(_))
            | (Projection::Index(_), Projection::Index(_))
            | (Projection::Dereference, Projection::Dereference) => {}
            _ => return true,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ownership::{BasicBlock, Terminator};

    #[test]
    fn replacement_kills_old_contents_and_joins_reaching_loans() {
        let local = crate::hir::LocalId::from_raw(la_arena::RawIdx::from_u32(0));
        let place = Place {
            root: local,
            projections: Vec::new(),
        };
        let store = |loan| Operation::StoreBorrower {
            destination: place.clone(),
            value: BorrowValue::Loan(loan),
            span: 0..0,
        };
        let function = Function {
            entry: 0,
            blocks: vec![
                BasicBlock {
                    operations: Vec::new(),
                    terminator: Terminator::Branch(vec![1, 2]),
                },
                BasicBlock {
                    operations: vec![store(LoanId(0))],
                    terminator: Terminator::Goto(3),
                },
                BasicBlock {
                    operations: vec![store(LoanId(1))],
                    terminator: Terminator::Goto(3),
                },
                BasicBlock::default(),
            ],
            loans: Vec::new(),
            result_provenance: Default::default(),
        };
        let analysis = analyze_function(&function);
        assert_eq!(
            analysis.entries[3]
                .as_ref()
                .unwrap()
                .contents
                .get(&place)
                .unwrap(),
            &HashSet::from([LoanId(0), LoanId(1)])
        );
    }

    #[test]
    fn branch_overwrite_does_not_keep_the_replaced_definition() {
        let local = crate::hir::LocalId::from_raw(la_arena::RawIdx::from_u32(0));
        let place = Place {
            root: local,
            projections: Vec::new(),
        };
        let store = |value| Operation::StoreBorrower {
            destination: place.clone(),
            value,
            span: 0..0,
        };
        let function = Function {
            entry: 0,
            blocks: vec![
                BasicBlock {
                    operations: vec![store(BorrowValue::Loan(LoanId(0)))],
                    terminator: Terminator::Branch(vec![1, 2]),
                },
                BasicBlock {
                    operations: vec![store(BorrowValue::Empty)],
                    terminator: Terminator::Goto(3),
                },
                BasicBlock {
                    operations: vec![store(BorrowValue::Loan(LoanId(1)))],
                    terminator: Terminator::Goto(3),
                },
                BasicBlock::default(),
            ],
            loans: Vec::new(),
            result_provenance: Default::default(),
        };
        let analysis = analyze_function(&function);
        assert_eq!(
            analysis.entries[3].as_ref().unwrap().contents.get(&place),
            Some(&HashSet::from([LoanId(1)]))
        );
    }

    #[test]
    fn field_replacement_preserves_disjoint_borrower_contents() {
        let local = crate::hir::LocalId::from_raw(la_arena::RawIdx::from_u32(0));
        let root = Place {
            root: local,
            projections: Vec::new(),
        };
        let left = Place {
            root: local,
            projections: vec![crate::hir::Projection::Field("left".into())],
        };
        let right = Place {
            root: local,
            projections: vec![crate::hir::Projection::Field("right".into())],
        };
        let function = Function {
            entry: 0,
            blocks: vec![BasicBlock {
                operations: vec![
                    Operation::StoreBorrower {
                        destination: root,
                        value: BorrowValue::Fields(vec![
                            (
                                vec![crate::hir::Projection::Field("left".into())],
                                BorrowValue::Loan(LoanId(0)),
                            ),
                            (
                                vec![crate::hir::Projection::Field("right".into())],
                                BorrowValue::Loan(LoanId(1)),
                            ),
                        ]),
                        span: 0..0,
                    },
                    Operation::StoreBorrower {
                        destination: left.clone(),
                        value: BorrowValue::Empty,
                        span: 0..0,
                    },
                ],
                terminator: Terminator::Return,
            }],
            loans: Vec::new(),
            result_provenance: Default::default(),
        };
        let analysis = analyze_function(&function);
        let exit = analysis.exits[0].as_ref().unwrap();
        assert!(!exit.contents.contains_key(&left));
        assert_eq!(exit.contents.get(&right), Some(&HashSet::from([LoanId(1)])));
        assert_eq!(analysis.points[0].as_ref().unwrap().len(), 3);
    }

    #[test]
    fn backward_demand_finds_three_site_invalidation_conflicts() {
        let borrower = crate::hir::LocalId::from_raw(la_arena::RawIdx::from_u32(0));
        let owner = crate::hir::LocalId::from_raw(la_arena::RawIdx::from_u32(1));
        let index = crate::hir::ExprId::from_raw(la_arena::RawIdx::from_u32(0));
        let borrower_place = Place {
            root: borrower,
            projections: Vec::new(),
        };
        let owner_place = Place {
            root: owner,
            projections: Vec::new(),
        };
        let loan = super::super::LoanDefinition {
            id: LoanId(0),
            origin: Place {
                root: owner,
                projections: vec![Projection::Index(index)],
            },
            issued_at: super::super::MirPoint {
                block: 0,
                operation: 0,
            },
            parent: None,
            span: 0..1,
        };
        let function = Function {
            entry: 0,
            blocks: vec![BasicBlock {
                operations: vec![
                    Operation::StoreBorrower {
                        destination: borrower_place.clone(),
                        value: BorrowValue::Loan(LoanId(0)),
                        span: 0..1,
                    },
                    Operation::Invalidate {
                        place: owner_place,
                        kind: InvalidationKind::Reshape,
                        span: 2..3,
                    },
                    Operation::Use {
                        place: borrower_place,
                        mode: super::super::UseMode::Read,
                        span: 4..5,
                    },
                ],
                terminator: Terminator::Return,
            }],
            loans: vec![loan],
            result_provenance: Default::default(),
        };
        let provenance = analyze_function(&function);
        let requirements = analyze_function_requirements(&function, &provenance);
        let conflict = find_conflict(&function, &requirements).unwrap();
        assert_eq!(conflict.loan.span, 0..1);
        assert_eq!(conflict.invalidated_at, 2..3);
        assert_eq!(conflict.required_use.span, 4..5);
        assert!(
            requirements.points[0].as_ref().unwrap()[2]
                .loans
                .contains_key(&LoanId(0))
        );
        assert!(
            !requirements.points[0].as_ref().unwrap()[0]
                .loans
                .contains_key(&LoanId(0))
        );
    }

    #[test]
    fn backward_demand_ends_a_loan_after_its_last_use() {
        let borrower = crate::hir::LocalId::from_raw(la_arena::RawIdx::from_u32(0));
        let owner = crate::hir::LocalId::from_raw(la_arena::RawIdx::from_u32(1));
        let index = crate::hir::ExprId::from_raw(la_arena::RawIdx::from_u32(0));
        let borrower_place = Place {
            root: borrower,
            projections: Vec::new(),
        };
        let function = Function {
            entry: 0,
            blocks: vec![BasicBlock {
                operations: vec![
                    Operation::StoreBorrower {
                        destination: borrower_place.clone(),
                        value: BorrowValue::Loan(LoanId(0)),
                        span: 0..1,
                    },
                    Operation::Use {
                        place: borrower_place,
                        mode: super::super::UseMode::Read,
                        span: 2..3,
                    },
                    Operation::Invalidate {
                        place: Place {
                            root: owner,
                            projections: Vec::new(),
                        },
                        kind: InvalidationKind::Reshape,
                        span: 4..5,
                    },
                ],
                terminator: Terminator::Return,
            }],
            loans: vec![super::super::LoanDefinition {
                id: LoanId(0),
                origin: Place {
                    root: owner,
                    projections: vec![Projection::Index(index)],
                },
                issued_at: super::super::MirPoint {
                    block: 0,
                    operation: 0,
                },
                parent: None,
                span: 0..1,
            }],
            result_provenance: Default::default(),
        };
        let provenance = analyze_function(&function);
        let requirements = analyze_function_requirements(&function, &provenance);
        assert!(find_conflict(&function, &requirements).is_none());
    }

    #[test]
    fn mutually_exclusive_invalidation_and_use_do_not_conflict() {
        let borrower = crate::hir::LocalId::from_raw(la_arena::RawIdx::from_u32(0));
        let owner = crate::hir::LocalId::from_raw(la_arena::RawIdx::from_u32(1));
        let index = crate::hir::ExprId::from_raw(la_arena::RawIdx::from_u32(0));
        let borrower_place = Place {
            root: borrower,
            projections: Vec::new(),
        };
        let function = Function {
            entry: 0,
            blocks: vec![
                BasicBlock {
                    operations: vec![Operation::StoreBorrower {
                        destination: borrower_place.clone(),
                        value: BorrowValue::Loan(LoanId(0)),
                        span: 0..1,
                    }],
                    terminator: Terminator::Branch(vec![1, 2]),
                },
                BasicBlock {
                    operations: vec![Operation::Invalidate {
                        place: Place {
                            root: owner,
                            projections: Vec::new(),
                        },
                        kind: InvalidationKind::Reshape,
                        span: 2..3,
                    }],
                    terminator: Terminator::Return,
                },
                BasicBlock {
                    operations: vec![Operation::Use {
                        place: borrower_place,
                        mode: super::super::UseMode::Read,
                        span: 4..5,
                    }],
                    terminator: Terminator::Return,
                },
            ],
            loans: vec![super::super::LoanDefinition {
                id: LoanId(0),
                origin: Place {
                    root: owner,
                    projections: vec![Projection::Index(index)],
                },
                issued_at: super::super::MirPoint {
                    block: 0,
                    operation: 0,
                },
                parent: None,
                span: 0..1,
            }],
            result_provenance: Default::default(),
        };
        let provenance = analyze_function(&function);
        let requirements = analyze_function_requirements(&function, &provenance);
        assert!(find_conflict(&function, &requirements).is_none());
        assert!(requirements.entries[1].as_ref().unwrap().loans.is_empty());
        assert!(
            requirements.entries[2]
                .as_ref()
                .unwrap()
                .loans
                .contains_key(&LoanId(0))
        );
    }
}
