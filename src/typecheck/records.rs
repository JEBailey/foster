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
        if let Ty::Sequence(element) = &expected
            && self.is_string_type(&actual)
        {
            return self.unify((**element).clone(), Ty::CodePoint, function);
        }
        match (expected, actual) {
            (Ty::Sequence(expected), Ty::List(actual)) => self.unify(*expected, *actual, function),
            (Ty::Sequence(expected), Ty::Bytes) => self.unify(*expected, Ty::Byte, function),
            (Ty::Sequence(expected), actual @ Ty::Record(_, _)) => {
                self.coerce_sequence_shape(function, *expected, actual)
            }
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
            (
                Ty::Record(expected, expected_arguments),
                actual @ (Ty::List(_) | Ty::Sequence(_) | Ty::Bytes),
            ) => self.coerce_record_shape(function, expected, &expected_arguments, actual),
            (expected, actual) => self.unify(expected, actual, function),
        }
    }

    fn coerce_sequence_shape(
        &mut self,
        function: FunctionId,
        element: Ty,
        actual: Ty,
    ) -> Result<(), FosterError> {
        for (name, expected) in [
            ("empty?", Ty::Bool),
            ("length", Ty::Int),
            ("head", element.clone()),
        ] {
            let Some(found) = self.structural_accessor_type(function, &actual, name)? else {
                return Err(self.error(
                    function,
                    format!(
                        "type `{}` cannot adapt to `Sequence<{}>`: missing accessible member `{name}`",
                        self.describe(&actual),
                        self.describe(&element)
                    ),
                ));
            };
            self.unify(expected, found, function)?;
        }
        let Some(rest) = self.structural_accessor_type(function, &actual, "rest")? else {
            return Err(self.error(
                function,
                format!(
                    "type `{}` cannot adapt to `Sequence<{}>`: missing accessible member `rest`",
                    self.describe(&actual),
                    self.describe(&element)
                ),
            ));
        };
        self.coerce(Ty::Sequence(Box::new(element)), rest, function)
    }

    fn structural_accessor_type(
        &mut self,
        function: FunctionId,
        actual: &Ty,
        name: &str,
    ) -> Result<Option<Ty>, FosterError> {
        if let Some(field) = self.structural_field_type(function, actual, name)? {
            return Ok(Some(field));
        }
        let Ty::Record(record, arguments) = self.resolved(actual.clone()) else {
            return Ok(None);
        };
        let method = match self
            .effective_record_methods(record, &arguments)?
            .into_iter()
            .find(|method| method.name == name)
        {
            Some(method) => Some(Ty::Callable {
                parameters: method.parameters,
                parameter_modes: method.parameter_modes,
                result: Box::new(method.result),
                erased: false,
                effects: method.effects,
                suspends: method.suspends,
            }),
            None => self
                .record_method_type(function, record, arguments, name, false, false)
                .ok(),
        };
        match method.map(|method| self.resolved(method)) {
            Some(Ty::Callable {
                parameters, result, ..
            }) if parameters.is_empty() => Ok(Some(*result)),
            _ => Ok(None),
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
        let expected_fields = self.effective_record_fields(expected, expected_arguments)?;

        for field in &expected_fields {
            if !field.public && expected_definition.module != caller_module {
                return Err(self.error(
                    function,
                    format!(
                        "type `{}` cannot be structurally adapted because field `{}` is private",
                        expected_definition.name, field.name
                    ),
                ));
            }
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
            self.coerce(field.ty.clone(), actual_type, function)?;
        }
        let expected_methods = self.effective_record_methods(expected, expected_arguments)?;
        for method in expected_methods {
            if !method.public && expected_definition.module != caller_module {
                continue;
            }
            let Some(actual_method) =
                self.structural_method_type(function, &actual, &method.name)?
            else {
                return Err(self.error(
                    function,
                    format!(
                        "type `{}` cannot adapt to `{}`: missing accessible method `{}`",
                        self.describe(&actual),
                        expected_definition.name,
                        method.name
                    ),
                ));
            };
            let Ty::Callable {
                parameters,
                parameter_modes,
                result,
                effects,
                suspends,
                ..
            } = self.resolved(actual_method)
            else {
                unreachable!("structural method lookup returns a callable")
            };
            let mut allowed_effects = method.effects.clone();
            allowed_effects.push(crate::ast::Effect {
                kind: crate::ast::EffectKind::Read,
                target: crate::ast::GroupPath::root("self"),
            });
            if parameters.len() != method.parameters.len()
                || parameter_modes != method.parameter_modes
                || !effects_are_subset(&effects, &allowed_effects)
                || (suspends && !method.suspends)
            {
                return Err(self.error(
                    function,
                    format!(
                        "type `{}` has an incompatible implementation of method `{}`",
                        self.describe(&actual),
                        method.name
                    ),
                ));
            }
            for (expected, actual) in method.parameters.into_iter().zip(parameters) {
                self.unify(expected, actual, function)?;
            }
            self.coerce(method.result, *result, function)?;
        }
        Ok(())
    }

    fn structural_method_type(
        &mut self,
        function: FunctionId,
        actual: &Ty,
        name: &str,
    ) -> Result<Option<Ty>, FosterError> {
        let actual = self.resolved(actual.clone());
        if let Some(method) = self.builtin_collection_method(&actual, name) {
            return Ok(Some(method));
        }
        let Ty::Record(record, arguments) = actual.clone() else {
            return Ok(None);
        };
        if let Some(method) = self
            .effective_record_methods(record, &arguments)?
            .into_iter()
            .find(|method| method.name == name)
        {
            return Ok(Some(Ty::Callable {
                parameters: method.parameters,
                parameter_modes: method.parameter_modes,
                result: Box::new(method.result),
                erased: false,
                effects: method.effects,
                suspends: method.suspends,
            }));
        }
        if let Ok(method) =
            self.record_method_type(function, record, arguments.clone(), name, false, false)
        {
            return Ok(Some(method));
        }
        if let Some(field) = self.structural_field_type(function, &actual, name)? {
            return Ok(Some(Ty::Callable {
                parameters: Vec::new(),
                parameter_modes: Vec::new(),
                result: Box::new(field),
                erased: false,
                effects: vec![crate::ast::Effect {
                    kind: crate::ast::EffectKind::Read,
                    target: crate::ast::GroupPath::root("self"),
                }],
                suspends: false,
            }));
        }
        Ok(None)
    }

    fn builtin_collection_method(&self, actual: &Ty, name: &str) -> Option<Ty> {
        let element = if self.is_string_type(actual) {
            Ty::CodePoint
        } else {
            match actual {
                Ty::List(element) | Ty::Sequence(element) => (**element).clone(),
                Ty::Bytes => Ty::Byte,
                _ => return None,
            }
        };
        let result = match name {
            "empty?" => Ty::Bool,
            "length" => Ty::Int,
            "iterator" => self.core_record_type("core.iteration", "Iterator", element)?,
            _ => return None,
        };
        Some(Ty::Callable {
            parameters: Vec::new(),
            parameter_modes: Vec::new(),
            result: Box::new(result),
            erased: false,
            effects: Vec::new(),
            suspends: false,
        })
    }

    fn core_record_type(&self, module: &str, name: &str, argument: Ty) -> Option<Ty> {
        let module = self.hir.module_named(module)?;
        let record = self.hir.modules[module].records.get(name).copied()?;
        Some(Ty::Record(record, vec![argument]))
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
                let fields = self.effective_record_fields(record, &arguments)?;
                let Some(field) = fields.iter().find(|field| field.name == name).cloned() else {
                    return Ok(None);
                };
                if !field.public && definition.module != caller_module {
                    return Ok(None);
                }
                Ok(Some(field.ty))
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
        let definition = self.hir.records[record].clone();
        let arguments = definition
            .parameters
            .iter()
            .map(|_| self.fresh())
            .collect::<Vec<_>>();
        let fields = self.effective_record_fields(record, &arguments)?;
        if definition.module != self.hir.functions[function].module
            && fields.iter().any(|field| !field.public)
        {
            return Err(self.error(
                function,
                format!(
                    "record `{}` cannot be constructed outside its module because it has private fields",
                    definition.name
                ),
            ));
        }
        let mut seen = std::collections::HashSet::new();
        for (name, expression) in supplied {
            if !seen.insert(name.as_str()) {
                return Err(self.error(function, format!("field `{name}` is initialized twice")));
            }
            let field = fields
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
            let actual = self.infer_expression(function, *expression)?;
            self.coerce(field.ty.clone(), actual, function)?;
        }
        let missing = fields
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
        let definition = self.hir.records[record].clone();
        let fields = self.effective_record_fields(record, arguments)?;
        let field = fields
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
        Ok(field.ty.clone())
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
