use std::collections::{HashMap, HashSet};

use super::queries::{expression_uses_local, place_root, type_exposes_group};
use super::*;

#[derive(Debug, Clone, Copy)]
struct Loan {
    root: LocalId,
    projected_item: bool,
}

pub(super) fn check_loan_safety(
    hir: &PackageHir,
    types: &crate::types::TypeInformation,
) -> Result<(), FosterError> {
    for (_, function) in hir.functions.iter() {
        let mut loans = HashMap::<LocalId, Loan>::new();
        let mut provenance = HashMap::<LocalId, HashSet<LocalId>>::new();
        let mut invalid = HashMap::<LocalId, Loan>::new();

        for (index, statement) in function.body.iter().enumerate() {
            let expression = statement_expression(statement);
            if let Some((local, loan)) = invalid
                .iter()
                .find(|(local, _)| expression_uses_local(hir, expression, **local))
            {
                return Err(FosterError::runtime(format!(
                    "in `{}.{}`: borrowed value `{}` is no longer usable; its reference into `{}` was invalidated",
                    hir.modules[function.module].name,
                    function.name,
                    hir.locals[*local].name,
                    hir.locals[loan.root].name
                )));
            }

            if matches!(statement, Stmt::Return { .. })
                || (index + 1 == function.body.len()
                    && matches!(statement, Stmt::Expr(_) | Stmt::Bind { .. }))
            {
                check_escape(hir, function, expression, &loans)?;
            }

            reject_self_origin_storage(hir, types, function, statement, &provenance)?;

            if let Stmt::Bind { local, value } = statement
                && let Some(loan) = expression_loan(hir, *value, &loans)
            {
                loans.insert(*local, loan);
            }

            match statement {
                Stmt::Bind { local, value } | Stmt::Assign { local, value } => {
                    let roots = expression_borrow_roots(hir, types, *value, &provenance);
                    if roots.is_empty() {
                        provenance.remove(local);
                    } else {
                        provenance.insert(*local, roots);
                    }
                }
                _ => {}
            }

            let mut reshaped = HashSet::new();
            let mut consumed = HashSet::new();
            collect_invalidations(hir, expression, &mut reshaped, &mut consumed);
            for (local, loan) in &loans {
                if (loan.projected_item && reshaped.contains(&loan.root))
                    || consumed.contains(&loan.root)
                {
                    invalid.insert(*local, *loan);
                }
            }
        }
    }
    Ok(())
}

fn reject_self_origin_storage(
    hir: &PackageHir,
    types: &crate::types::TypeInformation,
    function: &Function,
    statement: &Stmt,
    provenance: &HashMap<LocalId, HashSet<LocalId>>,
) -> Result<(), FosterError> {
    let (origin, value) = match statement {
        Stmt::Set { place, value } => (place_root(hir, *place), *value),
        Stmt::Assign { local, value } => (Some(*local), *value),
        _ => return Ok(()),
    };
    let Some(origin) = origin else {
        return Ok(());
    };
    if expression_borrow_roots(hir, types, value, provenance).contains(&origin) {
        return Err(FosterError::runtime(format!(
            "in `{}.{}`: cannot store a value borrowing `{}` into its own origin",
            hir.modules[function.module].name, function.name, hir.locals[origin].name
        )));
    }
    Ok(())
}

fn expression_borrow_roots(
    hir: &PackageHir,
    types: &crate::types::TypeInformation,
    expression: ExprId,
    provenance: &HashMap<LocalId, HashSet<LocalId>>,
) -> HashSet<LocalId> {
    if types.expression_type(expression).is_some_and(|ty| {
        matches!(
            types.types[ty],
            crate::types::Type::Unit
                | crate::types::Type::Bool
                | crate::types::Type::Int
                | crate::types::Type::Float
                | crate::types::Type::CodePoint
                | crate::types::Type::Byte
                | crate::types::Type::Symbol
                | crate::types::Type::Module(_)
        )
    }) {
        return HashSet::new();
    }
    let mut roots = HashSet::new();
    collect_borrow_roots(hir, expression, provenance, &mut roots);
    roots
}

