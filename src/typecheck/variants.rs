use super::*;

impl Checker<'_> {
    pub(super) fn has_type_evidence(&self, expression: ExprId, expected: &Ty) -> bool {
        let hir::Expr::Name(ResolvedName::Local(local)) = self.hir.expressions[expression] else {
            return false;
        };
        self.type_facts.iter().any(|(subject, ty)| {
            *subject == local && self.resolved(ty.clone()) == self.resolved(expected.clone())
        })
    }
    pub(super) fn structural_pattern_type(&self, ty: &Ty) -> bool {
        matches!(ty, Ty::Record(record, _) if !matches!(self.hir.modules[self.hir.records[*record].module].name.as_str(), "core.string" | "core.symbol" | "core.bytes" | "core.bytes.buffer"))
    }
    pub(super) fn is_type_pattern_place(&self, expression: ExprId) -> bool {
        crate::semantics::expression_place(self.hir, &self.member_kinds, expression).is_some_and(
            |place| {
                self.hir.locals[place.root].kind == hir::LocalKind::TypePattern
                    && !self
                        .type_facts
                        .iter()
                        .any(|(local, ty)| *local == place.root && self.structural_pattern_type(ty))
            },
        )
    }
    pub(super) fn enter_type_pattern(&mut self, subject: Option<ExprId>, test: &hir::BranchTest) {
        if let hir::BranchTest::Pattern(pattern) = test
            && let hir::Pattern::IsType {
                target,
                binding: Some(local),
                ..
            } = pattern.unspanned()
        {
            if let Some(subject) = subject
                && let hir::Expr::Name(ResolvedName::Local(original)) =
                    self.hir.expressions[subject]
            {
                let inherited = self
                    .type_facts
                    .iter()
                    .filter(|(local, _)| *local == original)
                    .map(|(_, ty)| (*local, ty.clone()))
                    .collect::<Vec<_>>();
                self.type_facts.extend(inherited);
            }
            self.type_facts.push((*local, self.pattern_type(target)));
        }
    }

    pub(super) fn refined_receiver(&self, expression: ExprId, original: Ty) -> Ty {
        if let hir::Expr::Name(ResolvedName::Local(local)) = self.hir.expressions[expression] {
            let facts = self
                .type_facts
                .iter()
                .filter(|(subject, _)| *subject == local)
                .filter(|(_, ty)| self.resolved(ty.clone()) != self.resolved(original.clone()))
                .map(|(_, ty)| ty.clone())
                .collect::<Vec<_>>();
            if !facts.is_empty() {
                return Ty::Intersection(std::iter::once(original).chain(facts).collect());
            }
        }
        original
    }

    pub(super) fn pattern_type(&self, target: &crate::codegen::types::ExecutableType) -> Ty {
        use crate::codegen::types::ExecutableType as E;
        match target {
            E::Unit => Ty::Unit,
            E::Bool => Ty::Bool,
            E::Integer => Ty::Int,
            E::Float => Ty::Float,
            E::Byte => Ty::Byte,
            E::CodePoint => Ty::CodePoint,
            E::Bytes => self.bytes_type(),
            E::Record { record, .. } => Ty::Record(*record, Vec::new()),
            E::Variant { variant, .. } => Ty::Variant(*variant, Vec::new()),
            _ => unreachable!(),
        }
    }

    pub(super) fn check_pattern(
        &mut self,
        function: FunctionId,
        pattern: &hir::Pattern,
        expected: Ty,
        covered: &mut std::collections::HashSet<hir::VariantId>,
        catch_all: &mut bool,
        top_level: bool,
    ) -> Result<(), FosterError> {
        match pattern.unspanned() {
            hir::Pattern::IsType {
                target, binding, ..
            } => {
                if !top_level {
                    return Err(self.error(function, "`is` must be a top-level branch pattern"));
                }
                self.body_cacheable = false;
                let ty = self.pattern_type(target);
                if let Some(local) = binding {
                    // Structural evidence augments the original receiver. Keeping its
                    // representation and self type avoids constructing a detached cast.
                    let ty = if self.structural_pattern_type(&ty) {
                        expected
                    } else {
                        ty
                    };
                    self.locals.insert(*local, ty);
                }
            }
            hir::Pattern::Wildcard => {
                if top_level {
                    *catch_all = true;
                }
            }
            hir::Pattern::Binding(local) => {
                let expected = self.resolved(expected);
                if let Some(group) = reference_group(&expected) {
                    self.local_groups.insert(*local, group);
                }
                self.locals.insert(*local, expected);
                if top_level {
                    *catch_all = true;
                }
            }
            hir::Pattern::Bool(_) => self.unify(expected, Ty::Bool, function)?,
            hir::Pattern::Integer(_) => self.unify(expected, Ty::Int, function)?,
            hir::Pattern::Float(_) => self.unify(expected, Ty::Float, function)?,
            hir::Pattern::String(_) => self.unify(expected, self.string_type(), function)?,
            hir::Pattern::CodePoint(_) => self.unify(expected, Ty::CodePoint, function)?,
            hir::Pattern::Symbol(_) => self.unify(expected, self.symbol_type(), function)?,
            hir::Pattern::Variant { variant, fields } => {
                let definition = self.hir.variants[*variant].clone();
                let parent = self.hir.variant_types[definition.parent].clone();
                let payload_count = usize::from(definition.payload.is_some());
                if fields.len() != payload_count {
                    return Err(self.error(
                        function,
                        format!(
                            "pattern `{}.{}` expects {} payload value(s), received {}",
                            parent.name,
                            definition.name,
                            payload_count,
                            fields.len()
                        ),
                    ));
                }
                let generics = parent
                    .parameters
                    .iter()
                    .map(|p| (p.clone(), self.fresh()))
                    .collect::<HashMap<_, _>>();
                let args = parent
                    .parameters
                    .iter()
                    .map(|p| generics[p].clone())
                    .collect();
                self.unify(expected, Ty::Variant(definition.parent, args), function)?;
                if top_level && fields.iter().all(pattern_is_irrefutable) {
                    covered.insert(*variant);
                }
                for (field, annotation) in fields.iter().zip(definition.payload.iter()) {
                    let ty = self.annotation_type(parent.module, annotation, &generics)?;
                    self.check_pattern(function, field, ty, covered, catch_all, false)?;
                }
            }
            hir::Pattern::Spanned { .. } => unreachable!("patterns are unwrapped above"),
        }
        Ok(())
    }
}
