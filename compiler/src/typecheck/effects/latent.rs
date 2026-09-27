//! Deferred callback effects retained by values and propagated through helper returns.
use super::*;

impl EffectDerivation<'_, '_> {
    pub(super) fn remember_call_latent(&mut self, callee: ExprId, arguments: &[ExprId]) {
        let retained = arguments
            .iter()
            .flat_map(|value| self.latent_effects(*value))
            .collect::<Vec<_>>();
        if retained.is_empty() {
            return;
        }
        let Some(target) = self.call_target(callee) else {
            return;
        };
        let definition = &self.checker.hir.functions[target];
        let receiver = match self.checker.hir.expressions[callee] {
            hir::Expr::Member { object, .. } => Some(object),
            _ => None,
        };
        let effects = self.callee_effects(target).0.to_vec();
        for effect in effects {
            if !matches!(
                effect.kind,
                crate::ast::EffectKind::Mut | crate::ast::EffectKind::Reshape
            ) {
                continue;
            }
            let place = if effect.target.root == "self" {
                receiver
            } else {
                definition
                    .parameters
                    .iter()
                    .position(|parameter| {
                        self.checker.hir.locals[parameter.local].name == effect.target.root
                    })
                    .and_then(|index| index.checked_sub(usize::from(receiver.is_some())))
                    .and_then(|index| arguments.get(index).copied())
            };
            if let Some(place) = place.and_then(|place| {
                crate::semantics::expression_place(
                    self.checker.hir,
                    &self.checker.member_kinds,
                    place,
                )
            }) {
                let stored = self.latent.entry(place.root).or_default();
                for effect in &retained {
                    if !stored.contains(effect) {
                        stored.push(effect.clone());
                    }
                }
            }
        }
    }
    pub(super) fn remember_pattern_latent(&mut self, pattern: &hir::Pattern, expression: ExprId) {
        match pattern.unspanned() {
            hir::Pattern::Binding(local) => self.remember_latent(*local, expression),
            hir::Pattern::Record { fields } => {
                for (_, pattern) in fields {
                    self.remember_pattern_latent(pattern, expression);
                }
            }
            hir::Pattern::Variant { fields, .. } => {
                for pattern in fields {
                    self.remember_pattern_latent(pattern, expression);
                }
            }
            _ => {}
        }
    }
    pub(super) fn invoke_latent(&mut self, expression: ExprId) {
        for effect in self.latent_effects(expression) {
            self.add(effect.kind, effect.target);
        }
    }

    pub(super) fn remember_latent(&mut self, local: LocalId, expression: ExprId) {
        let effects = self.latent_effects(expression);
        if effects.is_empty() {
            return;
        }
        let retained = self.latent.entry(local).or_default();
        for effect in effects {
            if !retained.contains(&effect) {
                retained.push(effect);
            }
        }
    }

    pub(super) fn return_latent(&mut self, expression: ExprId) {
        for effect in self.latent_effects(expression) {
            if self.contract.contains(&effect.target.root) && !self.result_effects.contains(&effect)
            {
                self.result_effects.push(effect);
            }
        }
    }

    pub(super) fn latent_effects(&self, expression: ExprId) -> Vec<crate::ast::Effect> {
        if self
            .checker
            .expressions
            .get(&expression)
            .is_some_and(|ty| !self.retains_callbacks(self.checker.resolved(ty.clone())))
        {
            return Vec::new();
        }
        match &self.checker.hir.expressions[expression] {
            hir::Expr::Name(ResolvedName::Local(local)) => {
                self.latent.get(local).cloned().unwrap_or_default()
            }
            hir::Expr::Closure { function, captures } => {
                let mut retained = captures
                    .iter()
                    .flat_map(|capture| {
                        self.latent
                            .get(&capture.local)
                            .into_iter()
                            .flatten()
                            .cloned()
                    })
                    .collect::<Vec<_>>();
                retained.extend(
                    self.callee_effects(*function)
                        .0
                        .iter()
                        .filter_map(|effect| {
                            let capture = captures.iter().find(|capture| {
                                let group =
                                    self.checker.locals.get(&capture.local).and_then(|ty| {
                                        reference_group(&self.checker.resolved(ty.clone()))
                                    });
                                (capture.mode == hir::CaptureMode::Ref || group.is_some())
                                    && (self.checker.hir.locals[capture.local].name
                                        == effect.target.root
                                        || group.as_deref() == Some(effect.target.root.as_str()))
                            })?;
                            Some(crate::ast::Effect {
                                capture: false,
                                kind: effect.kind,
                                target: self
                                    .local_group(capture.local)
                                    .with_children(&effect.target.children),
                            })
                        }),
                );
                retained
            }
            hir::Expr::Branch { arms, .. } => arms
                .iter()
                .flat_map(|arm| {
                    arm.body
                        .last()
                        .into_iter()
                        .flat_map(|statement| match statement {
                            hir::Stmt::Expr(value) => self.latent_effects(*value),
                            _ => Vec::new(),
                        })
                })
                .collect(),
            hir::Expr::MoveOut(value)
            | hir::Expr::Reference(value)
            | hir::Expr::Member { object: value, .. }
            | hir::Expr::Index { object: value, .. } => self.latent_effects(*value),
            hir::Expr::Call { callee, arguments } => {
                let mut effects = self.latent_effects(*callee);
                for argument in arguments {
                    effects.extend(self.latent_effects(*argument));
                }
                if let Some(target) = self.call_target(*callee) {
                    let definition = &self.checker.hir.functions[target];
                    let summary = self
                        .summaries
                        .and_then(|rows| rows.get(&target))
                        .map(|row| &row.2)
                        .unwrap_or(&definition.result_effects);
                    let receiver = match self.checker.hir.expressions[*callee] {
                        hir::Expr::Member { object, .. } => Some(object),
                        _ => None,
                    };
                    for effect in summary {
                        let actual = if effect.target.root == "self" {
                            receiver
                        } else {
                            definition
                                .parameters
                                .iter()
                                .position(|parameter| {
                                    self.checker.hir.locals[parameter.local].name
                                        == effect.target.root
                                })
                                .and_then(|index| {
                                    index.checked_sub(usize::from(receiver.is_some()))
                                })
                                .and_then(|index| arguments.get(index).copied())
                        };
                        if let Some(actual) = actual {
                            effects.push(crate::ast::Effect {
                                capture: false,
                                kind: effect.kind,
                                target: self
                                    .place_group(actual)
                                    .with_children(&effect.target.children),
                            });
                        }
                    }
                }
                effects
            }
            hir::Expr::Record { fields, .. } => fields
                .iter()
                .flat_map(|(_, value)| self.latent_effects(*value))
                .collect(),
            hir::Expr::List(values) => values
                .iter()
                .flat_map(|value| self.latent_effects(*value))
                .collect(),
            _ => Vec::new(),
        }
    }

    pub(super) fn retains_callbacks(&self, ty: Ty) -> bool {
        if ty == self.checker.string_type() {
            return false;
        }
        if let Some(element) = self.checker.list_element(&ty) {
            return self.retains_callbacks(element);
        }
        match ty {
            Ty::Unit
            | Ty::Never
            | Ty::Bool
            | Ty::Int
            | Ty::RawInt
            | Ty::Float
            | Ty::CodePoint
            | Ty::Byte
            | Ty::RawBytes
            | Ty::RawByteBuffer => false,
            _ => true,
        }
    }
}