fn collect_borrow_roots(
    hir: &PackageHir,
    expression: ExprId,
    provenance: &HashMap<LocalId, HashSet<LocalId>>,
    roots: &mut HashSet<LocalId>,
) {
    match &hir.expressions[expression] {
        Expr::Reference(place) => {
            if let Some(root) = place_root(hir, *place) {
                roots.extend(
                    provenance
                        .get(&root)
                        .cloned()
                        .unwrap_or_else(|| HashSet::from([root])),
                );
            }
        }
        Expr::Closure { captures, .. } => {
            for capture in captures
                .iter()
                .filter(|capture| capture.mode == CaptureMode::Ref)
            {
                roots.extend(
                    provenance
                        .get(&capture.local)
                        .cloned()
                        .unwrap_or_else(|| HashSet::from([capture.local])),
                );
            }
        }
        Expr::Name(ResolvedName::Local(local)) => {
            roots.extend(provenance.get(local).into_iter().flatten().copied());
        }
        Expr::List(values) => {
            for value in values {
                collect_borrow_roots(hir, *value, provenance, roots);
            }
        }
        Expr::Call { callee, arguments } => {
            collect_borrow_roots(hir, *callee, provenance, roots);
            for argument in arguments {
                collect_borrow_roots(hir, *argument, provenance, roots);
            }
        }
        Expr::Member { object, .. }
        | Expr::MoveOut(object)
        | Expr::Remote(object)
        | Expr::Await(object)
        | Expr::Unary {
            operand: object, ..
        } => collect_borrow_roots(hir, *object, provenance, roots),
        Expr::Index { object, index }
        | Expr::Binary {
            left: object,
            right: index,
            ..
        } => {
            collect_borrow_roots(hir, *object, provenance, roots);
            collect_borrow_roots(hir, *index, provenance, roots);
        }
        Expr::Record { fields, .. } => {
            for (_, value) in fields {
                collect_borrow_roots(hir, *value, provenance, roots);
            }
        }
        Expr::Branch { subject, arms } => {
            if let Some(subject) = subject {
                collect_borrow_roots(hir, *subject, provenance, roots);
            }
            for arm in arms {
                if let BranchTest::Condition(condition) = arm.test {
                    collect_borrow_roots(hir, condition, provenance, roots);
                }
                collect_borrow_roots(hir, arm.value, provenance, roots);
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
        Expr::Name(ResolvedName::Local(local)) => loans.get(&local).copied(),
        _ => None,
    }
}

fn place_loan(hir: &PackageHir, expression: ExprId) -> Option<Loan> {
    match hir.expressions[expression] {
        Expr::Name(ResolvedName::Local(root)) => Some(Loan {
            root,
            projected_item: false,
        }),
        Expr::Member { object, .. } => place_loan(hir, object),
        Expr::Index { object, .. } => place_loan(hir, object).map(|mut loan| {
            loan.projected_item = true;
            loan
        }),
        _ => None,
    }
}

fn check_escape(
    hir: &PackageHir,
    function: &Function,
    expression: ExprId,
    loans: &HashMap<LocalId, Loan>,
) -> Result<(), FosterError> {
    let Some(loan) = expression_loan(hir, expression, loans) else {
        return Ok(());
    };
    let Some(parameter) = function
        .parameters
        .iter()
        .position(|parameter| *parameter == loan.root)
    else {
        return Err(FosterError::runtime(format!(
            "in `{}.{}`: returned reference borrows local `{}`",
            hir.modules[function.module].name, function.name, hir.locals[loan.root].name
        )));
    };
    let Some(ast::TypeExpr::Reference { group, .. }) = function.parameter_types[parameter].as_ref()
    else {
        return Err(FosterError::runtime(format!(
            "in `{}.{}`: returned reference borrows parameter `{}` without an exposed group",
            hir.modules[function.module].name, function.name, hir.locals[loan.root].name
        )));
    };
    if !type_exposes_group(function.return_type.as_ref(), group) {
        return Err(FosterError::runtime(format!(
            "in `{}.{}`: returned reference group `{group}` is absent from the result type",
            hir.modules[function.module].name, function.name
        )));
    }
    Ok(())
}

fn collect_invalidations(
    hir: &PackageHir,
    expression: ExprId,
    reshaped: &mut HashSet<LocalId>,
    consumed: &mut HashSet<LocalId>,
) {
    match &hir.expressions[expression] {
        Expr::MoveOut(place) => {
            if let Some(root) = place_root(hir, *place) {
                consumed.insert(root);
            }
            collect_invalidations(hir, *place, reshaped, consumed);
        }
        Expr::Call { callee, arguments } => {
            if let Expr::Member { object, name } = &hir.expressions[*callee]
                && matches!(
                    name.as_str(),
                    "push" | "extend" | "clear" | "truncate" | "reserve"
                )
                && let Some(root) = place_root(hir, *object)
            {
                reshaped.insert(root);
            }
            collect_invalidations(hir, *callee, reshaped, consumed);
            for argument in arguments {
                collect_invalidations(hir, *argument, reshaped, consumed);
            }
        }
        Expr::List(values) => {
            for value in values {
                collect_invalidations(hir, *value, reshaped, consumed);
            }
        }
        Expr::Member { object, .. }
        | Expr::Reference(object)
        | Expr::Remote(object)
        | Expr::Await(object)
        | Expr::Unary {
            operand: object, ..
        } => {
            collect_invalidations(hir, *object, reshaped, consumed);
        }
        Expr::Index { object, index }
        | Expr::Binary {
            left: object,
            right: index,
            ..
        } => {
            collect_invalidations(hir, *object, reshaped, consumed);
            collect_invalidations(hir, *index, reshaped, consumed);
        }
        Expr::Record { fields, .. } => {
            for (_, value) in fields {
                collect_invalidations(hir, *value, reshaped, consumed);
            }
        }
        Expr::Branch { subject, arms } => {
            if let Some(subject) = subject {
                collect_invalidations(hir, *subject, reshaped, consumed);
            }
            for arm in arms {
                if let BranchTest::Condition(condition) = arm.test {
                    collect_invalidations(hir, condition, reshaped, consumed);
                }
                collect_invalidations(hir, arm.value, reshaped, consumed);
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
