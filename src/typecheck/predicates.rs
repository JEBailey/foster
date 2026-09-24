use super::*;

impl Checker<'_> {
    /// Foreign resources are thread-bound even when hidden in a private field,
    /// a generic collection, or an enum payload.
    pub(super) fn contains_c_resource(&mut self, ty: &Ty) -> Result<bool, FosterError> {
        if self.hir.module_named("std.ffi").is_none() {
            return Ok(false);
        }
        self.contains_c_resource_inner(ty, &mut std::collections::HashSet::new())
    }

    fn contains_c_resource_inner(
        &mut self,
        ty: &Ty,
        seen: &mut std::collections::HashSet<Ty>,
    ) -> Result<bool, FosterError> {
        let ty = self.resolved(ty.clone());
        if !seen.insert(ty.clone()) {
            return Ok(false);
        }
        match ty {
            Ty::Record(record, arguments) => {
                let definition = &self.hir.records[record];
                if definition.name == "CResource"
                    && self.hir.modules[definition.module].name == "std.ffi"
                {
                    return Ok(true);
                }
                for argument in &arguments {
                    if self.contains_c_resource_inner(argument, seen)? {
                        return Ok(true);
                    }
                }
                for field in self.effective_record_fields(record, &arguments)? {
                    if self.contains_c_resource_inner(&field.ty, seen)? {
                        return Ok(true);
                    }
                }
            }
            Ty::Variant(variant, arguments) => {
                for argument in &arguments {
                    if self.contains_c_resource_inner(argument, seen)? {
                        return Ok(true);
                    }
                }
                let definition = self.hir.variant_types[variant].clone();
                let generics = definition
                    .parameters
                    .iter()
                    .cloned()
                    .zip(arguments)
                    .collect();
                for alternative in definition.alternatives {
                    let alternative = self.hir.variants[alternative].clone();
                    for annotation in alternative.payload.iter().chain(alternative.member.iter()) {
                        let ty = self.annotation_type(definition.module, annotation, &generics)?;
                        if self.contains_c_resource_inner(&ty, seen)? {
                            return Ok(true);
                        }
                    }
                }
            }
            Ty::Reference(_, inner)
            | Ty::RawList(inner)
            | Ty::Sequence(inner)
            | Ty::Future(inner) => return self.contains_c_resource_inner(&inner, seen),
            Ty::Intersection(members) => {
                for member in members {
                    if self.contains_c_resource_inner(&member, seen)? {
                        return Ok(true);
                    }
                }
            }
            _ => {}
        }
        Ok(false)
    }
}

pub(super) fn contains_variable(ty: &Ty) -> bool {
    match ty {
        Ty::Variable(_) => true,
        Ty::Generic(_) => false,
        Ty::RawList(element)
        | Ty::Sequence(element)
        | Ty::Remote(element)
        | Ty::Future(element) => contains_variable(element),
        Ty::Function(parameters, result) => {
            parameters.iter().any(contains_variable) || contains_variable(result)
        }
        Ty::Callable {
            parameters, result, ..
        } => parameters.iter().any(|p| contains_variable(&p.ty)) || contains_variable(result),
        Ty::Reference(_, value) => contains_variable(value),
        Ty::Record(_, arguments) => arguments.iter().any(contains_variable),
        Ty::Intersection(members) => members.iter().any(contains_variable),
        Ty::Variant(_, arguments) => arguments.iter().any(contains_variable),
        _ => false,
    }
}

pub(super) fn remote_transferable(ty: &Ty, hir: &hir::PackageHir) -> bool {
    match ty {
        Ty::Reference(_, _)
        | Ty::Function(_, _)
        | Ty::Callable { .. }
        | Ty::Future(_)
        | Ty::Module(_) => false,
        Ty::RawList(value) | Ty::Sequence(value) => remote_transferable(value, hir),
        Ty::Remote(_) => true,
        Ty::Record(record, _) if hir.modules[hir.records[*record].module].name == "core.future" => {
            false
        }
        Ty::Record(_, arguments) | Ty::Variant(_, arguments) => arguments
            .iter()
            .all(|value| remote_transferable(value, hir)),
        Ty::Intersection(members) => members.iter().all(|value| remote_transferable(value, hir)),
        _ => true,
    }
}

pub(super) fn pattern_is_irrefutable(pattern: &hir::Pattern) -> bool {
    match pattern.unspanned() {
        hir::Pattern::Wildcard | hir::Pattern::Binding(_) => true,
        hir::Pattern::Record { fields } => fields
            .iter()
            .all(|(_, pattern)| pattern_is_irrefutable(pattern)),
        _ => false,
    }
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
            let name = &hir.locals[parameter.local].name;
            let reference_group = parameter
                .ty
                .as_ref()
                .and_then(|annotation| match annotation {
                    crate::ast::TypeExpr::Reference { group, .. } => Some(group.as_str()),
                    _ => None,
                });
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
                        .any(|parameter| hir.locals[parameter.local].name == effect.target.root))
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
    // Mutating an owner may move values out of its descendants as part of replacing
    // them, but it never grants permission to consume the owner itself.
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
                        | (crate::ast::EffectKind::Consume, crate::ast::EffectKind::Mut)
                )
                && !(actual.kind == crate::ast::EffectKind::Consume
                    && expected.kind == crate::ast::EffectKind::Mut
                    && actual.target == expected.target)
        })
    })
}
