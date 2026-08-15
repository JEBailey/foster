use super::*;

impl Checker<'_> {
    pub(super) fn coerce(
        &mut self,
        expected: Ty,
        actual: Ty,
        function: FunctionId,
    ) -> Result<(), FosterError> {
        let expected = self.resolved(expected);
        let actual = self.resolved(actual);
        match (expected, actual) {
            (Ty::Sequence(expected), Ty::List(actual)) => self.unify(*expected, *actual, function),
            (Ty::Sequence(expected), Ty::String) => self.unify(*expected, Ty::CodePoint, function),
            (Ty::Sequence(expected), Ty::Sequence(actual)) => {
                self.unify(*expected, *actual, function)
            }
            (Ty::Intersection(requirements), actual) => {
                for requirement in requirements {
                    self.coerce(requirement, actual.clone(), function)?;
                }
                Ok(())
            }
            (Ty::Reference(expected_group, expected), Ty::Reference(actual_group, actual))
                if expected_group == actual_group
                    || expected_group == "_"
                    || actual_group == "_"
                    || expected_group == FRAME_GROUP
                    || actual_group == FRAME_GROUP =>
            {
                self.coerce(*expected, *actual, function)
            }
            (Ty::Record(expected, expected_arguments), Ty::Record(actual, actual_arguments))
                if expected != actual =>
            {
                self.coerce_record_shape(
                    function,
                    expected,
                    &expected_arguments,
                    Ty::Record(actual, actual_arguments),
                )
            }
            (expected, actual) => self.unify(expected, actual, function),
        }
    }

    fn coerce_record_shape(
        &mut self,
        function: FunctionId,
        expected: RecordId,
        expected_arguments: &[Ty],
        actual: Ty,
    ) -> Result<(), FosterError> {
        let caller_module = self.hir.functions[function].module;
        let expected_definition = self.hir.records[expected].clone();
        let expected_generics = expected_definition
            .parameters
            .iter()
            .cloned()
            .zip(expected_arguments.iter().cloned())
            .collect::<HashMap<_, _>>();

        for field in &expected_definition.fields {
            if !field.public && expected_definition.module != caller_module {
                return Err(self.error(
                    function,
                    format!(
                        "type `{}` cannot be structurally adapted because field `{}` is private",
                        expected_definition.name, field.name
                    ),
                ));
            }
            let expected_type =
                self.annotation_type(expected_definition.module, &field.ty, &expected_generics)?;
            let Some(actual_type) = self.structural_field_type(function, &actual, &field.name)?
            else {
                return Err(self.error(
                    function,
                    format!(
                        "type `{}` cannot adapt to `{}`: missing accessible field `{}`",
                        self.describe(&actual),
                        expected_definition.name,
                        field.name
                    ),
                ));
            };
            self.unify(expected_type, actual_type, function)?;
        }
        Ok(())
    }

    fn structural_field_type(
        &mut self,
        function: FunctionId,
        actual: &Ty,
        name: &str,
    ) -> Result<Option<Ty>, FosterError> {
        let caller_module = self.hir.functions[function].module;
        match self.resolved(actual.clone()) {
            Ty::Record(record, arguments) => {
                let definition = self.hir.records[record].clone();
                let Some(field) = definition
                    .fields
                    .iter()
                    .find(|field| field.name == name)
                    .cloned()
                else {
                    return Ok(None);
                };
                if !field.public && definition.module != caller_module {
                    return Ok(None);
                }
                let generics = definition
                    .parameters
                    .iter()
                    .cloned()
                    .zip(arguments)
                    .collect::<HashMap<_, _>>();
                self.annotation_type(definition.module, &field.ty, &generics)
                    .map(Some)
            }
            Ty::Intersection(members) => {
                let mut found: Option<Ty> = None;
                for member in members {
                    if let Some(candidate) = self.structural_field_type(function, &member, name)? {
                        if let Some(previous) = &found {
                            self.unify(previous.clone(), candidate.clone(), function)?;
                        }
                        found = Some(candidate);
                    }
                }
                Ok(found)
            }
            _ => Ok(None),
        }
    }

    pub(super) fn infer_record(
        &mut self,
        function: FunctionId,
        record: RecordId,
        supplied: &[(String, ExprId)],
    ) -> Result<Ty, FosterError> {
        let definition = &self.hir.records[record];
        if definition.module != self.hir.functions[function].module
            && definition.fields.iter().any(|field| !field.public)
        {
            return Err(self.error(
                function,
                format!(
                    "record `{}` cannot be constructed outside its module because it has private fields",
                    definition.name
                ),
            ));
        }
        let arguments = definition
            .parameters
            .iter()
            .map(|_| self.fresh())
            .collect::<Vec<_>>();
        let generics = definition
            .parameters
            .iter()
            .cloned()
            .zip(arguments.iter().cloned())
            .collect::<HashMap<_, _>>();
        let mut seen = std::collections::HashSet::new();
        for (name, expression) in supplied {
            if !seen.insert(name.as_str()) {
                return Err(self.error(function, format!("field `{name}` is initialized twice")));
            }
            let field = definition
                .fields
                .iter()
                .find(|field| field.name == *name)
                .ok_or_else(|| {
                    self.error(
                        function,
                        format!("record `{}` has no field `{name}`", definition.name),
                    )
                })?;
            if definition.module != self.hir.functions[function].module && !field.public {
                return Err(self.error(
                    function,
                    format!("field `{}.{name}` is private", definition.name),
                ));
            }
            let expected = self.annotation_type(definition.module, &field.ty, &generics)?;
            let actual = self.infer_expression(function, *expression)?;
            self.coerce(expected, actual, function)?;
        }
        let missing = definition
            .fields
            .iter()
            .filter(|field| !seen.contains(field.name.as_str()))
            .map(|field| field.name.as_str())
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(self.error(
                function,
                format!(
                    "record `{}` is missing field(s): {}",
                    definition.name,
                    missing.join(", ")
                ),
            ));
        }
        Ok(Ty::Record(record, arguments))
    }

    pub(super) fn record_field_type(
        &mut self,
        function: FunctionId,
        record: RecordId,
        arguments: &[Ty],
        name: &str,
    ) -> Result<Ty, FosterError> {
        let definition = &self.hir.records[record];
        let field = definition
            .fields
            .iter()
            .find(|field| field.name == name)
            .ok_or_else(|| {
                self.error(
                    function,
                    format!("record `{}` has no field `{name}`", definition.name),
                )
            })?;
        if definition.module != self.hir.functions[function].module && !field.public {
            return Err(self.error(
                function,
                format!("field `{}.{name}` is private", definition.name),
            ));
        }
        let generics = definition
            .parameters
            .iter()
            .cloned()
            .zip(arguments.iter().cloned())
            .collect::<HashMap<_, _>>();
        self.annotation_type(definition.module, &field.ty, &generics)
    }

    pub(super) fn solve_member_constraints(&mut self) -> Result<(), FosterError> {
        let constraints = std::mem::take(&mut self.member_constraints);
        let mut pending = constraints;
        loop {
            let mut next = Vec::new();
            let mut progress = false;
            for constraint in pending {
                let receiver = self.resolved(constraint.receiver.clone());
                if matches!(receiver, Ty::Variable(_)) {
                    next.push(constraint);
                    continue;
                }
                let member = self.infer_member(constraint.function, receiver, &constraint.name)?;
                self.unify(constraint.result, member, constraint.function)?;
                progress = true;
            }
            if next.is_empty() {
                return Ok(());
            }
            if !progress {
                let constraint = &next[0];
                return Err(self.error(
                    constraint.function,
                    format!(
                        "cannot resolve member `{}` until the receiver type is known; add a type annotation",
                        constraint.name
                    ),
                ));
            }
            pending = next;
        }
    }
}
