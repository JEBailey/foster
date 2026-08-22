use super::*;

impl Checker<'_> {
    pub(super) fn fresh(&mut self) -> Ty {
        let variable = self.next_variable;
        self.next_variable += 1;
        Ty::Variable(variable)
    }

    pub(super) fn unify(
        &mut self,
        left: Ty,
        right: Ty,
        function: FunctionId,
    ) -> Result<(), FosterError> {
        let left = self.resolved(left);
        let right = self.resolved(right);
        match (left, right) {
            (Ty::Variable(a), Ty::Variable(b)) if a == b => Ok(()),
            (Ty::Variable(variable), ty) | (ty, Ty::Variable(variable)) => {
                if self.occurs(variable, &ty) {
                    return Err(self.error(function, "type contains itself"));
                }
                self.substitutions.insert(variable, ty);
                Ok(())
            }
            (Ty::RawList(a), Ty::RawList(b)) | (Ty::Sequence(a), Ty::Sequence(b)) => {
                self.unify(*a, *b, function)
            }
            (Ty::Remote(a), Ty::Remote(b)) | (Ty::Future(a), Ty::Future(b)) => {
                self.unify(*a, *b, function)
            }
            (Ty::Record(a_record, a_arguments), Ty::Record(b_record, b_arguments))
                if a_record == b_record =>
            {
                for (a, b) in a_arguments.into_iter().zip(b_arguments) {
                    self.unify(a, b, function)?;
                }
                Ok(())
            }
            (Ty::Intersection(a), Ty::Intersection(b)) if a.len() == b.len() => {
                for (a, b) in a.into_iter().zip(b) {
                    self.unify(a, b, function)?;
                }
                Ok(())
            }
            (Ty::Variant(a, aa), Ty::Variant(b, ba)) if a == b => {
                for (x, y) in aa.into_iter().zip(ba) {
                    self.unify(x, y, function)?;
                }
                Ok(())
            }
            (Ty::Function(a_params, a_result), Ty::Function(b_params, b_result)) => {
                if a_params.len() != b_params.len() {
                    return Err(self.error(
                        function,
                        format!(
                            "function expects {} argument(s), received {}",
                            a_params.len(),
                            b_params.len()
                        ),
                    ));
                }
                for (a, b) in a_params.into_iter().zip(b_params) {
                    self.coerce(a, b, function)?;
                }
                self.unify(*a_result, *b_result, function)
            }
            (
                Ty::Callable {
                    parameters: a_params,
                    parameter_modes: a_modes,
                    result: a_result,
                    erased: a_erased,
                    effects: a_effects,
                    suspends: a_suspends,
                },
                Ty::Callable {
                    parameters: b_params,
                    parameter_modes: b_modes,
                    result: b_result,
                    erased: b_erased,
                    effects: b_effects,
                    suspends: b_suspends,
                },
            ) => {
                if a_params.len() != b_params.len() {
                    return Err(self.error(function, "function arity mismatch"));
                }
                let modes_compatible = a_modes.iter().zip(&b_modes).all(|(expected, actual)| {
                    !matches!(
                        (expected, actual),
                        (
                            crate::ast::ParameterMode::Borrow,
                            crate::ast::ParameterMode::Consume
                        )
                    )
                });
                if !modes_compatible
                    || !effects_are_subset(&b_effects, &a_effects)
                    || (!a_erased && b_erased)
                    || (!a_suspends && b_suspends)
                {
                    return Err(self.error(
                        function,
                        "callable contract is incompatible with the expected function type",
                    ));
                }
                for (a, b) in a_params.into_iter().zip(b_params) {
                    self.unify(a, b, function)?;
                }
                self.unify(*a_result, *b_result, function)
            }
            (
                Ty::Callable {
                    parameters: a_params,
                    result: a_result,
                    ..
                },
                Ty::Function(b_params, b_result),
            ) => {
                if a_params.len() != b_params.len() {
                    return Err(self.error(function, "function arity mismatch"));
                }
                for (a, b) in a_params.into_iter().zip(b_params) {
                    self.coerce(a, b, function)?;
                }
                self.unify(*a_result, *b_result, function)
            }
            (
                Ty::Function(a_params, a_result),
                Ty::Callable {
                    parameters: b_params,
                    result: b_result,
                    ..
                },
            ) => {
                if a_params.len() != b_params.len() {
                    return Err(self.error(function, "function arity mismatch"));
                }
                for (a, b) in a_params.into_iter().zip(b_params) {
                    self.unify(a, b, function)?;
                }
                self.unify(*a_result, *b_result, function)
            }
            (Ty::Reference(a_group, a), Ty::Reference(b_group, b))
                if a_group == b_group
                    || a_group == "_"
                    || b_group == "_"
                    || a_group == FRAME_GROUP
                    || b_group == FRAME_GROUP =>
            {
                self.unify(*a, *b, function)
            }
            (a, b) if a == b => Ok(()),
            (a, b) => Err(self.error(
                function,
                format!(
                    "type mismatch: expected `{}`, found `{}`",
                    self.describe(&a),
                    self.describe(&b)
                ),
            )),
        }
    }

    pub(super) fn resolved(&self, mut ty: Ty) -> Ty {
        loop {
            match ty {
                Ty::Variable(variable) => match self.substitutions.get(&variable) {
                    Some(replacement) => ty = replacement.clone(),
                    None => return Ty::Variable(variable),
                },
                Ty::RawList(element) => return Ty::RawList(Box::new(self.resolved(*element))),
                Ty::Sequence(element) => {
                    return Ty::Sequence(Box::new(self.resolved(*element)));
                }
                Ty::Remote(value) => return Ty::Remote(Box::new(self.resolved(*value))),
                Ty::Future(value) => return Ty::Future(Box::new(self.resolved(*value))),
                Ty::Variant(id, args) => {
                    return Ty::Variant(id, args.into_iter().map(|t| self.resolved(t)).collect());
                }
                Ty::Function(parameters, result) => {
                    return Ty::Function(
                        parameters
                            .into_iter()
                            .map(|parameter| self.resolved(parameter))
                            .collect(),
                        Box::new(self.resolved(*result)),
                    );
                }
                Ty::Callable {
                    parameters,
                    parameter_modes,
                    result,
                    erased,
                    effects,
                    suspends,
                } => {
                    return Ty::Callable {
                        parameters: parameters
                            .into_iter()
                            .map(|parameter| self.resolved(parameter))
                            .collect(),
                        parameter_modes,
                        result: Box::new(self.resolved(*result)),
                        erased,
                        effects,
                        suspends,
                    };
                }
                Ty::Reference(group, value) => {
                    return Ty::Reference(group, Box::new(self.resolved(*value)));
                }
                Ty::Record(record, arguments) => {
                    return Ty::Record(
                        record,
                        arguments
                            .into_iter()
                            .map(|argument| self.resolved(argument))
                            .collect(),
                    );
                }
                Ty::Intersection(members) => {
                    return Ty::Intersection(
                        members
                            .into_iter()
                            .map(|member| self.resolved(member))
                            .collect(),
                    );
                }
                concrete => return concrete,
            }
        }
    }

    pub(super) fn occurs(&self, variable: u32, ty: &Ty) -> bool {
        match self.resolved(ty.clone()) {
            Ty::Variable(found) => found == variable,
            Ty::RawList(element)
            | Ty::Sequence(element)
            | Ty::Remote(element)
            | Ty::Future(element) => self.occurs(variable, &element),
            Ty::Function(parameters, result) => {
                parameters.iter().any(|ty| self.occurs(variable, ty))
                    || self.occurs(variable, &result)
            }
            Ty::Callable {
                parameters, result, ..
            } => {
                parameters.iter().any(|ty| self.occurs(variable, ty))
                    || self.occurs(variable, &result)
            }
            Ty::Reference(_, value) => self.occurs(variable, &value),
            Ty::Record(_, arguments) => arguments
                .iter()
                .any(|argument| self.occurs(variable, argument)),
            Ty::Intersection(members) => members.iter().any(|member| self.occurs(variable, member)),
            _ => false,
        }
    }
}
