use super::*;

impl Checker<'_> {
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
            hir::Pattern::Wildcard => {
                if top_level {
                    *catch_all = true;
                }
            }
            hir::Pattern::Binding(local) => {
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
                if fields.len() != definition.payload.len() {
                    return Err(self.error(
                        function,
                        format!(
                            "pattern `{}.{}` expects {} payload value(s), received {}",
                            parent.name,
                            definition.name,
                            definition.payload.len(),
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
