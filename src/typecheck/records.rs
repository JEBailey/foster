use super::*;

impl Checker<'_> {
    pub(super) fn coerce_expression(
        &mut self,
        expected: Ty,
        actual: Ty,
        function: FunctionId,
        expression: ExprId,
    ) -> Result<(), FosterError> {
        let expected = self.resolved(expected);
        let actual = self.resolved(actual);
        if expected == Ty::Int && matches!(actual, Ty::CodePoint | Ty::Byte) {
            self.integer_promotions.insert(expression);
            self.expressions.insert(expression, Ty::Int);
            return Ok(());
        }
        let Ty::Variant(union, arguments) = expected.clone() else {
            return self.coerce(expected, actual, function);
        };
        if self.hir.variant_types[union].kind == crate::ast::VariantKind::Enum {
            return self.coerce(expected, actual, function);
        }
        if matches!(actual, Ty::Variant(found, _) if found == union) {
            return self.coerce(expected, actual, function);
        }

        let definition = self.hir.variant_types[union].clone();
        let generics = definition
            .parameters
            .iter()
            .cloned()
            .zip(arguments)
            .collect::<HashMap<_, _>>();
        let initial = self.substitutions.clone();
        let mut matched_substitutions = None;
        for member in definition.alternatives {
            self.substitutions = initial.clone();
            let member_type = self.annotation_type(
                definition.module,
                self.hir.variants[member]
                    .member
                    .as_ref()
                    .expect("a union alternative has a member type"),
                &generics,
            )?;
            if self.unify(member_type, actual.clone(), function).is_ok() {
                matched_substitutions.get_or_insert_with(|| self.substitutions.clone());
            }
        }
        self.substitutions = initial;
        if let Some(substitutions) = matched_substitutions {
            self.substitutions = substitutions;
            Ok(())
        } else {
            self.coerce(expected, actual, function)
        }
    }

    pub(super) fn coerce(
        &mut self,
        expected: Ty,
        actual: Ty,
        function: FunctionId,
    ) -> Result<(), FosterError> {
        let expected = self.resolved(expected);
        let actual = self.resolved(actual);
        if let (
            Ty::Variant(expected_union, expected_arguments),
            Ty::Variant(actual_union, actual_arguments),
        ) = (expected.clone(), actual.clone())
            && self.hir.variant_types[expected_union].kind == crate::ast::VariantKind::Union
            && self.hir.variant_types[actual_union].kind == crate::ast::VariantKind::Union
        {
            return self.coerce_union_to_union(
                function,
                expected_union,
                expected_arguments,
                actual_union,
                actual_arguments,
            );
        }
        if let Ty::Variant(union, arguments) = expected.clone()
            && self.hir.variant_types[union].kind == crate::ast::VariantKind::Union
        {
            return self.coerce_to_union(function, union, arguments, actual);
        }
        if let Ty::Variant(union, arguments) = actual.clone()
            && self.hir.variant_types[union].kind == crate::ast::VariantKind::Union
        {
            return self.coerce_from_union(function, expected, union, arguments);
        }
        if let Ty::Sequence(element) = &expected
            && self.is_string_type(&actual)
        {
            return self.unify((**element).clone(), Ty::CodePoint, function);
        }
        if let Ty::Sequence(element) = &expected
            && self.is_bytes_type(&actual)
        {
            return self.unify((**element).clone(), Ty::Byte, function);
        }
        if let Ty::Sequence(element) = &expected
            && let Some(actual) = self.list_element(&actual)
        {
            return self.unify((**element).clone(), actual, function);
        }
        match (expected, actual) {
            (Ty::Sequence(expected), Ty::RawList(actual)) => {
                self.unify(*expected, *actual, function)
            }
            (Ty::Sequence(expected), Ty::RawBytes) => self.unify(*expected, Ty::Byte, function),
            (Ty::Sequence(expected), actual @ Ty::Record(_, _)) => {
                self.coerce_sequence_shape(function, *expected, actual)
            }
            (Ty::Sequence(expected), Ty::Sequence(actual)) => {
                self.unify(*expected, *actual, function)
            }
            (expected @ Ty::Intersection(_), actual @ Ty::Intersection(_)) => {
                self.unify(expected, actual, function)
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
            (Ty::Record(expected, expected_arguments), actual @ Ty::Variant(_, _)) => {
                self.coerce_record_shape(function, expected, &expected_arguments, actual)
            }
            (
                Ty::Record(expected, expected_arguments),
                actual @ (Ty::RawList(_) | Ty::Sequence(_) | Ty::RawBytes),
            ) => self.coerce_record_shape(function, expected, &expected_arguments, actual),
            (expected, actual) => self.unify(expected, actual, function),
        }
    }

    fn coerce_to_union(
        &mut self,
        function: FunctionId,
        union: VariantTypeId,
        arguments: Vec<Ty>,
        actual: Ty,
    ) -> Result<(), FosterError> {
        if matches!(&actual, Ty::Variant(found, _) if *found == union) {
            return self.unify(Ty::Variant(union, arguments), actual, function);
        }
        let definition = self.hir.variant_types[union].clone();
        let generics = definition
            .parameters
            .iter()
            .cloned()
            .zip(arguments)
            .collect::<HashMap<_, _>>();
        let initial = self.substitutions.clone();
        for alternative in definition.alternatives {
            self.substitutions = initial.clone();
            let member = self.annotation_type(
                definition.module,
                self.hir.variants[alternative]
                    .member
                    .as_ref()
                    .expect("a union alternative has a member type"),
                &generics,
            )?;
            if self.coerce(member, actual.clone(), function).is_ok() {
                return Ok(());
            }
        }
        self.substitutions = initial;
        Err(self.error(
            function,
            format!(
                "type `{}` does not satisfy union contract `{}`",
                self.describe(&actual),
                definition.name
            ),
        ))
    }

    fn coerce_from_union(
        &mut self,
        function: FunctionId,
        expected: Ty,
        union: VariantTypeId,
        arguments: Vec<Ty>,
    ) -> Result<(), FosterError> {
        let definition = self.hir.variant_types[union].clone();
        let generics = definition
            .parameters
            .iter()
            .cloned()
            .zip(arguments)
            .collect::<HashMap<_, _>>();
        for alternative in definition.alternatives {
            let member = self.annotation_type(
                definition.module,
                self.hir.variants[alternative]
                    .member
                    .as_ref()
                    .expect("a union alternative has a member type"),
                &generics,
            )?;
            self.coerce(expected.clone(), member, function)?;
        }
        Ok(())
    }

    fn coerce_union_to_union(
        &mut self,
        function: FunctionId,
        expected: VariantTypeId,
        expected_arguments: Vec<Ty>,
        actual: VariantTypeId,
        actual_arguments: Vec<Ty>,
    ) -> Result<(), FosterError> {
        if expected == actual {
            return self.unify(
                Ty::Variant(expected, expected_arguments),
                Ty::Variant(actual, actual_arguments),
                function,
            );
        }
        let definition = self.hir.variant_types[actual].clone();
        let generics = definition
            .parameters
            .iter()
            .cloned()
            .zip(actual_arguments)
            .collect::<HashMap<_, _>>();
        for alternative in definition.alternatives {
            let member = self.annotation_type(
                definition.module,
                self.hir.variants[alternative]
                    .member
                    .as_ref()
                    .expect("a union alternative has a member type"),
                &generics,
            )?;
            self.coerce_to_union(function, expected, expected_arguments.clone(), member)?;
        }
        Ok(())
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
            let actual_methods = self.structural_method_types(function, &actual, &method.name)?;
            if actual_methods.is_empty() {
                return Err(self.error(
                    function,
                    format!(
                        "type `{}` cannot adapt to `{}`: missing accessible method `{}`",
                        self.describe(&actual),
                        expected_definition.name,
                        method.name
                    ),
                ));
            }
            let mut allowed_effects = method.effects.clone();
            allowed_effects.push(crate::ast::Effect {
                kind: crate::ast::EffectKind::Read,
                target: crate::ast::GroupPath::root("self"),
            });
            let initial_substitutions = self.substitutions.clone();
            let initial_next_variable = self.next_variable;
            let mut matched = None;
            for actual_method in actual_methods {
                self.substitutions = initial_substitutions.clone();
                self.next_variable = initial_next_variable;
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
                if parameters.len() != method.parameters.len()
                    || parameter_modes != method.parameter_modes
                    || !effects_are_subset(&effects, &allowed_effects)
                    || (suspends && !method.suspends)
                {
                    continue;
                }
                let parameters_match = method
                    .parameters
                    .iter()
                    .cloned()
                    .zip(parameters)
                    .all(|(expected, actual)| self.unify(expected, actual, function).is_ok());
                if parameters_match
                    && self
                        .coerce(method.result.clone(), *result, function)
                        .is_ok()
                {
                    matched = Some((self.substitutions.clone(), self.next_variable));
                    break;
                }
            }
            let Some((substitutions, next_variable)) = matched else {
                self.substitutions = initial_substitutions;
                self.next_variable = initial_next_variable;
                return Err(self.error(
                    function,
                    format!(
                        "type `{}` has an incompatible implementation of method `{}`",
                        self.describe(&actual),
                        method.name
                    ),
                ));
            };
            self.substitutions = substitutions;
            self.next_variable = next_variable;
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
        if let Ty::Variant(variant, arguments) = actual.clone() {
            return self
                .variant_method_type(function, variant, arguments, name)
                .map(Some)
                .or(Ok(None));
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

    fn structural_method_types(
        &mut self,
        function: FunctionId,
        actual: &Ty,
        name: &str,
    ) -> Result<Vec<Ty>, FosterError> {
        let resolved = self.resolved(actual.clone());
        if let Ty::Record(record, arguments) = &resolved {
            let methods = self
                .effective_record_methods(*record, arguments)?
                .into_iter()
                .filter(|method| method.name == name)
                .map(|method| Ty::Callable {
                    parameters: method.parameters,
                    parameter_modes: method.parameter_modes,
                    result: Box::new(method.result),
                    erased: false,
                    effects: method.effects,
                    suspends: method.suspends,
                })
                .collect::<Vec<_>>();
            if !methods.is_empty() {
                return Ok(methods);
            }
        }
        Ok(self
            .structural_method_type(function, &resolved, name)?
            .into_iter()
            .collect())
    }

    fn builtin_collection_method(&self, actual: &Ty, name: &str) -> Option<Ty> {
        let element = if self.is_string_type(actual) {
            Ty::CodePoint
        } else if self.is_bytes_type(actual) {
            Ty::Byte
        } else if let Some(element) = self.list_element(actual) {
            element
        } else {
            match actual {
                Ty::RawList(element) | Ty::Sequence(element) => (**element).clone(),
                Ty::RawBytes => Ty::Byte,
                _ => return None,
            }
        };
        let result = match name {
            "empty?" => Ty::Bool,
            "length" => Ty::Int,
            "iterator" => self.core_record_type("std.iter", "Iterator", element)?,
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
            self.check_expression(function, *expression, field.ty.clone())?;
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
