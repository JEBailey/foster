use std::collections::{HashMap, HashSet};

use super::queries::{expression_uses_local, place_root, type_exposes_group};
use super::*;

#[derive(Debug, Clone)]
struct Loan {
    place: Place,
}

#[derive(Debug, Clone)]
struct InvalidBorrow {
    origin: Place,
    invalidated_at: std::ops::Range<usize>,
}

pub(super) fn check_loan_safety(
    hir: &PackageHir,
    types: &crate::types::TypeInformation,
) -> Result<(), FosterError> {
    for (_, function) in hir.functions.iter() {
        let mut loans = HashMap::<LocalId, Loan>::new();
        let mut provenance = HashMap::<LocalId, HashSet<Place>>::new();
        let mut invalid = HashMap::<LocalId, InvalidBorrow>::new();

        for (index, statement) in function.body.iter().enumerate() {
            let expression = statement_expression(statement);
            if let Stmt::Return {
                guard: Some(guard), ..
            } = statement
            {
                reject_invalid_uses(hir, types, function, *guard, &invalid)?;
                invalidate_expression(hir, types, *guard, &provenance, &mut invalid);
            }
            reject_invalid_uses(hir, types, function, expression, &invalid)?;

            if matches!(statement, Stmt::Return { .. })
                || (index + 1 == function.body.len()
                    && matches!(statement, Stmt::Expr(_) | Stmt::Bind { .. }))
            {
                check_escape(hir, types, function, expression, &loans, &provenance)?;
            }

            reject_self_origin_storage(hir, types, function, statement, &provenance)?;

            if let Stmt::Bind { local, value } = statement
                && let Some(loan) = expression_loan(hir, *value, &loans)
            {
                loans.insert(*local, loan);
            }

            match statement {
                Stmt::Bind { local, value } | Stmt::Assign { local, value } => {
                    let places = expression_borrow_places(hir, types, *value, &provenance);
                    if places.is_empty() {
                        provenance.remove(local);
                    } else {
                        provenance.insert(*local, places);
                    }
                }
                _ => {}
            }

            // A return value executes only on the path that leaves the function. A
            // guarded return's guard was applied above because its effects also occur
            // on the continuation path.
            if !matches!(statement, Stmt::Return { .. }) {
                invalidate_expression(hir, types, expression, &provenance, &mut invalid);
            }
        }
    }
    Ok(())
}

fn reject_invalid_uses(
    hir: &PackageHir,
    types: &crate::types::TypeInformation,
    function: &Function,
    expression: ExprId,
    invalid: &HashMap<LocalId, InvalidBorrow>,
) -> Result<(), FosterError> {
    let Some((local, invalidation)) = invalid
        .iter()
        .find(|(local, _)| expression_uses_local(hir, expression, **local))
    else {
        return Ok(());
    };
    if expression_calls_local(hir, expression, *local)
        && matches!(
            types.locals.get(local).map(|ty| &types.types[*ty]),
            Some(crate::types::Type::Function(_))
        )
    {
        return Err(FosterError::runtime(format!(
            "in `{}.{}`: closure `{}` is no longer callable; structural mutation invalidated its captured reference into `{}`",
            hir.modules[function.module].name,
            function.name,
            hir.locals[*local].name,
            hir.locals[invalidation.origin.root].name
        ))
        .with_code("E0401")
        .with_source_module(hir.modules[function.module].name.clone())
        .with_primary_label(expression_span(hir, expression), "this call uses an invalidated captured reference")
        .with_label(hir.locals[*local].span.clone(), "closure containing the borrow was created here")
        .with_label(invalidation.invalidated_at.clone(), format!("this operation reshaped `{}`", hir.locals[invalidation.origin.root].name))
        .with_help(format!("recreate `{}` after reshaping `{}`", hir.locals[*local].name, hir.locals[invalidation.origin.root].name)));
    }
    Err(FosterError::runtime(format!(
        "in `{}.{}`: borrowed value `{}` is no longer usable; its reference into `{}` was invalidated",
        hir.modules[function.module].name,
        function.name,
        hir.locals[*local].name,
        hir.locals[invalidation.origin.root].name
    ))
    .with_code("E0401")
    .with_source_module(hir.modules[function.module].name.clone())
    .with_primary_label(expression_span(hir, expression), format!("invalidated borrow `{}` is used here", hir.locals[*local].name))
    .with_label(hir.locals[*local].span.clone(), "borrow was stored here")
    .with_label(invalidation.invalidated_at.clone(), format!("this operation reshaped `{}`", hir.locals[invalidation.origin.root].name))
    .with_help(format!("use `{}` before reshaping `{}`, or reacquire the reference afterward", hir.locals[*local].name, hir.locals[invalidation.origin.root].name)))
}

