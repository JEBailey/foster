use crate::hir::{self, ExprId, Projection, ResolvedName};
use crate::types::TypeInformation;

use super::InvalidationKind;

pub(crate) fn call_invalidations(
    hir: &hir::PackageHir,
    types: &TypeInformation,
    callee: ExprId,
    arguments: &[ExprId],
) -> Vec<(hir::Place, InvalidationKind)> {
    let Some(ty) = types.expression_type(callee) else {
        return Vec::new();
    };
    let crate::types::Type::Function(signature) = &types.types[ty] else {
        return Vec::new();
    };
    let receiver = match hir.expressions[callee] {
        hir::Expr::Member { object, .. } => Some(object),
        _ => None,
    };
    let function = match hir.expressions[callee] {
        hir::Expr::Name(ResolvedName::Function(function)) => Some(function),
        _ => types.extension_methods.get(&callee).copied(),
    };

    signature
        .effects
        .iter()
        .filter_map(|effect| {
            let kind = match effect.kind {
                crate::ast::EffectKind::Reshape => InvalidationKind::Reshape,
                crate::ast::EffectKind::Consume => InvalidationKind::Consume,
                _ => return None,
            };
            let mut target = if effect.target.root == "self" {
                receiver.and_then(|receiver| hir::queries::expression_place(hir, receiver))
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
                                    Some(crate::ast::TypeExpr::Reference { group, .. })
                                        if *group == effect.target.root
                                )
                        })
                        .and_then(|(index, _)| {
                            let argument = index.checked_sub(usize::from(receiver.is_some()))?;
                            arguments
                                .get(argument)
                                .and_then(|argument| hir::queries::expression_place(hir, *argument))
                        })
                })
            }?;
            target.projections.extend(
                effect
                    .target
                    .children
                    .iter()
                    .cloned()
                    .map(Projection::Field),
            );
            Some((target, kind))
        })
        .collect()
}
