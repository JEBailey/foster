use super::*;

impl Checker<'_> {
    pub(super) fn check_function(&mut self, function_id: FunctionId) -> Result<(), FosterError> {
        let function = &self.hir.functions[function_id];
        let signature = self.functions[&function_id].clone();
        for (local, ty) in function.parameters.iter().zip(&signature.parameters) {
            let group = reference_group(ty).unwrap_or_else(|| {
                if self.hir.locals[*local].name == "self" {
                    "self".to_owned()
                } else {
                    FRAME_GROUP.to_owned()
                }
            });
            self.local_groups.insert(*local, group);
            let local_ty = match ty {
                Ty::Reference(_, value) => (**value).clone(),
                ty => ty.clone(),
            };
            self.locals.insert(*local, local_ty);
        }

        let body = function.body.clone();
        let mut final_value = None;
        for statement in &body {
            final_value = self.check_statement(function_id, statement)?;
        }
        if let Some(final_value) = final_value {
            self.coerce(signature.result, final_value, function_id)?;
        }
        Ok(())
    }

    pub(super) fn check_statement(
        &mut self,
        function: FunctionId,
        statement: &hir::Stmt,
    ) -> Result<Option<Ty>, FosterError> {
        match statement {
            hir::Stmt::Return { value, guard } => {
                if let Some(guard) = guard {
                    let guard = self.infer_expression(function, *guard)?;
                    self.unify(Ty::Bool, guard, function)?;
                }
                let value = self.infer_expression(function, *value)?;
                let result = self.functions[&function].result.clone();
                self.coerce(result, value, function)?;
                Ok(None)
            }
            hir::Stmt::Bind { local, value } => {
                let value = self.infer_expression(function, *value)?;
                let group = reference_group(&self.resolved(value.clone()))
                    .unwrap_or_else(|| FRAME_GROUP.to_owned());
                self.local_groups.insert(*local, group);
                self.locals.insert(*local, value.clone());
                Ok(Some(value))
            }
            hir::Stmt::Assign { local, value } => {
                let value = self.infer_expression(function, *value)?;
                let local_type = match &self.locals[local] {
                    Ty::Reference(_, value) => (**value).clone(),
                    ty => ty.clone(),
                };
                self.coerce(local_type, value.clone(), function)?;
                Ok(Some(value))
            }
            hir::Stmt::Set { place, value } => {
                let place = self.infer_expression(function, *place)?;
                let value = self.infer_expression(function, *value)?;
                self.coerce(place, value.clone(), function)?;
                Ok(Some(value))
            }
            hir::Stmt::Expr(expression) => Ok(Some(self.infer_expression(function, *expression)?)),
        }
    }

    pub(super) fn infer_expression(
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
            hir::Expr::Symbol(_) => Ty::Symbol,
            hir::Expr::List(items) => {
                let element = self.fresh();
                for item in items {
                    let item = self.infer_expression(function, item)?;
                    self.unify(element.clone(), item, function)?;
                }
                Ty::List(Box::new(element))
            }
            hir::Expr::Name(name) => self.type_of_name(name)?,
            hir::Expr::Call { callee, arguments } => {
                self.infer_call(function, callee, &arguments)?
            }
            hir::Expr::Member { object, name } => {
                let object = self.infer_expression(function, object)?;
                self.infer_member(function, object, &name)?
            }
            hir::Expr::Index { object, index } => {
                let object = self.infer_expression(function, object)?;
                let index = self.infer_expression(function, index)?;
                self.unify(Ty::Int, index, function)?;
                match self.resolved(object.clone()) {
                    Ty::Bytes | Ty::ByteBuffer => Ty::Byte,
                    Ty::List(element) => *element,
                    Ty::Variable(_) => {
                        let element = self.fresh();
                        self.unify(object, Ty::List(Box::new(element.clone())), function)?;
                        element
                    }
                    other => {
                        return Err(self.error(
                            function,
                            format!("type `{}` does not support indexing", self.describe(&other)),
                        ));
                    }
                }
            }
            hir::Expr::Reference(place) => {
                let value = self.infer_expression(function, place)?;
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
            hir::Expr::Await(future) => {
                let result = self.fresh();
                let future = self.infer_expression(function, future)?;
                self.unify(future, Ty::Future(Box::new(result.clone())), function)?;
                result
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
                    let value = self.infer_expression(function, arm.value)?;
                    self.unify(result.clone(), value, function)?;
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
                result
            }
            hir::Expr::Closure {
                function: closure, ..
            } => {
                self.infer_partial_parameter_modes(closure)?;
                let signature = &self.functions[&closure];
                Ty::Callable {
                    parameters: signature.parameters.clone(),
                    parameter_modes: signature.parameter_modes.clone(),
                    result: Box::new(signature.result.clone()),
                    erased: false,
                    effects: callable_effects(self.hir, closure),
                    suspends: self.hir.functions[closure].suspends,
                }
            }
        };
        self.expressions.insert(expression_id, ty.clone());
        Ok(ty)
    }

    fn infer_partial_parameter_modes(&mut self, closure: FunctionId) -> Result<(), FosterError> {
        let definition = &self.hir.functions[closure];
        if !definition
            .parameters
            .iter()
            .any(|local| self.hir.locals[*local].name.starts_with("$partial"))
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
                parameter_modes, ..
            } = self.resolved(callee)
            else {
                continue;
            };
            for (argument, mode) in arguments.iter().zip(parameter_modes) {
                let hir::Expr::Name(ResolvedName::Local(local)) = self.hir.expressions[*argument]
                else {
                    continue;
                };
                if mode == crate::ast::ParameterMode::Consume
                    && let Some(index) = parameters.iter().position(|parameter| *parameter == local)
                {
                    updates.push(index);
                }
            }
        }
        for index in updates {
            self.functions
                .get_mut(&closure)
                .expect("partial closure has a signature")
                .parameter_modes[index] = crate::ast::ParameterMode::Consume;
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
        let left = self.infer_expression(function, left)?;
        let right = self.infer_expression(function, right)?;
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
                    self.unify(left, right, function)?;
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
            ResolvedName::Local(local) => match &self.locals[&local] {
                Ty::Reference(_, value) => (**value).clone(),
                ty => ty.clone(),
            },
            ResolvedName::Constant(constant) => self.constants[&constant].clone(),
            ResolvedName::Function(function) => {
                let signature = self.functions[&function].clone();
                let mut generics = HashMap::new();
                Ty::Callable {
                    parameters: signature
                        .parameters
                        .into_iter()
                        .map(|ty| self.instantiate(ty, &mut generics))
                        .collect(),
                    parameter_modes: signature.parameter_modes.clone(),
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
                if definition.payload.is_empty() {
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
            Ty::List(element) => Ty::List(Box::new(self.instantiate(*element, generics))),
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
                parameter_modes,
                result,
                erased,
                effects,
                suspends,
            } => Ty::Callable {
                parameters: parameters
                    .into_iter()
                    .map(|ty| self.instantiate(ty, generics))
                    .collect(),
                parameter_modes,
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