fn invalidate_expression(
    hir: &PackageHir,
    types: &crate::types::TypeInformation,
    expression: ExprId,
    provenance: &HashMap<LocalId, HashSet<Place>>,
    invalid: &mut HashMap<LocalId, InvalidBorrow>,
) {
    let mut reshaped = HashSet::new();
    let mut consumed = HashSet::new();
    collect_invalidations(hir, types, expression, &mut reshaped, &mut consumed);
    for (local, origins) in provenance {
        if let Some(origin) = origins.iter().find(|origin| {
            (origin
                .projections
                .iter()
                .any(|projection| matches!(projection, Projection::Index(_)))
                && reshaped.iter().any(|place| places_overlap(place, origin)))
                || consumed.iter().any(|place| places_overlap(place, origin))
        }) {
            invalid.insert(
                *local,
                InvalidBorrow {
                    origin: origin.clone(),
                    invalidated_at: expression_span(hir, expression),
                },
            );
        }
    }
}

fn expression_span(hir: &PackageHir, expression: ExprId) -> std::ops::Range<usize> {
    hir.expression_spans
        .get(&expression)
        .cloned()
        .unwrap_or(0..0)
}

fn expression_calls_local(hir: &PackageHir, expression: ExprId, local: LocalId) -> bool {
    match &hir.expressions[expression] {
        Expr::Call { callee, arguments } => {
            matches!(hir.expressions[*callee], Expr::Name(ResolvedName::Local(found)) if found == local)
                || expression_calls_local(hir, *callee, local)
                || arguments
                    .iter()
                    .any(|argument| expression_calls_local(hir, *argument, local))
        }
        Expr::List(values) => values
            .iter()
            .any(|value| expression_calls_local(hir, *value, local)),
        Expr::Member { object, .. }
        | Expr::Reference(object)
        | Expr::MoveOut(object)
        | Expr::Remote(object)
        | Expr::Await(object)
        | Expr::Unary { operand: object, .. } => expression_calls_local(hir, *object, local),
        Expr::Index { object, index }
        | Expr::Binary {
            left: object,
            right: index,
            ..
        } => {
            expression_calls_local(hir, *object, local)
                || expression_calls_local(hir, *index, local)
        }
        Expr::Record { fields, .. } => fields
            .iter()
            .any(|(_, value)| expression_calls_local(hir, *value, local)),
        Expr::Branch { subject, arms } => {
            subject.is_some_and(|subject| expression_calls_local(hir, subject, local))
                || arms.iter().any(|arm| {
                    matches!(arm.test, BranchTest::Condition(condition) if expression_calls_local(hir, condition, local))
                        || expression_calls_local(hir, arm.value, local)
                })
        }
        _ => false,
    }
}

