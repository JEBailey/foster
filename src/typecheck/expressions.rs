use super::*;

impl Checker<'_> {
    pub(super) fn check_function(&mut self, function_id: FunctionId) -> Result<(), FosterError> {
        let result = self.check_function_unlocated(function_id);
        result.map_err(|error| {
            let definition = &self.hir.functions[function_id];
            error.with_fallback_location(
                self.hir.modules[definition.module].name.clone(),
                definition.span.clone(),
                "type checking failed in this function",
            )
        })
    }

    fn check_function_unlocated(&mut self, function_id: FunctionId) -> Result<(), FosterError> {
        let function = &self.hir.functions[function_id];
        let signature = self.functions[&function_id].clone();
        for (local, ty) in function.parameters.iter().zip(&signature.parameters) {
            let group = reference_group(&ty.ty).unwrap_or_else(|| {
                if function.receiver == Some(local.local) {
                    "self".to_owned()
                } else {
                    FRAME_GROUP.to_owned()
                }
            });
            self.local_groups.insert(local.local, group);
            let local_ty = match &ty.ty {
                Ty::Reference(_, value) => (**value).clone(),
                ty => ty.clone(),
            };
            self.locals.insert(local.local, local_ty);
        }

        let body = function.body.clone();
        let mut final_value = None;
        let mut final_expression = None;
        let mut divergent = false;
        for (index, statement) in body.iter().enumerate() {
            final_value = if index + 1 == body.len()
                && let hir::Stmt::Expr(expression) = statement
                && (!divergent || !matches!(self.hir.expressions[*expression], hir::Expr::Unit))
            {
                Some(self.check_expression(function_id, *expression, signature.result.clone())?)
            } else {
                self.check_statement(function_id, statement)?
            };
            divergent |= final_value
                .as_ref()
                .is_some_and(|ty| self.resolved(ty.clone()) == Ty::Never);
            final_expression = match statement {
                hir::Stmt::Return { value, .. }
                | hir::Stmt::Bind { value, .. }
                | hir::Stmt::Assign { value, .. }
                | hir::Stmt::Set { value, .. }
                | hir::Stmt::Expr(value) => Some(*value),
                hir::Stmt::Assert { condition, .. } => Some(*condition),
                hir::Stmt::Loop { .. } | hir::Stmt::Break { .. } | hir::Stmt::Continue { .. } => {
                    None
                }
            };
        }
        let final_value = if divergent {
            Some(Ty::Never)
        } else {
            final_value
        };
        if let Some(final_value) = final_value {
            if let Some(final_expression) = final_expression {
                self.coerce_expression(
                    signature.result,
                    final_value,
                    function_id,
                    final_expression,
                )
                .map_err(|error| {
                    self.error_at_expression(
                        error,
                        function_id,
                        final_expression,
                        "function result has an incompatible type",
                    )
                })?;
            } else {
                self.unify(signature.result, final_value, function_id)?;
            }
        }
        Ok(())
    }

    pub(super) fn check_statement(
        &mut self,
        function: FunctionId,
        statement: &hir::Stmt,
    ) -> Result<Option<Ty>, FosterError> {
        match statement {
            hir::Stmt::Return {
                value: value_expression,
                guard,
            } => {
                if let Some(guard_expression) = guard {
                    let guard = self.infer_expression(function, *guard_expression)?;
                    self.unify(Ty::Bool, guard, function).map_err(|error| {
                        self.error_at_expression(
                            error,
                            function,
                            *guard_expression,
                            "return guard must be Bool",
                        )
                    })?;
                }
                let result = self.functions[&function].result.clone();
                self.check_expression(function, *value_expression, result)
                    .map_err(|error| {
                        self.error_at_expression(
                            error,
                            function,
                            *value_expression,
                            "returned value has an incompatible type",
                        )
                    })?;
                Ok(Some(if guard.is_none() { Ty::Never } else { Ty::Unit }))
            }
            hir::Stmt::Assert { condition, message } => {
                self.check_expression(function, *condition, Ty::Bool)
                    .map_err(|error| {
                        self.error_at_expression(
                            error,
                            function,
                            *condition,
                            "assertion condition must be Bool",
                        )
                    })?;
                if let Some(message) = message {
                    self.check_expression(function, *message, self.string_type())
                        .map_err(|error| {
                            self.error_at_expression(
                                error,
                                function,
                                *message,
                                "assertion message must be String",
                            )
                        })?;
                }
                Ok(Some(
                    if matches!(self.hir.expressions[*condition], hir::Expr::Bool(false)) {
                        Ty::Never
                    } else {
                        Ty::Unit
                    },
                ))
            }
            hir::Stmt::Loop { body, .. } => {
                for statement in body {
                    self.check_statement(function, statement)?;
                }
                Ok(Some(if self.loop_can_break(body) {
                    Ty::Unit
                } else {
                    Ty::Never
                }))
            }
            hir::Stmt::Break { guard } | hir::Stmt::Continue { guard } => {
                if let Some(guard) = guard {
                    self.check_expression(function, *guard, Ty::Bool)
                        .map_err(|error| {
                            self.error_at_expression(
                                error,
                                function,
                                *guard,
                                "control guard must be Bool",
                            )
                        })?;
                }
                Ok(Some(if guard.is_none() { Ty::Never } else { Ty::Unit }))
            }
            hir::Stmt::Bind { local, value } => {
                let value = self.infer_expression(function, *value)?;
                let group = reference_group(&self.resolved(value.clone()))
                    .unwrap_or_else(|| FRAME_GROUP.to_owned());
                self.local_groups.insert(*local, group);
                self.locals.insert(*local, value.clone());
                Ok(Some(value))
            }
            hir::Stmt::Assign {
                local,
                value: value_expression,
            } => {
                let structural = self
                    .type_facts
                    .iter()
                    .any(|(subject, ty)| subject == local && self.structural_pattern_type(ty));
                // Other arm bindings may alias this same place.
                self.type_facts.clear();
                if self.hir.locals[*local].kind == hir::LocalKind::TypePattern && !structural {
                    return Err(self.error(
                        function,
                        "mutation through a narrowed type-pattern binding is not supported yet",
                    ));
                }
                let local_type = match &self.locals[local] {
                    Ty::Reference(_, value) => (**value).clone(),
                    ty => ty.clone(),
                };
                let value = self
                    .check_expression(function, *value_expression, local_type)
                    .map_err(|error| {
                        self.error_at_expression(
                            error,
                            function,
                            *value_expression,
                            "assigned value has an incompatible type",
                        )
                    })?;
                Ok(Some(value))
            }
            hir::Stmt::Set {
                place,
                value: value_expression,
            } => {
                let place_type = self.infer_expression(function, *place)?;
                if self.is_type_pattern_place(*place) {
                    return Err(self.error(
                        function,
                        "mutation through a narrowed type-pattern binding is not supported yet",
                    ));
                }
                if crate::semantics::expression_category(self.hir, &self.member_kinds, *place)
                    != crate::semantics::ExpressionCategory::Place
                {
                    return Err(self.error_at_expression(
                        self.error(function, "left side of assignment is not a stored place"),
                        function,
                        *place,
                        "this expression produces a value rather than designating storage",
                    ));
                }
                let value = self
                    .check_expression(function, *value_expression, place_type)
                    .map_err(|error| {
                        self.error_at_expression(
                            error,
                            function,
                            *value_expression,
                            "stored value has an incompatible type",
                        )
                    })?;
                Ok(Some(value))
            }
            hir::Stmt::Expr(expression) => Ok(Some(self.infer_expression(function, *expression)?)),
        }
    }

    fn check_branch_body(
        &mut self,
        function: FunctionId,
        body: &crate::block::Block<hir::Stmt>,
        expected: Option<Ty>,
    ) -> Result<Option<Ty>, FosterError> {
        if body.is_empty() {
            return Ok(Some(Ty::Unit));
        }
        let mut divergent = false;
        let mut result = None;
        for (index, statement) in body.iter().enumerate() {
            let value = if index + 1 == body.len()
                && let hir::Stmt::Expr(expression) = statement
                && (!divergent || !matches!(self.hir.expressions[*expression], hir::Expr::Unit))
            {
                Some(if let Some(expected) = expected.clone() {
                    self.check_expression(function, *expression, expected)?
                } else {
                    self.infer_expression(function, *expression)?
                })
            } else {
                self.check_statement(function, statement)?
            };
            if value
                .as_ref()
                .is_some_and(|ty| self.resolved(ty.clone()) == Ty::Never)
            {
                divergent = true;
            }
            result = value;
        }
        if divergent {
            return Ok(Some(Ty::Never));
        }
        if matches!(body.last(), Some(hir::Stmt::Expr(_))) {
            return Ok(result);
        }
        Err(self.error(
            function,
            "branch-arm block must end with a value or an unconditional control transfer",
        ))
    }

    fn loop_can_break(&self, body: &crate::block::Block<hir::Stmt>) -> bool {
        for statement in body {
            let nested_break = match statement {
                hir::Stmt::Return { value, guard } => {
                    self.expression_can_break(*value)
                        || guard.is_some_and(|id| self.expression_can_break(id))
                }
                hir::Stmt::Break { guard } | hir::Stmt::Continue { guard } => {
                    guard.is_some_and(|id| self.expression_can_break(id))
                }
                hir::Stmt::Assert { condition, message } => {
                    self.expression_can_break(*condition)
                        || message.is_some_and(|id| self.expression_can_break(id))
                }
                _ => false,
            };
            if nested_break {
                return true;
            }
            match statement {
                hir::Stmt::Break { guard } => {
                    if !guard.is_some_and(|id| {
                        matches!(self.hir.expressions[id], hir::Expr::Bool(false))
                    }) {
                        return true;
                    }
                }
                hir::Stmt::Loop { body } if !self.loop_can_break(body) => break,
                hir::Stmt::Return { guard: None, .. } | hir::Stmt::Continue { guard: None } => {
                    break;
                }
                hir::Stmt::Assert { condition, .. }
                    if matches!(self.hir.expressions[*condition], hir::Expr::Bool(false)) =>
                {
                    break;
                }
                hir::Stmt::Expr(id)
                | hir::Stmt::Bind { value: id, .. }
                | hir::Stmt::Assign { value: id, .. }
                | hir::Stmt::Set { value: id, .. } => {
                    if self.expression_can_break(*id) {
                        return true;
                    }
                    if self
                        .expressions
                        .get(id)
                        .is_some_and(|ty| self.resolved(ty.clone()) == Ty::Never)
                    {
                        break;
                    }
                }
                _ => {}
            }
        }
        false
    }

    fn expression_can_break(&self, id: ExprId) -> bool {
        match &self.hir.expressions[id] {
            hir::Expr::Branch { subject, arms } => {
                if subject.is_some_and(|id| self.expression_can_break(id)) {
                    return true;
                }
                for arm in arms {
                    if let hir::BranchTest::Condition(id) = arm.test
                        && self.expression_can_break(id)
                    {
                        return true;
                    }
                    if let hir::BranchTest::Condition(condition) = arm.test
                        && matches!(self.hir.expressions[condition], hir::Expr::Bool(false))
                    {
                        continue;
                    }
                    if self.loop_can_break(&arm.body) {
                        return true;
                    }
                    if matches!(arm.test, hir::BranchTest::Wildcard)
                        || matches!(arm.test, hir::BranchTest::Condition(id) if matches!(self.hir.expressions[id], hir::Expr::Bool(true)))
                    {
                        break;
                    }
                }
                false
            }
            hir::Expr::Call { callee, arguments } => {
                self.expression_can_break(*callee)
                    || arguments.iter().any(|id| self.expression_can_break(*id))
            }
            hir::Expr::List(items) => items.iter().any(|id| self.expression_can_break(*id)),
            hir::Expr::Record { fields, .. } => {
                fields.iter().any(|(_, id)| self.expression_can_break(*id))
            }
            hir::Expr::Binary { left, right, .. } => {
                self.expression_can_break(*left) || self.expression_can_break(*right)
            }
            hir::Expr::Unary { operand, .. }
            | hir::Expr::Reference(operand)
            | hir::Expr::MoveOut(operand)
            | hir::Expr::Remote(operand)
            | hir::Expr::Await(operand)
            | hir::Expr::Panic(operand)
            | hir::Expr::Try { value: operand, .. } => self.expression_can_break(*operand),
            hir::Expr::Member { object, .. } => self.expression_can_break(*object),
            hir::Expr::Index { object, index } => {
                self.expression_can_break(*object) || self.expression_can_break(*index)
            }
            _ => false,
        }
    }

    pub(super) fn check_expression(
        &mut self,
        function: FunctionId,
        expression_id: ExprId,
        expected: Ty,
    ) -> Result<Ty, FosterError> {
        let expected = self.resolved(expected);
        let expression = self.hir.expressions[expression_id].clone();

        if let hir::Expr::List(items) = expression.clone()
            && let Some(element) = self.list_element(&expected)
        {
            for item in items {
                self.check_expression(function, item, element.clone())?;
            }
            self.expressions.insert(expression_id, expected.clone());
            return Ok(expected);
        }

        if let hir::Expr::Branch { subject, arms } = expression {
            if arms.is_empty() {
                return Err(self.error(function, "branch expression has no arms"));
            }
            if subject.is_none()
                && !arms
                    .iter()
                    .any(|arm| matches!(arm.test, hir::BranchTest::Wildcard))
            {
                return Err(self.error(function, "branch expression requires a `_` arm"));
            }
            let subject_ty = subject
                .map(|subject| self.infer_expression(function, subject))
                .transpose()?;
            let mut yields = false;
            let mut covered = std::collections::HashSet::new();
            let mut catch_all = false;
            for arm in arms {
                if let hir::BranchTest::Condition(condition) = arm.test {
                    let condition = self.infer_expression(function, condition)?;
                    self.unify(Ty::Bool, condition, function)?;
                } else if let hir::BranchTest::Pattern(pattern) = &arm.test {
                    self.check_pattern(
                        function,
                        pattern,
                        subject_ty.clone().expect("pattern branch has subject"),
                        &mut covered,
                        &mut catch_all,
                        true,
                    )?;
                }
                let checkpoint = self.type_facts.len();
                self.enter_type_pattern(subject, &arm.test);
                let checked = self.check_branch_body(function, &arm.body, Some(expected.clone()));
                self.type_facts.truncate(checkpoint);
                if let Some(value) = checked?
                    && self.resolved(value.clone()) != Ty::Never
                {
                    yields = true;
                    self.unify(expected.clone(), value, function)?;
                }
            }
            if let Some(Ty::Variant(parent, _)) = subject_ty.map(|ty| self.resolved(ty)) {
                let required = self.hir.variant_types[parent].alternatives.len();
                if !catch_all && covered.len() != required {
                    return Err(self.error(
                        function,
                        format!(
                            "non-exhaustive branch on `{}`",
                            self.hir.variant_types[parent].name
                        ),
                    ));
                }
            } else if subject.is_some() && !catch_all {
                return Err(self.error(function, "pattern branch requires `_` for exhaustiveness"));
            }
            let result = if yields { expected } else { Ty::Never };
            self.expressions.insert(expression_id, result.clone());
            return Ok(result);
        }

        let actual = self.infer_expression(function, expression_id)?;
        let never = self.resolved(actual.clone()) == Ty::Never;
        self.coerce_expression(expected.clone(), actual, function, expression_id)?;
        Ok(if never {
            Ty::Never
        } else {
            self.resolved(expected)
        })
    }

    pub(super) fn infer_expression(
        &mut self,
        function: FunctionId,
        expression_id: ExprId,
    ) -> Result<Ty, FosterError> {
        crate::compiler::cancellation::check()?;
        let result = self.infer_expression_unlocated(function, expression_id);
        if let hir::Expr::Call {
            callee,
            ref arguments,
        } = self.hir.expressions[expression_id]
            && let Some(Ty::Callable { effects, .. }) = self.expressions.get(&callee)
            && effects
                .iter()
                .any(|effect| effect.kind != crate::ast::EffectKind::Read)
        {
            let narrowed_receiver = matches!(self.hir.expressions[callee], hir::Expr::Member { object, .. } if self.is_type_pattern_place(object));
            if narrowed_receiver
                || arguments
                    .iter()
                    .any(|argument| self.is_type_pattern_place(*argument))
            {
                return Err(self.error(function, "mutation or consumption through a narrowed type-pattern binding is not supported yet"));
            }
            // A mutation may replace an erased value through an alias. Require a
            // fresh capability test before dispatching through the old proof.
            self.type_facts.clear();
        }
        result.map_err(|error| {
            let label = error.message.clone();
            self.error_at_expression(error, function, expression_id, label)
        })
    }

    fn infer_expression_unlocated(
        &mut self,
        function: FunctionId,
        expression_id: ExprId,
    ) -> Result<Ty, FosterError> {
        if let Some(ty) = self.expressions.get(&expression_id) {
            return Ok(ty.clone());
        }
        let expression = self.hir.expressions[expression_id].clone();
        let ty = match expression {
            hir::Expr::Unit => Ty::Unit,
            hir::Expr::Bool(_) => Ty::Bool,
            hir::Expr::Integer(_) => Ty::Int,
            hir::Expr::Float(_) => Ty::Float,
            hir::Expr::String(_) => self.string_type(),
            hir::Expr::CodePoint(_) => Ty::CodePoint,
            hir::Expr::Symbol(_) => self.symbol_type(),
            hir::Expr::List(items) => {
                let element = self.fresh();
                for item in items {
                    let item = self.infer_expression(function, item)?;
                    if self.resolved(item.clone()) != Ty::Never {
                        self.unify(element.clone(), item, function)?;
                    }
                }
                self.list_type(element)
            }
            hir::Expr::Name(ResolvedName::Function(function_id))
                if self
                    .hir
                    .functions_named(
                        self.hir.functions[function_id].module,
                        &self.hir.functions[function_id].name,
                    )
                    .len()
                    > 1 =>
            {
                return Err(self.error(
                    function,
                    format!(
                        "overloaded function `{}` requires a call whose arguments select one overload",
                        self.hir.functions[function_id].name
                    ),
                ));
            }
            hir::Expr::Name(name) => self.type_of_name(name)?,
            hir::Expr::Call { callee, arguments } => {
                self.infer_call(function, expression_id, callee, &arguments)?
            }
            hir::Expr::Member { object, name } => {
                let original = self.infer_expression(function, object)?;
                let object = self.refined_receiver(object, original);
                let stored_field = self.has_stored_member(&object, &name)?;
                let member = self.infer_member(function, object, &name)?;
                let kind = if stored_field {
                    crate::semantics::MemberKind::StoredPlace
                } else if matches!(self.resolved(member.clone()), Ty::Callable { .. }) {
                    crate::semantics::MemberKind::Method
                } else {
                    let value_kind = if self.is_copy_type(&member) {
                        crate::semantics::ComputedValueKind::Copy
                    } else if matches!(self.resolved(member.clone()), Ty::Reference(_, _)) {
                        crate::semantics::ComputedValueKind::Borrowed
                    } else {
                        crate::semantics::ComputedValueKind::IndependentOwned
                    };
                    crate::semantics::MemberKind::ComputedValue(value_kind)
                };
                self.member_kinds.insert(expression_id, kind);
                if kind == crate::semantics::MemberKind::Method {
                    self.bare_method_members.insert(expression_id);
                }
                member
            }
            hir::Expr::Index { object, index } => {
                let object = self.infer_expression(function, object)?;
                self.check_expression(function, index, Ty::Int)?;
                if self.is_bytes_type(&object) || self.is_byte_buffer_type(&object) {
                    Ty::Byte
                } else if let Some(element) = self.list_element(&object) {
                    element
                } else {
                    match self.resolved(object.clone()) {
                        Ty::RawBytes | Ty::RawByteBuffer => Ty::Byte,
                        Ty::RawList(element) => *element,
                        Ty::Variable(_) => {
                            let element = self.fresh();
                            self.unify(object, self.list_type(element.clone()), function)?;
                            element
                        }
                        other => {
                            return Err(self.error(
                                function,
                                format!(
                                    "type `{}` does not support indexing",
                                    self.describe(&other)
                                ),
                            ));
                        }
                    }
                }
            }
            hir::Expr::Reference(place) => {
                let value = self.infer_expression(function, place)?;
                if self.is_type_pattern_place(place) {
                    return Err(self.error(
                        function,
                        "references to a narrowed type-pattern binding are not supported yet",
                    ));
                }
                Ty::Reference(self.expression_group(place), Box::new(value))
            }
            hir::Expr::MoveOut(place) => self.infer_expression(function, place)?,
            hir::Expr::Remote(value) => {
                let value = self.infer_expression(function, value)?;
                match self.resolved(value) {
                    record @ Ty::Record(_, _) => Ty::Remote(Box::new(record)),
                    Ty::Reference(group, value) if matches!(*value, Ty::Record(_, _)) => {
                        Ty::Remote(Box::new(Ty::Reference(group, value)))
                    }
                    other => {
                        return Err(self.error(
                            function,
                            format!(
                                "`remote` requires a record, found `{}`",
                                self.describe(&other)
                            ),
                        ));
                    }
                }
            }
            hir::Expr::Panic(message) => {
                self.check_expression(function, message, self.string_type())?;
                Ty::Never
            }
            hir::Expr::Await(future) => {
                let result = self.fresh();
                let future = self.infer_expression(function, future)?;
                self.unify(future, Ty::Future(Box::new(result.clone())), function)?;
                result
            }
            hir::Expr::Try {
                value,
                binding,
                variant: Some(variant),
            } => {
                let operand = self.infer_expression(function, value)?;
                let Ty::Variant(parent, arguments) = self.resolved(operand) else {
                    return Err(self.error(function, "`try<Variant>` requires an enum value"));
                };
                let definition = self.hir.variant_types[parent].clone();
                if definition.kind != crate::ast::VariantKind::Enum {
                    return Err(self.error(function, "`try<Variant>` requires an enum value"));
                }
                let selected = definition
                    .alternatives
                    .iter()
                    .copied()
                    .find(|id| self.hir.variants[*id].name == variant)
                    .ok_or_else(|| {
                        self.error(
                            function,
                            format!("enum `{}` has no variant `{variant}`", definition.name),
                        )
                    })?;
                let generics = definition
                    .parameters
                    .iter()
                    .cloned()
                    .zip(arguments)
                    .collect::<HashMap<_, _>>();
                let success = match self.hir.variants[selected].payload.clone() {
                    Some(annotation) => {
                        self.annotation_type(definition.module, &annotation, &generics)?
                    }
                    None => Ty::Unit,
                };
                let mut returned = self.resolved(self.functions[&function].result.clone());
                if matches!(returned, Ty::Variable(_)) {
                    let arguments = definition.parameters.iter().map(|_| self.fresh()).collect();
                    let inferred = Ty::Variant(parent, arguments);
                    self.unify(returned, inferred.clone(), function)?;
                    returned = inferred;
                }
                let Ty::Variant(output, output_arguments) = returned else {
                    return Err(self.error(function, "`try<Variant>` requires an enum return type"));
                };
                let output_definition = self.hir.variant_types[output].clone();
                if output_definition.kind != crate::ast::VariantKind::Enum {
                    return Err(self.error(function, "`try<Variant>` requires an enum return type"));
                }
                let output_generics = output_definition
                    .parameters
                    .iter()
                    .cloned()
                    .zip(output_arguments)
                    .collect::<HashMap<_, _>>();
                for other in definition
                    .alternatives
                    .iter()
                    .copied()
                    .filter(|id| *id != selected)
                {
                    let case = self.hir.variants[other].clone();
                    let target = output_definition
                        .alternatives
                        .iter()
                        .copied()
                        .find(|id| self.hir.variants[*id].name == case.name)
                        .ok_or_else(|| {
                            self.error(
                                function,
                                format!(
                                    "`try<{variant}>` cannot propagate `{}` into `{}`",
                                    case.name, output_definition.name
                                ),
                            )
                        })?;
                    match (case.payload, self.hir.variants[target].payload.clone()) {
                        (Some(source), Some(target)) => {
                            let source = self.annotation_type(definition.module, &source, &generics)?;
                            let target = self.annotation_type(output_definition.module, &target, &output_generics)?;
                            self.unify(source, target, function).map_err(|_| self.error(function,
                                format!("`try<{variant}>` requires matching payload types for propagated variant `{}`", case.name)))?;
                        }
                        (None, None) => {}
                        _ => return Err(self.error(function, format!("`try<{variant}>` requires matching payloads for propagated variant `{}`", case.name))),
                    }
                }
                self.locals.insert(binding, success.clone());
                success
            }
            hir::Expr::Try {
                value,
                binding,
                variant: None,
            } => {
                let result_module = self.hir.module_named("core.result").ok_or_else(|| {
                    FosterError::runtime("`try` requires the embedded `core.result` module")
                })?;
                let result_type = self
                    .hir
                    .variant_type_named(result_module, "Result")
                    .ok_or_else(|| FosterError::runtime("`try` requires `core.result.Result`"))?;
                let success = self.fresh();
                let error = self.fresh();
                let operand = self.infer_expression(function, value)?;
                self.unify(
                    Ty::Variant(result_type, vec![success.clone(), error.clone()]),
                    operand,
                    function,
                )
                .map_err(|_| {
                    self.error(
                        function,
                        format!(
                            "`try` requires a Result value, found `{}`",
                            self.describe(&self.resolved(self.expressions[&value].clone()))
                        ),
                    )
                })?;
                let function_success = self.fresh();
                let function_result = self.functions[&function].result.clone();
                self.unify(
                    Ty::Variant(result_type, vec![function_success, error]),
                    function_result.clone(),
                    function,
                )
                .map_err(|_| {
                    self.error(
                        function,
                        format!(
                            "`try` requires the enclosing function to return Result with the same error type, found `{}`",
                            self.describe(&self.resolved(function_result))
                        ),
                    )
                })?;
                self.locals.insert(binding, success.clone());
                success
            }
            hir::Expr::Record { record, fields } => self.infer_record(function, record, &fields)?,
            hir::Expr::Unary { operator, operand } => {
                let operand = self.infer_expression(function, operand)?;
                match operator {
                    UnaryOp::Negate => {
                        let numeric = if matches!(self.resolved(operand.clone()), Ty::Float) {
                            Ty::Float
                        } else {
                            Ty::Int
                        };
                        self.unify_numeric_operand(numeric.clone(), operand, function)?;
                        numeric
                    }
                    UnaryOp::Not => {
                        self.unify(Ty::Bool, operand, function)?;
                        Ty::Bool
                    }
                    UnaryOp::BitNot => {
                        self.unify(Ty::Byte, operand, function)?;
                        Ty::Byte
                    }
                }
            }
            hir::Expr::Binary {
                left,
                operator,
                right,
            } => self.infer_binary(function, left, operator, right)?,
            hir::Expr::Branch { subject, arms } => {
                let result = self.fresh();
                if arms.is_empty() {
                    return Err(self.error(function, "branch expression has no arms"));
                }
                if subject.is_none()
                    && !arms
                        .iter()
                        .any(|arm| matches!(arm.test, hir::BranchTest::Wildcard))
                {
                    return Err(self.error(function, "branch expression requires a `_` arm"));
                }
                let subject_ty = subject
                    .map(|s| self.infer_expression(function, s))
                    .transpose()?;
                let mut yields = false;
                let mut covered = std::collections::HashSet::new();
                let mut catch_all = false;
                for arm in arms {
                    if let hir::BranchTest::Condition(condition) = arm.test {
                        let condition = self.infer_expression(function, condition)?;
                        self.unify(Ty::Bool, condition, function)?;
                    } else if let hir::BranchTest::Pattern(pattern) = &arm.test {
                        self.check_pattern(
                            function,
                            pattern,
                            subject_ty.clone().expect("pattern branch has subject"),
                            &mut covered,
                            &mut catch_all,
                            true,
                        )?;
                    }
                    let checkpoint = self.type_facts.len();
                    self.enter_type_pattern(subject, &arm.test);
                    let checked = self.check_branch_body(function, &arm.body, None);
                    self.type_facts.truncate(checkpoint);
                    if let Some(value) = checked?
                        && self.resolved(value.clone()) != Ty::Never
                    {
                        yields = true;
                        self.unify(result.clone(), value, function)?;
                    }
                }
                if let Some(Ty::Variant(parent, _)) = subject_ty.map(|t| self.resolved(t)) {
                    let expected = self.hir.variant_types[parent].alternatives.len();
                    if !catch_all && covered.len() != expected {
                        return Err(self.error(
                            function,
                            format!(
                                "non-exhaustive branch on `{}`",
                                self.hir.variant_types[parent].name
                            ),
                        ));
                    }
                } else if subject.is_some() && !catch_all {
                    return Err(
                        self.error(function, "pattern branch requires `_` for exhaustiveness")
                    );
                }
                if yields { result } else { Ty::Never }
            }
            hir::Expr::Closure {
                function: closure,
                captures,
            } => {
                for capture in captures {
                    let Some(source) = capture.source else {
                        continue;
                    };
                    let ty = self.infer_expression(function, source)?;
                    let group = match self.resolved(ty.clone()) {
                        Ty::Reference(group, _) => group,
                        _ => self.expression_group(source),
                    };
                    self.locals.insert(capture.local, ty);
                    self.local_groups.insert(capture.local, group);
                }
                self.infer_partial_parameter_modes(closure)?;
                let signature = &self.functions[&closure];
                Ty::Callable {
                    parameters: signature.parameters.clone(),
                    result: Box::new(signature.result.clone()),
                    erased: false,
                    effects: callable_effects(self.hir, closure),
                    suspends: self.hir.functions[closure].suspends,
                }
            }
        };
        let never = |id: &ExprId| {
            self.expressions
                .get(id)
                .is_some_and(|ty| self.resolved(ty.clone()) == Ty::Never)
        };
        let diverges = match &self.hir.expressions[expression_id] {
            hir::Expr::Call { callee, arguments } => never(callee) || arguments.iter().any(never),
            hir::Expr::List(items) => items.iter().any(never),
            hir::Expr::Record { fields, .. } => fields.iter().any(|(_, id)| never(id)),
            hir::Expr::Binary { left, right, .. }
            | hir::Expr::Index {
                object: left,
                index: right,
            } => never(left) || never(right),
            hir::Expr::Unary { operand, .. }
            | hir::Expr::Reference(operand)
            | hir::Expr::MoveOut(operand)
            | hir::Expr::Remote(operand)
            | hir::Expr::Await(operand)
            | hir::Expr::Try { value: operand, .. }
            | hir::Expr::Member {
                object: operand, ..
            } => never(operand),
            hir::Expr::Branch {
                subject: Some(subject),
                ..
            } => never(subject),
            _ => false,
        };
        let ty = if diverges { Ty::Never } else { ty };
        self.expressions.insert(expression_id, ty.clone());
        Ok(ty)
    }

    pub(super) fn has_stored_member(
        &mut self,
        object: &Ty,
        name: &str,
    ) -> Result<bool, FosterError> {
        match self.resolved(object.clone()) {
            Ty::Reference(_, value) => self.has_stored_member(&value, name),
            Ty::Record(record, arguments) => Ok(self
                .effective_record_fields(record, &arguments)?
                .iter()
                .any(|field| field.name == name)),
            Ty::Intersection(members) => {
                for member in members {
                    if self.has_stored_member(&member, name)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            _ => Ok(false),
        }
    }

    fn infer_partial_parameter_modes(&mut self, closure: FunctionId) -> Result<(), FosterError> {
        let definition = &self.hir.functions[closure];
        if !definition
            .parameters
            .iter()
            .any(|local| self.hir.locals[local.local].name.starts_with("$partial"))
        {
            return Ok(());
        }
        let parameters = definition.parameters.clone();
        let calls = definition
            .body
            .iter()
            .filter_map(|statement| {
                let expression = match statement {
                    hir::Stmt::Return { value, .. }
                    | hir::Stmt::Bind { value, .. }
                    | hir::Stmt::Assign { value, .. }
                    | hir::Stmt::Set { value, .. }
                    | hir::Stmt::Expr(value) => *value,
                    hir::Stmt::Assert { .. }
                    | hir::Stmt::Loop { .. }
                    | hir::Stmt::Break { .. }
                    | hir::Stmt::Continue { .. } => return None,
                };
                match &self.hir.expressions[expression] {
                    hir::Expr::Call { callee, arguments } => Some((*callee, arguments.clone())),
                    _ => None,
                }
            })
            .collect::<Vec<_>>();
        let mut updates = Vec::new();
        for (callee, arguments) in calls {
            let callee = self.infer_expression(closure, callee)?;
            let Ty::Callable {
                parameters: callable_parameters,
                ..
            } = self.resolved(callee)
            else {
                continue;
            };
            for (argument, mode) in arguments
                .iter()
                .zip(callable_parameters.iter().map(|p| p.mode))
            {
                let hir::Expr::Name(ResolvedName::Local(local)) = self.hir.expressions[*argument]
                else {
                    continue;
                };
                if mode == crate::ast::ParameterMode::Consume
                    && let Some(index) = parameters
                        .iter()
                        .position(|parameter| parameter.local == local)
                {
                    updates.push(index);
                }
            }
        }
        for index in updates {
            self.functions
                .get_mut(&closure)
                .expect("partial closure has a signature")
                .parameters[index]
                .mode = crate::ast::ParameterMode::Consume;
        }
        Ok(())
    }

    pub(super) fn expression_group(&self, expression: ExprId) -> String {
        match self.hir.expressions[expression] {
            hir::Expr::Name(ResolvedName::Local(local)) => self
                .local_groups
                .get(&local)
                .cloned()
                .unwrap_or_else(|| FRAME_GROUP.to_owned()),
            hir::Expr::Member { object, .. }
            | hir::Expr::Index { object, .. }
            | hir::Expr::Reference(object) => self.expression_group(object),
            _ => FRAME_GROUP.to_owned(),
        }
    }
}

impl Checker<'_> {
    pub(super) fn infer_binary(
        &mut self,
        function: FunctionId,
        left: ExprId,
        operator: BinaryOp,
        right: ExprId,
    ) -> Result<Ty, FosterError> {
        let left_expression = left;
        let right_expression = right;
        let left = self.infer_expression(function, left)?;
        let right = self.infer_expression(function, right)?;
        if self.resolved(left.clone()) == Ty::Never || self.resolved(right.clone()) == Ty::Never {
            return Ok(Ty::Never);
        }
        match operator {
            BinaryOp::Add => {
                if self.is_string_type(&left) || self.is_string_type(&right) {
                    let string = self.string_type();
                    self.unify(string.clone(), left, function)?;
                    self.unify(string.clone(), right, function)?;
                    Ok(string)
                } else {
                    self.infer_numeric_binary(function, left, right)
                }
            }
            BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide => {
                self.infer_numeric_binary(function, left, right)
            }
            BinaryOp::BitAnd | BinaryOp::BitOr | BinaryOp::BitXor => {
                self.unify(Ty::Byte, left, function)?;
                self.unify(Ty::Byte, right, function)?;
                Ok(Ty::Byte)
            }
            BinaryOp::ShiftLeft | BinaryOp::ShiftRight => {
                self.unify(Ty::Byte, left, function)?;
                self.unify(Ty::Int, right, function)?;
                Ok(Ty::Byte)
            }
            BinaryOp::Less | BinaryOp::LessEqual | BinaryOp::Greater | BinaryOp::GreaterEqual => {
                self.infer_numeric_binary(function, left, right)?;
                Ok(Ty::Bool)
            }
            BinaryOp::Equal | BinaryOp::NotEqual => {
                if integer_like(&self.resolved(left.clone()))
                    && integer_like(&self.resolved(right.clone()))
                {
                    self.infer_numeric_binary(function, left, right)?;
                } else {
                    let left_description = self.describe(&self.resolved(left.clone()));
                    let right_description = self.describe(&self.resolved(right.clone()));
                    self.unify(left, right, function).map_err(|mut error| {
                        if let (Some(left), Some(right)) = (
                            self.hir.expression_spans.get(&left_expression),
                            self.hir.expression_spans.get(&right_expression),
                        ) {
                            error = error.with_primary_label(
                                left.start..right.end,
                                "comparison operands have incompatible types",
                            );
                        }
                        for (expression, side, description) in [
                            (left_expression, "left", left_description),
                            (right_expression, "right", right_description),
                        ] {
                            if let Some(span) = self.hir.expression_spans.get(&expression) {
                                error = error.with_label(
                                    span.clone(),
                                    format!("{side} operand has type `{description}`"),
                                );
                            }
                        }
                        // Keep the comparison as the primary error, with clickable operands.
                        error
                    })?;
                }
                Ok(Ty::Bool)
            }
        }
    }

    pub(super) fn infer_numeric_binary(
        &mut self,
        function: FunctionId,
        left: Ty,
        right: Ty,
    ) -> Result<Ty, FosterError> {
        let numeric = if matches!(self.resolved(left.clone()), Ty::Float)
            || matches!(self.resolved(right.clone()), Ty::Float)
        {
            Ty::Float
        } else {
            Ty::Int
        };
        self.unify_numeric_operand(numeric.clone(), left, function)?;
        self.unify_numeric_operand(numeric.clone(), right, function)?;
        Ok(numeric)
    }

    fn unify_numeric_operand(
        &mut self,
        numeric: Ty,
        operand: Ty,
        function: FunctionId,
    ) -> Result<(), FosterError> {
        if numeric == Ty::Int && matches!(self.resolved(operand.clone()), Ty::CodePoint | Ty::Byte)
        {
            Ok(())
        } else {
            self.unify(numeric, operand, function)
        }
    }

    pub(super) fn type_of_name(&mut self, name: ResolvedName) -> Result<Ty, FosterError> {
        Ok(match name {
            ResolvedName::Local(local) => {
                let ty = &self.locals[&local];
                if self.hir.locals[local].kind == hir::LocalKind::CapturedValue {
                    ty.clone()
                } else {
                    match ty {
                        Ty::Reference(_, value) => (**value).clone(),
                        ty => ty.clone(),
                    }
                }
            }
            ResolvedName::Constant(constant) => self.constants[&constant].clone(),
            ResolvedName::Function(function) => {
                let signature = self.functions[&function].clone();
                let mut generics = HashMap::new();
                Ty::Callable {
                    parameters: signature
                        .parameters
                        .into_iter()
                        .map(|parameter| parameter.map(|ty| self.instantiate(ty, &mut generics)))
                        .collect(),
                    result: Box::new(self.instantiate(signature.result, &mut generics)),
                    erased: false,
                    effects: callable_effects(self.hir, function),
                    suspends: self.hir.functions[function].suspends,
                }
            }
            ResolvedName::Module(module) => Ty::Module(self.hir.modules[module].name.clone()),
            ResolvedName::Builtin(Builtin::Print | Builtin::Println) => {
                Ty::Function(Vec::new(), Box::new(Ty::Unit))
            }
            ResolvedName::Builtin(builtin) => {
                let (parameters, result) = self.builtin_signature(builtin)?;
                Ty::Function(parameters, Box::new(result))
            }
            ResolvedName::Record(record) => {
                Ty::Module(format!("type {}", self.hir.records[record].name))
            }
            ResolvedName::Variant(variant) => {
                let definition = &self.hir.variants[variant];
                let parent = &self.hir.variant_types[definition.parent];
                let generics = parent
                    .parameters
                    .iter()
                    .map(|p| (p.clone(), self.fresh()))
                    .collect::<HashMap<_, _>>();
                let result = Ty::Variant(
                    definition.parent,
                    parent
                        .parameters
                        .iter()
                        .map(|p| generics[p].clone())
                        .collect(),
                );
                if definition.payload.is_none() {
                    result
                } else {
                    Ty::Function(
                        definition
                            .payload
                            .iter()
                            .map(|t| self.annotation_type(parent.module, t, &generics))
                            .collect::<Result<_, _>>()?,
                        Box::new(result),
                    )
                }
            }
        })
    }

    pub(super) fn instantiate(&mut self, ty: Ty, generics: &mut HashMap<String, Ty>) -> Ty {
        match ty {
            Ty::Generic(name) => generics.entry(name).or_insert_with(|| self.fresh()).clone(),
            Ty::RawList(element) => Ty::RawList(Box::new(self.instantiate(*element, generics))),
            Ty::Sequence(element) => Ty::Sequence(Box::new(self.instantiate(*element, generics))),
            Ty::Remote(value) => Ty::Remote(Box::new(self.instantiate(*value, generics))),
            Ty::Future(value) => Ty::Future(Box::new(self.instantiate(*value, generics))),
            Ty::Function(parameters, result) => Ty::Function(
                parameters
                    .into_iter()
                    .map(|ty| self.instantiate(ty, generics))
                    .collect(),
                Box::new(self.instantiate(*result, generics)),
            ),
            Ty::Callable {
                parameters,
                result,
                erased,
                effects,
                suspends,
            } => Ty::Callable {
                parameters: parameters
                    .into_iter()
                    .map(|parameter| parameter.map(|ty| self.instantiate(ty, generics)))
                    .collect(),
                result: Box::new(self.instantiate(*result, generics)),
                erased,
                effects,
                suspends,
            },
            Ty::Reference(group, value) => {
                Ty::Reference(group, Box::new(self.instantiate(*value, generics)))
            }
            Ty::Record(record, arguments) => Ty::Record(
                record,
                arguments
                    .into_iter()
                    .map(|ty| self.instantiate(ty, generics))
                    .collect(),
            ),
            Ty::Intersection(members) => Ty::Intersection(
                members
                    .into_iter()
                    .map(|member| self.instantiate(member, generics))
                    .collect(),
            ),
            Ty::Variant(variant, arguments) => Ty::Variant(
                variant,
                arguments
                    .into_iter()
                    .map(|ty| self.instantiate(ty, generics))
                    .collect(),
            ),
            concrete => concrete,
        }
    }
}

fn integer_like(ty: &Ty) -> bool {
    matches!(ty, Ty::Int | Ty::CodePoint | Ty::Byte)
}
