use std::collections::{HashSet, VecDeque};

use crate::error::FosterError;
use crate::hir::queries::{place_contains, places_overlap};
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

pub(super) fn populate_reborrow_parents(program: &mut Program) {
    for (function_id, function) in &mut program.functions {
        let analysis = &program.provenance[function_id];
        for (block, definition) in function.blocks.iter().enumerate() {
            let Some(points) = &analysis.points[block] else {
                continue;
            };
            for (operation_index, operation) in definition.operations.iter().enumerate() {
                let (Operation::StoreBorrower { value, .. }
                | Operation::ReturnBorrower { value, .. }) = operation
                else {
                    continue;
                };
                collect_reborrow_parents(value, &points[operation_index], &mut function.loans);
            }
        }
    }
}

pub(super) fn infer_result_provenance(hir: &PackageHir, program: &mut Program) {
    for (function_id, function) in &mut program.functions {
        let definition = &hir.functions[*function_id];
        let analysis = &program.provenance[function_id];
        let mut parameters = HashSet::new();
        let mut returns_borrower = false;
        for (block, basic_block) in function.blocks.iter().enumerate() {
            let Some(points) = &analysis.points[block] else {
                continue;
            };
            for (operation_index, operation) in basic_block.operations.iter().enumerate() {
                let Operation::ReturnBorrower { value, .. } = operation else {
                    continue;
                };
                let mut state = points[operation_index].clone();
                let loans = evaluate(value, &mut state);
                returns_borrower |= !loans.is_empty();
                for loan in loans {
                    collect_parameter_origins(
                        function,
                        definition,
                        loan,
                        &mut HashSet::new(),
                        &mut parameters,
                    );
                }
            }
        }
        let mut parameters = parameters.into_iter().collect::<Vec<_>>();
        parameters.sort_unstable();
        let receiver = parameters.first() == Some(&0) && definition.receiver.is_some();
        function.result_provenance = super::ResultProvenance {
            fresh_owned: !returns_borrower,
            parameters,
            receiver,
        };
    }
}

fn collect_parameter_origins(
    function: &Function,
    definition: &crate::hir::Function,
    loan: LoanId,
    visited: &mut HashSet<LoanId>,
    parameters: &mut HashSet<usize>,
) {
    if !visited.insert(loan) {
        return;
    }
    let loan = &function.loans[loan.0];
    if let Some(parameter) = definition
        .parameters
        .iter()
        .position(|local| *local == loan.origin.root)
    {
        parameters.insert(parameter);
    }
    for parent in &loan.parents {
        collect_parameter_origins(function, definition, *parent, visited, parameters);
    }
}

fn collect_reborrow_parents(
    value: &BorrowValue,
    state: &ProvenanceState,
    definitions: &mut [super::LoanDefinition],
) {
    match value {
        BorrowValue::Reborrow { loan, origin } => {
            definitions[loan.0]
                .parents
                .extend(contents_at(state, origin));
        }
        BorrowValue::Merge(values) => {
            for value in values {
                collect_reborrow_parents(value, state, definitions);
            }
        }
        BorrowValue::Fields(fields) => {
            for (_, value) in fields {
                collect_reborrow_parents(value, state, definitions);
            }
        }
        BorrowValue::Empty
        | BorrowValue::Loan(_)
        | BorrowValue::Place(_)
        | BorrowValue::MovePlace(_) => {}
    }
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
        Operation::Initialize { .. }
        | Operation::Invalidate { .. }
        | Operation::Suspend { .. }
        | Operation::Destroy { .. } => {}
    }
    require_ancestors(function, state);
}

fn require_ancestors(function: &Function, state: &mut RequirementState) {
    let mut work = state.loans.keys().copied().collect::<Vec<_>>();
    while let Some(child) = work.pop() {
        let Some(required_use) = state.loans.get(&child).cloned() else {
            continue;
        };
        for parent in &function.loans[child.0].parents {
            if !state.loans.contains_key(parent) {
                state.loans.insert(*parent, required_use.clone());
                work.push(*parent);
            }
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
        validate_suspensions(hir, *function_id, function, requirements)?;
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
            .with_code(super::diagnostics::INVALIDATED_LOAN)
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
                    InvalidationKind::Replace => {
                        format!("this operation replaces `{origin_name}`")
                    }
                },
            )
            .with_help("use or propagate the borrower before this invalidating operation, or reacquire it afterward"));
        }
    }
    Ok(())
}

