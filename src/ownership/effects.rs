use crate::hir::{self, ExprId, Projection};
use crate::types::TypeInformation;

use super::InvalidationKind;

pub(crate) fn call_invalidations(
    hir: &hir::PackageHir,
    types: &TypeInformation,
    callee: ExprId,
    arguments: &[ExprId],
) -> Vec<(hir::Place, InvalidationKind)> {
    call_effects(hir, types, callee, arguments, false)
}

pub(crate) fn call_mutations(
    hir: &hir::PackageHir,
    types: &TypeInformation,
    callee: ExprId,
    arguments: &[ExprId],
) -> Vec<(hir::Place, InvalidationKind)> {
    call_effects(hir, types, callee, arguments, true)
}

fn call_effects(
    hir: &hir::PackageHir,
    types: &TypeInformation,
    callee: ExprId,
    arguments: &[ExprId],
    include_mut: bool,
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
    let function = types.resolved_function_for_callee(callee);

    let mut changes = Vec::new();
    for effect in &signature.effects {
        let kind = match effect.kind {
            crate::ast::EffectKind::Mut if include_mut => InvalidationKind::Reshape,
            crate::ast::EffectKind::Reshape => InvalidationKind::Reshape,
            crate::ast::EffectKind::Consume => InvalidationKind::Consume,
            _ => continue,
        };
        let mut targets = Vec::new();
        if effect.target.root == "self" {
            targets.extend(receiver.and_then(|receiver| {
                crate::semantics::borrow_origin_place(hir, &types.member_kinds, receiver)
            }));
        } else if let Some(function) = function {
            let definition = &hir.functions[function];
            for (index, parameter) in definition.parameters.iter().enumerate() {
                if hir.locals[*parameter].name != effect.target.root
                    && !matches!(definition.parameter_types[index].as_ref(),
                        Some(crate::ast::TypeExpr::Reference { group, .. }) if *group == effect.target.root)
                {
                    continue;
                }
                let argument = if receiver.is_some() && index == 0 {
                    receiver
                } else {
                    index
                        .checked_sub(usize::from(receiver.is_some()))
                        .and_then(|index| arguments.get(index).copied())
                };
                targets.extend(argument.and_then(|argument| {
                    crate::semantics::borrow_origin_place(hir, &types.member_kinds, argument)
                }));
            }
        }
        for mut target in targets {
            target.projections.extend(
                effect
                    .target
                    .children
                    .iter()
                    .cloned()
                    .map(Projection::Field),
            );
            changes.push((target, kind));
        }
    }
    changes
}