fn reject_self_origin_storage(
    hir: &PackageHir,
    types: &crate::types::TypeInformation,
    function: &Function,
    statement: &Stmt,
    provenance: &HashMap<LocalId, HashSet<Place>>,
) -> Result<(), FosterError> {
    let (origin, value) = match statement {
        Stmt::Set { place, value } => (place_root(hir, *place), *value),
        Stmt::Assign { local, value } => (Some(*local), *value),
        _ => return Ok(()),
    };
    let Some(origin) = origin else {
        return Ok(());
    };
    let origin = Place {
        root: origin,
        projections: Vec::new(),
    };
    if expression_borrow_places(hir, types, value, provenance)
        .iter()
        .any(|borrowed| places_overlap(&origin, borrowed))
    {
        return Err(FosterError::runtime(format!(
            "in `{}.{}`: cannot store a value borrowing `{}` into its own origin",
            hir.modules[function.module].name, function.name, hir.locals[origin.root].name
        ))
        .with_code("E0403")
        .with_source_module(hir.modules[function.module].name.clone())
        .with_primary_label(
            expression_span(hir, value),
            "this value borrows from its destination",
        )
        .with_label(
            hir.locals[origin.root].span.clone(),
            "the destination owns the borrowed place",
        )
        .with_help(
            "store the borrower outside its origin, or capture the required value by ownership",
        ));
    }
    Ok(())
}

fn expression_borrow_places(
    hir: &PackageHir,
    types: &crate::types::TypeInformation,
    expression: ExprId,
    provenance: &HashMap<LocalId, HashSet<Place>>,
) -> HashSet<Place> {
    if types.expression_type(expression).is_some_and(|ty| {
        types.is_copy(ty) || matches!(types.types[ty], crate::types::Type::Module(_))
    }) {
        return HashSet::new();
    }
    let mut places = HashSet::new();
    collect_borrow_places(hir, expression, provenance, &mut places);
    places
}