fn validate_suspensions(
    hir: &PackageHir,
    function_id: FunctionId,
    function: &Function,
    requirements: &RequirementAnalysis,
) -> Result<(), FosterError> {
    for (block, definition) in function.blocks.iter().enumerate() {
        let Some(points) = &requirements.points[block] else {
            continue;
        };
        for (operation_index, operation) in definition.operations.iter().enumerate() {
            let Operation::Suspend { span } = operation else {
                continue;
            };
            for loan in points[operation_index + 1].loans.keys() {
                let loan = &function.loans[loan.0];
                let owner = &hir.locals[loan.origin.root];
                if owner.function != function_id {
                    let definition = &hir.functions[function_id];
                    return Err(FosterError::runtime(format!(
                        "in `{}.{}`: a loan required after suspension does not belong to the parked invocation",
                        hir.modules[definition.module].name, definition.name
                    ))
                    .with_code(super::diagnostics::UNSAFE_SUSPENSION)
                    .with_source_module(hir.modules[definition.module].name.clone())
                    .with_primary_label(span.clone(), "this suspension cannot preserve the loan's storage")
                    .with_label(loan.span.clone(), "loan is issued from non-frame storage here")
                    .with_help("move owned data into the invocation frame before awaiting"));
                }
            }
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
                        .with_code(super::diagnostics::SELF_BORROW)
                        .with_source_module(hir.modules[definition.module].name.clone())
                        .with_primary_label(span.clone(), "this value borrows from its destination")
                        .with_label(loan.span.clone(), "the stored borrower originates here")
                        .with_help("store the borrower outside its origin, or capture the required value by ownership"));
                    }
                }
                Operation::ReturnBorrower { value, kind, span } => {
                    let mut temporary = before.clone();
                    let loans = evaluate(value, &mut temporary);
                    let mut roots = HashSet::new();
                    for loan in loans {
                        collect_root_loans(function, loan, &mut HashSet::new(), &mut roots);
                    }
                    let mut returned = roots
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

fn collect_root_loans(
    function: &Function,
    loan: LoanId,
    visited: &mut HashSet<LoanId>,
    roots: &mut HashSet<LoanId>,
) {
    if !visited.insert(loan) {
        return;
    }
    let definition = &function.loans[loan.0];
    if definition.parents.is_empty() {
        roots.insert(loan);
    } else {
        for parent in &definition.parents {
            collect_root_loans(function, *parent, visited, roots);
        }
    }
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
        .with_code(super::diagnostics::BORROW_ESCAPE)
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
        .with_code(super::diagnostics::BORROW_ESCAPE)
        .with_source_module(module.clone())
        .with_primary_label(returned_at.clone(), "this return exposes a borrow without a named result group"));
    };
    if !crate::hir::queries::type_exposes_group(function.return_type.as_ref(), group) {
        return Err(FosterError::runtime(format!(
            "in `{module}.{}`: returned reference group `{group}` is absent from the result type",
            function.name
        ))
        .with_code(super::diagnostics::BORROW_ESCAPE)
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
            let (place, kind, span) = match operation {
                Operation::Invalidate { place, kind, span } => (place, *kind, span),
                Operation::Use {
                    place,
                    mode: super::UseMode::Move,
                    span,
                } => (place, InvalidationKind::Consume, span),
                Operation::StoreBorrower {
                    destination, span, ..
                } => (destination, InvalidationKind::Replace, span),
                Operation::Destroy { place, span } => (place, InvalidationKind::Consume, span),
                _ => continue,
            };
            let required_after = &points[operation_index + 1];
            if let Some((loan, required_use)) = required_after
                .loans
                .iter()
                .filter_map(|(id, use_span)| {
                    let loan = &function.loans[id.0];
                    let issued_here = loan.issued_at.block == block
                        && loan.issued_at.operation == operation_index;
                    let replacement_through_parameter =
                        kind == InvalidationKind::Replace && is_parameter_reborrow(function, loan);
                    (!issued_here
                        && !replacement_through_parameter
                        && invalidates(place, kind, &loan.origin))
                    .then_some((loan, use_span))
                })
                .min_by_key(|(loan, _)| loan.id)
            {
                return Some(InvalidationConflict {
                    loan: loan.clone(),
                    kind,
                    invalidated_at: span.clone(),
                    required_use: required_use.clone(),
                });
            }
        }
    }
    None
}

fn is_parameter_reborrow(function: &Function, loan: &super::LoanDefinition) -> bool {
    // Reference parameters begin with a synthetic `parameter = ref parameter` loan for the
    // caller's place. Writes through that parameter must preserve its own loan; reborrows derived
    // from the parameter remain subject to ordinary replacement invalidation.
    matches!(
        &function.blocks[loan.issued_at.block].operations[loan.issued_at.operation],
        Operation::StoreBorrower {
            destination,
            value: BorrowValue::Reborrow { loan: issued, origin },
            ..
        } if destination == origin && *issued == loan.id
    )
}

