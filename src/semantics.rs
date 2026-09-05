//! Shared semantic classification for expressions and resolved members.
//!
//! These categories describe source meaning, independently of whether a backend
//! happens to represent a value with an address or shared allocation.

use std::collections::HashMap;

use crate::hir::{self, ExprId, Place, Projection, ResolvedName};

/// The role an expression can play in the source language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpressionCategory {
    /// Stable program storage that can be read, borrowed, moved, or replaced.
    Place,
    /// A computed result, including values that use temporary storage.
    Value,
}

/// How a resolved member participates in place and ownership semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberKind {
    /// A declared stored field. It is a place when rooted in a place.
    StoredPlace,
    /// A computed value whose ordinary result type controls copy or ownership.
    ComputedValue,
    /// A callable member. Merely selecting it does not invoke it.
    Method,
}

/// Classifies an expression using the resolved member metadata produced by type checking.
pub fn expression_category(
    hir: &hir::PackageHir,
    members: &HashMap<ExprId, MemberKind>,
    expression: ExprId,
) -> ExpressionCategory {
    if expression_place(hir, members, expression).is_some() {
        ExpressionCategory::Place
    } else {
        ExpressionCategory::Value
    }
}

/// Returns the canonical place denoted by an expression, if it denotes storage.
pub fn expression_place(
    hir: &hir::PackageHir,
    members: &HashMap<ExprId, MemberKind>,
    expression: ExprId,
) -> Option<Place> {
    match &hir.expressions[expression] {
        hir::Expr::Name(ResolvedName::Local(root)) => Some(Place {
            root: *root,
            projections: Vec::new(),
        }),
        hir::Expr::Member { object, name }
            if members.get(&expression) == Some(&MemberKind::StoredPlace) =>
        {
            let mut place = place_base(hir, members, *object)?;
            place.projections.push(Projection::Field(name.clone()));
            Some(place)
        }
        hir::Expr::Index { object, index } => {
            let mut place = place_base(hir, members, *object)?;
            let constant = match hir.expressions[*index] {
                hir::Expr::Integer(value) => Some(value),
                _ => None,
            };
            place.projections.push(Projection::Index {
                expression: *index,
                constant,
            });
            Some(place)
        }
        _ => None,
    }
}

/// Returns the place whose storage a reference expression borrows.
///
/// A reference expression is a value, so it is deliberately excluded from
/// [`expression_place`]. Effect substitution still needs its originating place.
pub fn borrow_origin_place(
    hir: &hir::PackageHir,
    members: &HashMap<ExprId, MemberKind>,
    expression: ExprId,
) -> Option<Place> {
    match hir.expressions[expression] {
        hir::Expr::Reference(origin) => place_base(hir, members, origin),
        _ => expression_place(hir, members, expression),
    }
}

fn place_base(
    hir: &hir::PackageHir,
    members: &HashMap<ExprId, MemberKind>,
    expression: ExprId,
) -> Option<Place> {
    if let hir::Expr::Reference(origin) = hir.expressions[expression] {
        let mut place = place_base(hir, members, origin)?;
        place.projections.push(Projection::Dereference);
        Some(place)
    } else {
        expression_place(hir, members, expression)
    }
}
