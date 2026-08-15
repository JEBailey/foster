use crate::ast;

use super::{BranchTest, Expr, ExprId, LocalId, PackageHir, Place, Projection, ResolvedName};

pub(crate) fn expression_place(hir: &PackageHir, expression: ExprId) -> Option<Place> {
    match &hir.expressions[expression] {
        Expr::Name(ResolvedName::Local(root)) => Some(Place {
            root: *root,
            projections: Vec::new(),
        }),
        Expr::Member { object, name } => {
            let mut place = expression_place(hir, *object)?;
            place.projections.push(Projection::Field(name.clone()));
            Some(place)
        }
        Expr::Index { object, index } => {
            let mut place = expression_place(hir, *object)?;
            place.projections.push(Projection::Index(*index));
            Some(place)
        }
        Expr::Reference(object) => {
            let mut place = expression_place(hir, *object)?;
            place.projections.push(Projection::Dereference);
            Some(place)
        }
        _ => None,
    }
}

pub(super) fn place_root(hir: &PackageHir, expression: ExprId) -> Option<LocalId> {
    expression_place(hir, expression).map(|place| place.root)
}

pub(super) fn type_exposes_group(ty: Option<&ast::TypeExpr>, group: &str) -> bool {
    match ty {
        Some(ast::TypeExpr::Reference {
            group: found,
            value,
        }) => found == group || type_exposes_group(Some(value), group),
        Some(ast::TypeExpr::Named(_, arguments)) => arguments
            .iter()
            .any(|argument| type_exposes_group(Some(argument), group)),
        Some(ast::TypeExpr::Intersection(members)) => members
            .iter()
            .any(|member| type_exposes_group(Some(member), group)),
        Some(ast::TypeExpr::Function {
            parameters,
            result,
            effects,
            ..
        }) => {
            parameters
                .iter()
                .any(|parameter| type_exposes_group(Some(parameter), group))
                || type_exposes_group(Some(result), group)
                || effects.iter().any(|effect| effect.target.root == group)
        }
        None => false,
    }
}

pub(super) fn expression_uses_local(hir: &PackageHir, expression: ExprId, local: LocalId) -> bool {
    match &hir.expressions[expression] {
        Expr::Name(ResolvedName::Local(found)) => *found == local,
        Expr::List(values) => values
            .iter()
            .any(|value| expression_uses_local(hir, *value, local)),
        Expr::Call { callee, arguments } => {
            expression_uses_local(hir, *callee, local)
                || arguments
                    .iter()
                    .any(|argument| expression_uses_local(hir, *argument, local))
        }
        Expr::Member { object, .. }
        | Expr::Reference(object)
        | Expr::MoveOut(object)
        | Expr::Remote(object)
        | Expr::Await(object)
        | Expr::Unary {
            operand: object, ..
        } => expression_uses_local(hir, *object, local),
        Expr::Index { object, index }
        | Expr::Binary {
            left: object,
            right: index,
            ..
        } => {
            expression_uses_local(hir, *object, local)
                || expression_uses_local(hir, *index, local)
        }
        Expr::Record { fields, .. } => fields
            .iter()
            .any(|(_, value)| expression_uses_local(hir, *value, local)),
        Expr::Branch { subject, arms } => {
            subject.is_some_and(|subject| expression_uses_local(hir, subject, local))
                || arms.iter().any(|arm| {
                    matches!(arm.test, BranchTest::Condition(condition) if expression_uses_local(hir, condition, local))
                        || expression_uses_local(hir, arm.value, local)
                })
        }
        Expr::Closure { .. }
        | Expr::Unit
        | Expr::Bool(_)
        | Expr::Integer(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::CodePoint(_)
        | Expr::Symbol(_)
        | Expr::Name(_) => false,
    }
}