fn invalidates(invalidated: &Place, kind: InvalidationKind, origin: &Place) -> bool {
    places_overlap(invalidated, origin)
        && (matches!(kind, InvalidationKind::Consume | InvalidationKind::Replace)
            || origin
                .projections
                .iter()
                .any(|projection| matches!(projection, Projection::Index { .. })))
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
            } else if let Operation::Destroy { place, .. } = operation {
                replace(place, HashSet::new(), &mut state);
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
        BorrowValue::Reborrow { loan, .. } => HashSet::from([*loan]),
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
                projections: vec![Projection::Index {
                    expression: index,
                    constant: None,
                }],
            },
            issued_at: super::super::MirPoint {
                block: 0,
                operation: 0,
            },
            parents: HashSet::new(),
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
                    projections: vec![Projection::Index {
                        expression: index,
                        constant: None,
                    }],
                },
                issued_at: super::super::MirPoint {
                    block: 0,
                    operation: 0,
                },
                parents: HashSet::new(),
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
                    projections: vec![Projection::Index {
                        expression: index,
                        constant: None,
                    }],
                },
                issued_at: super::super::MirPoint {
                    block: 0,
                    operation: 0,
                },
                parents: HashSet::new(),
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

    fn model_function(events: &[super::super::model::Event]) -> Function {
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
        let operations = events
            .iter()
            .enumerate()
            .map(|(offset, event)| match event {
                super::super::model::Event::Issue => Operation::StoreBorrower {
                    destination: borrower_place.clone(),
                    value: BorrowValue::Loan(LoanId(0)),
                    span: offset..offset + 1,
                },
                super::super::model::Event::Use => Operation::Use {
                    place: borrower_place.clone(),
                    mode: super::super::UseMode::Read,
                    span: offset..offset + 1,
                },
                super::super::model::Event::Invalidate => Operation::Invalidate {
                    place: owner_place.clone(),
                    kind: InvalidationKind::Reshape,
                    span: offset..offset + 1,
                },
                super::super::model::Event::End => Operation::StoreBorrower {
                    destination: borrower_place.clone(),
                    value: BorrowValue::Empty,
                    span: offset..offset + 1,
                },
            })
            .collect();
        Function {
            entry: 0,
            blocks: vec![BasicBlock {
                operations,
                terminator: Terminator::Return,
            }],
            loans: vec![super::super::LoanDefinition {
                id: LoanId(0),
                origin: Place {
                    root: owner,
                    projections: vec![Projection::Index {
                        expression: index,
                        constant: None,
                    }],
                },
                issued_at: super::super::MirPoint {
                    block: 0,
                    operation: 0,
                },
                parents: HashSet::new(),
                span: 0..1,
            }],
            result_provenance: Default::default(),
        }
    }

    #[test]
    fn cfg_checker_matches_reference_model_for_bounded_event_histories() {
        use super::super::model::{self, Event};

        for encoded in 0usize..3usize.pow(6) {
            let mut value = encoded;
            let mut events = vec![Event::Issue];
            for _ in 0..6 {
                events.push(match value % 3 {
                    0 => Event::Use,
                    1 => Event::Invalidate,
                    _ => Event::End,
                });
                value /= 3;
            }
            let function = model_function(&events);
            let provenance = analyze_function(&function);
            let requirements = analyze_function_requirements(&function, &provenance);
            assert_eq!(
                find_conflict(&function, &requirements).is_none(),
                model::evaluate(&events).accepted,
                "reference-model disagreement for {events:?}"
            );
        }
    }

    #[test]
    fn harmless_cfg_splitting_preserves_the_ownership_decision() {
        use super::super::model::Event;
        let linear = model_function(&[Event::Issue, Event::Invalidate, Event::Use]);
        let mut split = model_function(&[Event::Issue, Event::Invalidate, Event::Use]);
        let operations = std::mem::take(&mut split.blocks[0].operations);
        split.blocks = operations
            .into_iter()
            .enumerate()
            .map(|(index, operation)| BasicBlock {
                operations: vec![operation],
                terminator: if index == 2 {
                    Terminator::Return
                } else {
                    Terminator::Goto(index + 1)
                },
            })
            .collect();
        for function in [&linear, &split] {
            let provenance = analyze_function(function);
            let requirements = analyze_function_requirements(function, &provenance);
            assert!(find_conflict(function, &requirements).is_some());
        }
    }

    #[test]
    fn ownership_decision_is_sensitive_to_invalidating_event_mutations() {
        use super::super::model::Event;
        for events in [
            vec![Event::Issue, Event::Invalidate, Event::Use],
            vec![Event::Issue, Event::Use],
            vec![Event::Issue, Event::Invalidate, Event::End, Event::Use],
        ] {
            let function = model_function(&events);
            let provenance = analyze_function(&function);
            let requirements = analyze_function_requirements(&function, &provenance);
            assert_eq!(
                find_conflict(&function, &requirements).is_none(),
                super::super::model::evaluate(&events).accepted
            );
        }
    }

    #[test]
    fn constant_indices_are_disjoint_while_dynamic_indices_overlap() {
        let local = crate::hir::LocalId::from_raw(la_arena::RawIdx::from_u32(0));
        let expression = |value| crate::hir::ExprId::from_raw(la_arena::RawIdx::from_u32(value));
        let place = |constant, value| Place {
            root: local,
            projections: vec![Projection::Index {
                expression: expression(value),
                constant,
            }],
        };
        assert!(!places_overlap(&place(Some(0), 0), &place(Some(1), 1)));
        assert!(places_overlap(&place(Some(0), 0), &place(Some(0), 1)));
        assert!(places_overlap(&place(Some(0), 0), &place(None, 2)));
    }
}
