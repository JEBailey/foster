use super::*;

pub(super) fn contains_variable(ty: &Ty) -> bool {
    match ty {
        Ty::Variable(_) => true,
        Ty::Generic(_) => false,
        Ty::List(element) | Ty::Sequence(element) | Ty::Remote(element) | Ty::Future(element) => {
            contains_variable(element)
        }
        Ty::Function(parameters, result) => {
            parameters.iter().any(contains_variable) || contains_variable(result)
        }
        Ty::Callable {
            parameters, result, ..
        } => parameters.iter().any(contains_variable) || contains_variable(result),
        Ty::Reference(_, value) => contains_variable(value),
        Ty::Record(_, arguments) => arguments.iter().any(contains_variable),
        Ty::Intersection(members) => members.iter().any(contains_variable),
        Ty::Variant(_, arguments) => arguments.iter().any(contains_variable),
        _ => false,
    }
}

pub(super) fn remote_transferable(ty: &Ty) -> bool {
    match ty {
        Ty::Reference(_, _)
        | Ty::Function(_, _)
        | Ty::Callable { .. }
        | Ty::Future(_)
        | Ty::Module(_) => false,
        Ty::List(value) | Ty::Sequence(value) => remote_transferable(value),
        Ty::Remote(_) => true,
        Ty::Record(_, arguments) | Ty::Variant(_, arguments) => {
            arguments.iter().all(remote_transferable)
        }
        Ty::Intersection(members) => members.iter().all(remote_transferable),
        _ => true,
    }
}

pub(super) fn pattern_is_irrefutable(pattern: &hir::Pattern) -> bool {
    matches!(
        pattern.unspanned(),
        hir::Pattern::Wildcard | hir::Pattern::Binding(_)
    )
}

pub(super) const FRAME_GROUP: &str = "<frame>";

pub(super) fn function_parameter_modes(
    hir: &hir::PackageHir,
    function: FunctionId,
) -> Vec<crate::ast::ParameterMode> {
    let definition = &hir.functions[function];
    definition
        .parameters
        .iter()
        .map(|parameter| {
            let name = &hir.locals[*parameter].name;
            let index = definition
                .parameters
                .iter()
                .position(|candidate| candidate == parameter)
                .expect("parameter belongs to function");
            let reference_group = definition.parameter_types[index].as_ref().and_then(
                |annotation| match annotation {
                    crate::ast::TypeExpr::Reference { group, .. } => Some(group.as_str()),
                    _ => None,
                },
            );
            if definition.effects.iter().any(|effect| {
                effect.kind == crate::ast::EffectKind::Consume
                    && (effect.target.root == *name
                        || reference_group == Some(effect.target.root.as_str()))
            }) {
                crate::ast::ParameterMode::Consume
            } else {
                crate::ast::ParameterMode::Borrow
            }
        })
        .collect()
}

pub(super) fn callable_effects(
    hir: &hir::PackageHir,
    function: FunctionId,
) -> Vec<crate::ast::Effect> {
    let definition = &hir.functions[function];
    definition
        .effects
        .iter()
        .filter(|effect| {
            effect.kind != crate::ast::EffectKind::Read
                && (effect.kind != crate::ast::EffectKind::Consume
                    || !definition
                        .parameters
                        .iter()
                        .any(|parameter| hir.locals[*parameter].name == effect.target.root))
        })
        .cloned()
        .collect()
}

pub(super) fn reference_group(ty: &Ty) -> Option<String> {
    match ty {
        Ty::Reference(group, _) if group != "_" => Some(group.clone()),
        _ => None,
    }
}

pub(super) fn effect_kind_name(kind: crate::ast::EffectKind) -> &'static str {
    match kind {
        crate::ast::EffectKind::Read => "read",
        crate::ast::EffectKind::Mut => "mut",
        crate::ast::EffectKind::Reshape => "reshape",
        crate::ast::EffectKind::Consume => "consume",
    }
}

pub(super) fn effects_are_subset(
    actual: &[crate::ast::Effect],
    expected: &[crate::ast::Effect],
) -> bool {
    actual.iter().all(|actual| {
        expected.iter().any(|expected| {
            expected.target.covers(&actual.target)
                && matches!(
                    (actual.kind, expected.kind),
                    (crate::ast::EffectKind::Read, crate::ast::EffectKind::Read)
                        | (crate::ast::EffectKind::Read, crate::ast::EffectKind::Mut)
                        | (
                            crate::ast::EffectKind::Read,
                            crate::ast::EffectKind::Reshape
                        )
                        | (
                            crate::ast::EffectKind::Read,
                            crate::ast::EffectKind::Consume
                        )
                        | (crate::ast::EffectKind::Mut, crate::ast::EffectKind::Mut)
                        | (crate::ast::EffectKind::Mut, crate::ast::EffectKind::Reshape)
                        | (
                            crate::ast::EffectKind::Reshape,
                            crate::ast::EffectKind::Reshape
                        )
                        | (
                            crate::ast::EffectKind::Consume,
                            crate::ast::EffectKind::Consume
                        )
                )
        })
    })
}
