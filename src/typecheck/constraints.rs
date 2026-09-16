//! Structural requirements on implementation parameters, independent of value representation.
use super::*;

impl Checker<'_> {
    pub(super) fn check_unconditional_constraints(
        &mut self,
        target: FunctionId,
        bindings: &HashMap<String, Ty>,
    ) -> Result<(), FosterError> {
        let previous = self.suppress_constraint_assumptions;
        self.suppress_constraint_assumptions = true;
        let result = self.check_constraints_with_bindings(target, target, bindings);
        self.suppress_constraint_assumptions = previous;
        result
    }
    pub(super) fn validate_constraints(&mut self) -> Result<(), FosterError> {
        for (function, definition) in self.hir.functions.iter() {
            if !definition.constraints.is_empty() && definition.receiver.is_some() {
                let receiver = self.functions[&function].parameters[0].ty.clone();
                let methods = match receiver {
                    Ty::Record(record, arguments) => {
                        self.effective_record_methods(record, &arguments)?
                    }
                    _ => Vec::new(),
                };
                for required in methods.iter().filter(|required| {
                    definition.name.rsplit('.').next() == Some(required.name.as_str())
                }) {
                    let signature = self.functions[&function].clone();
                    if required.parameters.len() + 1 != signature.parameters.len() {
                        continue;
                    }
                    let substitutions = self.substitutions.clone();
                    let matches = required
                        .parameters
                        .iter()
                        .zip(signature.parameters.iter().skip(1))
                        .all(|(expected, actual)| {
                            expected.mode == actual.mode
                                && self
                                    .unify(expected.ty.clone(), actual.ty.clone(), function)
                                    .is_ok()
                        });
                    self.substitutions = substitutions;
                    if matches {
                        let bindings = definition
                            .type_parameters
                            .iter()
                            .map(|name| (name.clone(), Ty::Generic(name.clone())))
                            .collect();
                        self.check_unconditional_constraints(function, &bindings)?;
                    }
                }
            }
            if !definition.constraints.is_empty()
                && definition.receiver.is_some()
                && matches!(definition.name.rsplit('.').next(), Some("copy" | "deinit"))
            {
                return Err(self.error(function, "conditional copy and deinit methods require runtime generic evidence and are not supported"));
            }
            let generics = definition
                .type_parameters
                .iter()
                .map(|name| (name.clone(), Ty::Generic(name.clone())))
                .collect();
            for constraint in &definition.constraints {
                if !definition.type_parameters.contains(&constraint.parameter) {
                    return Err(self.error(
                        function,
                        "implementation constraint names an undeclared type parameter",
                    ));
                }
                let requirement =
                    self.annotation_type(definition.module, &constraint.requirement, &generics)?;
                if definition.public
                    && let Some(private) = self.private_type_in(&requirement)
                {
                    return Err(self.error(
                        function,
                        format!(
                            "public function `{}` exposes private constraint type `{private}`",
                            definition.name
                        ),
                    ));
                }
                fn structural(ty: &Ty) -> bool {
                    match ty {
                        Ty::Record(_, _) => true,
                        Ty::Intersection(parts) => parts.iter().all(structural),
                        _ => false,
                    }
                }
                if !structural(&requirement) {
                    return Err(self.error(
                        function,
                        "implementation constraints must name structural record requirements",
                    ));
                }
            }
        }
        Ok(())
    }

    pub(super) fn validate_runtime_constraints(
        &mut self,
        caller: FunctionId,
        expected: &Ty,
    ) -> Result<(), FosterError> {
        let Ty::Record(record, arguments) = expected else {
            return Ok(());
        };
        let requirements = self.effective_record_methods(*record, arguments)?;
        for (_, method) in self.hir.functions.iter() {
            if method.receiver.is_some()
                && !method.constraints.is_empty()
                && requirements
                    .iter()
                    .any(|required| method.name.rsplit('.').next() == Some(required.name.as_str()))
            {
                return Err(self.error(caller, format!("runtime `is` cannot test conditional method `{}`: generic argument evidence is unavailable; use a statically checked structural parameter", method.name)));
            }
        }
        Ok(())
    }

    pub(super) fn constraint_requirements(
        &mut self,
        function: FunctionId,
        ty: &Ty,
    ) -> Result<Vec<Ty>, FosterError> {
        if self.suppress_constraint_assumptions {
            return Ok(Vec::new());
        }
        let Ty::Generic(name) = self.resolved(ty.clone()) else {
            return Ok(Vec::new());
        };
        let definition = &self.hir.functions[function];
        let generics = definition
            .type_parameters
            .iter()
            .map(|name| (name.clone(), Ty::Generic(name.clone())))
            .collect();
        definition
            .constraints
            .iter()
            .filter(|bound| bound.parameter == name)
            .map(|bound| self.annotation_type(definition.module, &bound.requirement, &generics))
            .collect()
    }

    pub(super) fn constraint_view(
        &mut self,
        function: FunctionId,
        ty: Ty,
    ) -> Result<Ty, FosterError> {
        let requirements = self.constraint_requirements(function, &ty)?;
        Ok(if requirements.is_empty() {
            ty
        } else {
            Ty::Intersection(requirements)
        })
    }

    pub(super) fn check_constraints_with_bindings(
        &mut self,
        caller: FunctionId,
        target: FunctionId,
        bindings: &HashMap<String, Ty>,
    ) -> Result<(), FosterError> {
        if self.hir.functions[target].constraints.is_empty() {
            return Ok(());
        }
        let key = (
            target,
            self.hir.functions[target]
                .type_parameters
                .iter()
                .filter_map(|name| bindings.get(name))
                .map(|ty| self.resolved(ty.clone()))
                .collect(),
        );
        if self.constraint_proofs.contains(&key) || self.constraint_proofs.len() >= 64 {
            return Err(self.error(
                caller,
                "cyclic implementation constraint cannot establish structural conformance",
            ));
        }
        self.constraint_proofs.push(key);
        let result = self.prove_constraints(caller, target, bindings);
        self.constraint_proofs.pop();
        result
    }

    fn prove_constraints(
        &mut self,
        caller: FunctionId,
        target: FunctionId,
        bindings: &HashMap<String, Ty>,
    ) -> Result<(), FosterError> {
        let definition = &self.hir.functions[target];
        for constraint in &definition.constraints {
            let actual = bindings
                .get(&constraint.parameter)
                .cloned()
                .ok_or_else(|| {
                    self.error(
                        caller,
                        format!(
                            "cannot establish constraint on `{}` for `{}`",
                            constraint.parameter, definition.name
                        ),
                    )
                })?;
            let actual = self.resolved(actual);
            let expected =
                self.annotation_type(definition.module, &constraint.requirement, bindings)?;
            if contains_variable(&actual) {
                return Err(self.error(
                    caller,
                    format!(
                        "cannot infer constrained parameter `{}` for `{}`",
                        constraint.parameter, definition.name
                    ),
                ));
            }
            self.coerce(expected.clone(), actual.clone(), caller).map_err(|error| self.error(caller,
                format!("implementation constraint for `{}` is not satisfied: `{}` must satisfy `{}` ({})",
                    definition.name, self.describe(&actual), self.describe(&expected), error.message)))?;
        }
        Ok(())
    }

    pub(super) fn check_callable_constraints(
        &mut self,
        caller: FunctionId,
        target: FunctionId,
        callable: &Ty,
        receiver: Option<Ty>,
    ) -> Result<(), FosterError> {
        if self.hir.functions[target].constraints.is_empty() {
            return Ok(());
        }
        let signature = self.functions[&target].clone();
        let mut bindings = HashMap::new();
        let expected = signature
            .parameters
            .iter()
            .map(|p| self.instantiate(p.ty.clone(), &mut bindings))
            .collect::<Vec<_>>();
        let expected_result = self.instantiate(signature.result, &mut bindings);
        let remote = matches!(receiver, Some(Ty::Remote(_)));
        let receiver = receiver.map(|ty| match self.resolved(ty) {
            Ty::Remote(value) => *value,
            ty => ty,
        });
        let actual = receiver
            .into_iter()
            .chain(callable.parameter_types().cloned())
            .collect::<Vec<_>>();
        if expected.len() != actual.len() {
            return Err(self.error(caller, "constrained callable has inconsistent arity"));
        }
        for (expected, actual) in expected.into_iter().zip(actual) {
            self.unify(expected, actual, caller)?;
        }
        if !remote {
            if let Ty::Callable { result, .. } | Ty::Function(_, result) = callable {
                self.unify(expected_result, (**result).clone(), caller)?;
            }
        }
        self.check_constraints_with_bindings(caller, target, &bindings)
    }

    pub(super) fn validate_constraint_uses(&mut self) -> Result<(), FosterError> {
        let calls = self
            .resolved_calls
            .iter()
            .map(|(expr, call)| (*expr, call.clone()))
            .collect::<Vec<_>>();
        for (expression, call) in calls {
            let Some(target) = call.function() else {
                continue;
            };
            if self.hir.functions[target].constraints.is_empty() {
                continue;
            }
            let Some(callable) = self.expressions.get(&expression).cloned() else {
                continue;
            };
            let caller = self.hir.expression_functions[&expression];
            let receiver = if matches!(call, ResolvedCall::Method { .. }) {
                match self.hir.expressions[expression] {
                    hir::Expr::Member { object, .. } => self.expressions.get(&object).cloned(),
                    _ => None,
                }
            } else {
                None
            };
            self.check_callable_constraints(caller, target, &callable, receiver)
                .map_err(|error| {
                    self.error_at_expression(
                        error,
                        caller,
                        expression,
                        "this implementation requires a matching structural contract",
                    )
                })?;
        }
        for (expression, value) in self.hir.expressions.iter() {
            let hir::Expr::Name(ResolvedName::Function(target)) = value else {
                continue;
            };
            if self.hir.functions[*target].constraints.is_empty() {
                continue;
            }
            let Some(callable) = self.expressions.get(&expression).cloned() else {
                continue;
            };
            self.check_callable_constraints(
                self.hir.expression_functions[&expression],
                *target,
                &callable,
                None,
            )?;
        }
        Ok(())
    }
}