fn collect_borrow_places(
    hir: &PackageHir,
    expression: ExprId,
    provenance: &HashMap<LocalId, HashSet<Place>>,
    places: &mut HashSet<Place>,
) {
    match &hir.expressions[expression] {
        Expr::Reference(expression) => {
            if let Some(borrowed) = queries::expression_place(hir, *expression) {
                if let Some(origins) = provenance.get(&borrowed.root) {
                    for origin in origins {
                        let mut composed = origin.clone();
                        composed.projections.extend(borrowed.projections.clone());
                        places.insert(composed);
                    }
                } else {
                    places.insert(borrowed);
                }
            }
        }
        Expr::Closure { captures, .. } => {
            for capture in captures
                .iter()
                .filter(|capture| capture.mode == CaptureMode::Ref)
            {
                places.extend(provenance.get(&capture.local).cloned().unwrap_or_else(|| {
                    HashSet::from([Place {
                        root: capture.local,
                        projections: Vec::new(),
                    }])
                }));
            }
        }
        Expr::Name(ResolvedName::Local(local)) => {
            places.extend(provenance.get(local).into_iter().flatten().cloned());
        }
        Expr::List(values) => {
            for value in values {
                collect_borrow_places(hir, *value, provenance, places);
            }
        }
        Expr::Call { callee, arguments } => {
            collect_borrow_places(hir, *callee, provenance, places);
            for argument in arguments {
                collect_borrow_places(hir, *argument, provenance, places);
            }
        }
        Expr::Member { object, .. }
        | Expr::MoveOut(object)
        | Expr::Remote(object)
        | Expr::Await(object)
        | Expr::Unary {
            operand: object, ..
        } => collect_borrow_places(hir, *object, provenance, places),
        Expr::Index { object, index }
        | Expr::Binary {
            left: object,
            right: index,
            ..
        } => {
            collect_borrow_places(hir, *object, provenance, places);
            collect_borrow_places(hir, *index, provenance, places);
        }
        Expr::Record { fields, .. } => {
            for (_, value) in fields {
                collect_borrow_places(hir, *value, provenance, places);
            }
        }
        Expr::Branch { subject, arms } => {
            if let Some(subject) = subject {
                collect_borrow_places(hir, *subject, provenance, places);
            }
            for arm in arms {
                if let BranchTest::Condition(condition) = arm.test {
                    collect_borrow_places(hir, condition, provenance, places);
                }
                collect_borrow_places(hir, arm.value, provenance, places);
            }
        }
        Expr::Unit
        | Expr::Bool(_)
        | Expr::Integer(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::CodePoint(_)
        | Expr::Symbol(_)
        | Expr::Name(_) => {}
    }
}

fn statement_expression(statement: &Stmt) -> ExprId {
    match statement {
        Stmt::Return { value, .. }
        | Stmt::Bind { value, .. }
        | Stmt::Assign { value, .. }
        | Stmt::Expr(value)
        | Stmt::Set { value, .. } => *value,
    }
}

fn expression_loan(
    hir: &PackageHir,
    expression: ExprId,
    loans: &HashMap<LocalId, Loan>,
) -> Option<Loan> {
    match hir.expressions[expression] {
        Expr::Reference(place) => place_loan(hir, place),
        Expr::Name(ResolvedName::Local(local)) => loans.get(&local).cloned(),
        _ => None,
    }
}

fn place_loan(hir: &PackageHir, expression: ExprId) -> Option<Loan> {
    queries::expression_place(hir, expression).map(|place| Loan { place })
}

fn check_escape(
    hir: &PackageHir,
    types: &crate::types::TypeInformation,
    function: &Function,
    expression: ExprId,
    loans: &HashMap<LocalId, Loan>,
    provenance: &HashMap<LocalId, HashSet<Place>>,
) -> Result<(), FosterError> {
    let mut returned = expression_borrow_places(hir, types, expression, provenance);
    if types
        .expression_type(expression)
        .is_some_and(|ty| matches!(types.types[ty], crate::types::Type::Reference { .. }))
        && let Some(loan) = expression_loan(hir, expression, loans)
    {
        returned.insert(loan.place);
    }
    for place in returned {
        check_place_escape(hir, function, &place, expression_span(hir, expression))?;
    }
    Ok(())
}

fn check_place_escape(
    hir: &PackageHir,
    function: &Function,
    place: &Place,
    returned_at: std::ops::Range<usize>,
) -> Result<(), FosterError> {
    let Some(parameter) = function
        .parameters
        .iter()
        .position(|parameter| *parameter == place.root)
    else {
        return Err(FosterError::runtime(format!(
            "in `{}.{}`: returned reference borrows local `{}`",
            hir.modules[function.module].name, function.name, hir.locals[place.root].name
        ))
        .with_code("E0402")
        .with_source_module(hir.modules[function.module].name.clone())
        .with_primary_label(returned_at, "this returned value contains a reference to frame-local storage")
        .with_label(hir.locals[place.root].span.clone(), "borrowed local is declared here")
        .with_help("return an owned value, or borrow from a reference parameter whose group appears in the result type"));
    };
    let Some(ast::TypeExpr::Reference { group, .. }) = function.parameter_types[parameter].as_ref()
    else {
        return Err(FosterError::runtime(format!(
            "in `{}.{}`: returned reference borrows parameter `{}` without an exposed group",
            hir.modules[function.module].name, function.name, hir.locals[place.root].name
        ))
        .with_code("E0402")
        .with_source_module(hir.modules[function.module].name.clone())
        .with_primary_label(
            returned_at,
            "this return exposes a borrow without a named result group",
        )
        .with_label(
            hir.locals[place.root].span.clone(),
            "parameter is not declared as a grouped reference",
        )
        .with_help(
            "declare the parameter as `ref[group] T` and expose `group` in the result type",
        ));
    };
    if !type_exposes_group(function.return_type.as_ref(), group) {
        return Err(FosterError::runtime(format!(
            "in `{}.{}`: returned reference group `{group}` is absent from the result type",
            hir.modules[function.module].name, function.name
        ))
        .with_code("E0402")
        .with_source_module(hir.modules[function.module].name.clone())
        .with_primary_label(
            returned_at,
            format!("returned borrow belongs to group `{group}`"),
        )
        .with_label(
            function.span.clone(),
            "function result contract does not expose this group",
        )
        .with_help(format!(
            "include group `{group}` in the declared result type"
        )));
    }
    Ok(())
}

fn collect_invalidations(
    hir: &PackageHir,
    types: &crate::types::TypeInformation,
    expression: ExprId,
    reshaped: &mut HashSet<Place>,
    consumed: &mut HashSet<Place>,
) {
    match &hir.expressions[expression] {
        Expr::MoveOut(place) => {
            if let Some(place) = queries::expression_place(hir, *place) {
                consumed.insert(place);
            }
            collect_invalidations(hir, types, *place, reshaped, consumed);
        }
        Expr::Call { callee, arguments } => {
            collect_call_effects(hir, types, *callee, arguments, reshaped, consumed);
            collect_invalidations(hir, types, *callee, reshaped, consumed);
            for argument in arguments {
                collect_invalidations(hir, types, *argument, reshaped, consumed);
            }
        }
        Expr::List(values) => {
            for value in values {
                collect_invalidations(hir, types, *value, reshaped, consumed);
            }
        }
        Expr::Member { object, .. }
        | Expr::Reference(object)
        | Expr::Remote(object)
        | Expr::Await(object)
        | Expr::Unary {
            operand: object, ..
        } => {
            collect_invalidations(hir, types, *object, reshaped, consumed);
        }
        Expr::Index { object, index }
        | Expr::Binary {
            left: object,
            right: index,
            ..
        } => {
            collect_invalidations(hir, types, *object, reshaped, consumed);
            collect_invalidations(hir, types, *index, reshaped, consumed);
        }
        Expr::Record { fields, .. } => {
            for (_, value) in fields {
                collect_invalidations(hir, types, *value, reshaped, consumed);
            }
        }
        Expr::Branch { subject, arms } => {
            if let Some(subject) = subject {
                collect_invalidations(hir, types, *subject, reshaped, consumed);
            }
            for arm in arms {
                if let BranchTest::Condition(condition) = arm.test {
                    collect_invalidations(hir, types, condition, reshaped, consumed);
                }
                collect_invalidations(hir, types, arm.value, reshaped, consumed);
            }
        }
        Expr::Closure { .. }
        | Expr::Unit
        | Expr::Bool(_)
        | Expr::Integer(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::CodePoint(_)
        | Expr::Symbol(_)
        | Expr::Name(_) => {}
    }
}

fn collect_call_effects(
    hir: &PackageHir,
    types: &crate::types::TypeInformation,
    callee: ExprId,
    arguments: &[ExprId],
    reshaped: &mut HashSet<Place>,
    consumed: &mut HashSet<Place>,
) {
    let effects = types
        .expression_type(callee)
        .and_then(|ty| match &types.types[ty] {
            crate::types::Type::Function(function) => Some(function.effects.as_slice()),
            _ => None,
        });
    let Some(effects) = effects else { return };
    let receiver = match &hir.expressions[callee] {
        Expr::Member { object, .. } => Some(*object),
        _ => None,
    };
    let function = match hir.expressions[callee] {
        Expr::Name(ResolvedName::Function(function)) => Some(function),
        _ => types.extension_methods.get(&callee).copied(),
    };
    for effect in effects {
        if !matches!(
            effect.kind,
            ast::EffectKind::Reshape | ast::EffectKind::Consume
        ) {
            continue;
        }
        let target = if effect.target.root == "self" {
            receiver.and_then(|receiver| queries::expression_place(hir, receiver))
        } else {
            function.and_then(|function| {
                let definition = &hir.functions[function];
                definition
                    .parameters
                    .iter()
                    .enumerate()
                    .find(|(index, parameter)| {
                        hir.locals[**parameter].name == effect.target.root
                            || matches!(
                                definition.parameter_types[*index].as_ref(),
                                Some(ast::TypeExpr::Reference { group, .. })
                                    if *group == effect.target.root
                            )
                    })
                    .and_then(|(index, _)| {
                        let argument = index.checked_sub(usize::from(receiver.is_some()))?;
                        arguments
                            .get(argument)
                            .and_then(|argument| queries::expression_place(hir, *argument))
                    })
            })
        };
        let Some(target) = target else { continue };
        match effect.kind {
            ast::EffectKind::Reshape => {
                reshaped.insert(target);
            }
            ast::EffectKind::Consume => {
                consumed.insert(target);
            }
            _ => {}
        }
    }
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
