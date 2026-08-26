use crate::ast;

use super::{Expr, ExprId, LocalId, PackageHir, Place, Projection, ResolvedName};

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
            let constant = match hir.expressions[*index] {
                Expr::Integer(value) => Some(value),
                _ => None,
            };
            place.projections.push(Projection::Index {
                expression: *index,
                constant,
            });
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

pub(crate) fn place_contains(parent: &Place, child: &Place) -> bool {
    parent.root == child.root
        && parent.projections.len() <= child.projections.len()
        && parent
            .projections
            .iter()
            .zip(&child.projections)
            .all(|(left, right)| projections_equal(left, right))
}

pub(crate) fn places_overlap(left: &Place, right: &Place) -> bool {
    if left.root != right.root {
        return false;
    }
    for (left, right) in left.projections.iter().zip(&right.projections) {
        match (left, right) {
            (Projection::Field(left), Projection::Field(right)) if left != right => return false,
            (
                Projection::Index {
                    constant: Some(left),
                    ..
                },
                Projection::Index {
                    constant: Some(right),
                    ..
                },
            ) if left != right => return false,
            (Projection::Field(_), Projection::Field(_))
            | (Projection::Index { .. }, Projection::Index { .. })
            | (Projection::Dereference, Projection::Dereference) => {}
            _ => return true,
        }
    }
    true
}

fn projections_equal(left: &Projection, right: &Projection) -> bool {
    match (left, right) {
        (Projection::Field(left), Projection::Field(right)) => left == right,
        (
            Projection::Index {
                expression: left_expression,
                constant: left_constant,
            },
            Projection::Index {
                expression: right_expression,
                constant: right_constant,
            },
        ) => match (left_constant, right_constant) {
            (Some(left), Some(right)) => left == right,
            _ => left_expression == right_expression,
        },
        (Projection::Dereference, Projection::Dereference) => true,
        _ => false,
    }
}

pub(crate) fn type_exposes_group(ty: Option<&ast::TypeExpr>, group: &str) -> bool {
    match ty {
        Some(ast::TypeExpr::Unit) => false,
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
    struct LocalUse {
        target: LocalId,
        found: bool,
    }

    impl super::visit::Visitor for LocalUse {
        fn visit_local_use(&mut self, local: LocalId) {
            self.found |= local == self.target;
        }
    }

    let mut visitor = LocalUse {
        target: local,
        found: false,
    };
    super::visit::Visitor::visit_expression(&mut visitor, hir, expression);
    visitor.found
}
