use super::calls::instantiate_call_groups;
use super::composition::EffectiveMethod;
use super::*;

struct Ranked<T> {
    conversions: usize,
    value: T,
    substitutions: HashMap<u32, Ty>,
    next_variable: u32,
}

enum Selection<T> {
    None,
    Ambiguous,
    Selected(Ranked<T>),
}

fn select_best<T>(matches: Vec<Ranked<T>>) -> Selection<T> {
    let Some(best_rank) = matches.iter().map(|candidate| candidate.conversions).min() else {
        return Selection::None;
    };
    let mut best = matches
        .into_iter()
        .filter(|candidate| candidate.conversions == best_rank);
    let selected = best.next().unwrap();
    if best.next().is_some() {
        Selection::Ambiguous
    } else {
        Selection::Selected(selected)
    }
}

impl Checker<'_> {
    pub(super) fn method_key(
        &self,
        name: &str,
        parameters: &[Ty],
        modes: &[crate::ast::ParameterMode],
    ) -> MethodKey {
        let mut generics = HashMap::new();
        let mut variables = HashMap::new();
        let mut next_generic = 0;
        MethodKey {
            name: name.to_owned(),
            parameters: modes
                .iter()
                .copied()
                .zip(parameters.iter().map(|parameter| {
                    Self::dispatch_type_key(
                        parameter,
                        &mut generics,
                        &mut variables,
                        &mut next_generic,
                    )
                }))
                .collect(),
        }
    }

    fn dispatch_type_key(
        ty: &Ty,
        generics: &mut HashMap<String, u32>,
        variables: &mut HashMap<u32, u32>,
        next_generic: &mut u32,
    ) -> DispatchTypeKey {
        let mut nested = |ty| Self::dispatch_type_key(ty, generics, variables, next_generic);
        match ty {
            Ty::Variable(variable) => {
                let index = *variables.entry(*variable).or_insert_with(|| {
                    let index = *next_generic;
                    *next_generic += 1;
                    index
                });
                DispatchTypeKey::Generic(index)
            }
            Ty::Generic(name) => {
                let index = *generics.entry(name.clone()).or_insert_with(|| {
                    let index = *next_generic;
                    *next_generic += 1;
                    index
                });
                DispatchTypeKey::Generic(index)
            }
            Ty::Unit => DispatchTypeKey::Unit,
            Ty::Bool => DispatchTypeKey::Bool,
            Ty::Int => DispatchTypeKey::Int,
            Ty::Float => DispatchTypeKey::Float,
            Ty::CodePoint => DispatchTypeKey::CodePoint,
            Ty::Byte => DispatchTypeKey::Byte,
            Ty::RawBytes => DispatchTypeKey::RawBytes,
            Ty::RawByteBuffer => DispatchTypeKey::RawByteBuffer,
            Ty::Reference(_, value) => DispatchTypeKey::Reference(Box::new(nested(value))),
            Ty::RawList(value) => DispatchTypeKey::RawList(Box::new(nested(value))),
            Ty::Sequence(value) => DispatchTypeKey::Sequence(Box::new(nested(value))),
            Ty::Remote(value) => DispatchTypeKey::Remote(Box::new(nested(value))),
            Ty::Future(value) => DispatchTypeKey::Future(Box::new(nested(value))),
            Ty::Function(parameters, result) => DispatchTypeKey::Function(
                parameters
                    .iter()
                    .map(|parameter| (crate::ast::ParameterMode::Borrow, nested(parameter)))
                    .collect(),
                Box::new(nested(result)),
            ),
            Ty::Callable {
                parameters,
                parameter_modes,
                result,
                ..
            } => DispatchTypeKey::Function(
                parameter_modes
                    .iter()
                    .copied()
                    .zip(parameters.iter().map(&mut nested))
                    .collect(),
                Box::new(nested(result)),
            ),
            Ty::Record(record, arguments) => {
                DispatchTypeKey::Record(*record, arguments.iter().map(&mut nested).collect())
            }
            Ty::Intersection(members) => {
                DispatchTypeKey::Intersection(members.iter().map(&mut nested).collect())
            }
            Ty::Variant(variant, arguments) => {
                DispatchTypeKey::Variant(*variant, arguments.iter().map(&mut nested).collect())
            }
            Ty::Module(name) => DispatchTypeKey::Module(name.clone()),
        }
    }

    fn overload_argument_conversions(
        &mut self,
        caller: FunctionId,
        expected: &[Ty],
        actual: &[Ty],
    ) -> Option<usize> {
        let mut conversions = 0;
        for (expected, actual) in expected.iter().cloned().zip(actual.iter().cloned()) {
            let expected_resolved = self.resolved(expected.clone());
            let actual_resolved = self.resolved(actual.clone());
            if expected_resolved != actual_resolved {
                conversions += 1;
            }
            let widening =
                expected_resolved == Ty::Int && matches!(actual_resolved, Ty::CodePoint | Ty::Byte);
            if !widening && self.coerce(expected, actual, caller).is_err() {
                return None;
            }
        }
        Some(conversions)
    }
    pub(super) fn contract_method_overloads(
        &mut self,
        caller: FunctionId,
        object: Ty,
        name: &str,
    ) -> Result<Vec<EffectiveMethod>, FosterError> {
        match self.resolved(object) {
            Ty::Record(record, arguments) => {
                let definition = &self.hir.records[record];
                Ok(self
                    .effective_record_methods(record, &arguments)?
                    .into_iter()
                    .filter(|method| {
                        method.name == name
                            && (definition.module == self.hir.functions[caller].module
                                || method.public)
                    })
                    .collect())
            }
            Ty::Intersection(members) => {
                let mut methods = Vec::new();
                for member in members {
                    for method in self.contract_method_overloads(caller, member, name)? {
                        if !methods.iter().any(|existing: &EffectiveMethod| {
                            existing.parameters == method.parameters
                                && existing.parameter_modes == method.parameter_modes
                        }) {
                            methods.push(method);
                        }
                    }
                }
                Ok(methods)
            }
            _ => Ok(Vec::new()),
        }
    }

    pub(super) fn infer_overloaded_contract_method_call(
        &mut self,
        function: FunctionId,
        call: ExprId,
        callee: ExprId,
        arguments: &[ExprId],
        name: &str,
        overloads: &[EffectiveMethod],
    ) -> Result<Ty, FosterError> {
        let argument_types = arguments
            .iter()
            .map(|argument| self.infer_expression(function, *argument))
            .collect::<Result<Vec<_>, _>>()?;
        let initial_substitutions = self.substitutions.clone();
        let initial_next_variable = self.next_variable;
        let mut matches = Vec::new();
        for method in overloads
            .iter()
            .filter(|method| method.parameters.len() == arguments.len())
        {
            self.substitutions = initial_substitutions.clone();
            self.next_variable = initial_next_variable;
            if let Some(conversions) =
                self.overload_argument_conversions(function, &method.parameters, &argument_types)
            {
                matches.push(Ranked {
                    conversions,
                    value: method.clone(),
                    substitutions: self.substitutions.clone(),
                    next_variable: self.next_variable,
                });
            }
        }
        self.substitutions = initial_substitutions;
        self.next_variable = initial_next_variable;
        let selected = match select_best(matches) {
            Selection::None => {
                return Err(self.error_at_expression(
                    self.error(
                        function,
                        format!("no overload of contract method `{name}` accepts these arguments"),
                    ),
                    function,
                    call,
                    "arguments are incompatible with every contract overload",
                ));
            }
            Selection::Ambiguous => {
                return Err(self.error_at_expression(
                    self.error(
                        function,
                        format!("call to overloaded contract method `{name}` is ambiguous"),
                    ),
                    function,
                    call,
                    "more than one contract overload is equally specific",
                ));
            }
            Selection::Selected(selected) => selected,
        };
        self.substitutions = selected.substitutions;
        self.next_variable = selected.next_variable;
        let method = selected.value;
        let callable = Ty::Callable {
            parameters: method.parameters.clone(),
            parameter_modes: method.parameter_modes.clone(),
            result: Box::new(method.result.clone()),
            erased: false,
            effects: method.effects,
            suspends: method.suspends,
        };
        self.expressions.insert(callee, callable.clone());
        let dispatch = self.method_key(name, &method.parameters, &method.parameter_modes);
        let requirement = method.requirement;
        self.resolved_calls.insert(
            callee,
            ResolvedCall::ContractMethod {
                dispatch,
                requirement,
            },
        );
        self.check_argument_modes(function, &callable, arguments, &argument_types)?;
        let result = self.fresh();
        self.unify_call(
            function,
            call,
            callable,
            arguments,
            argument_types,
            result.clone(),
        )?;
        Ok(result)
    }

    pub(super) fn inherent_method_overloads(
        &self,
        caller: FunctionId,
        receiver: &Ty,
        name: &str,
    ) -> Vec<FunctionId> {
        let receiver = match receiver {
            Ty::Remote(value) => match value.as_ref() {
                Ty::Reference(_, value) => value.as_ref(),
                value => value,
            },
            Ty::Reference(_, value) => value.as_ref(),
            receiver => receiver,
        };
        let owner = match receiver {
            Ty::Record(record, _) => Some((
                self.hir.records[*record].module,
                self.hir.records[*record].name.as_str(),
            )),
            Ty::Variant(variant, _) => Some((
                self.hir.variant_types[*variant].module,
                self.hir.variant_types[*variant].name.as_str(),
            )),
            Ty::Bool => self
                .hir
                .module_named("core.bool")
                .map(|module| (module, "Bool")),
            Ty::Int => self
                .hir
                .module_named("core.int")
                .map(|module| (module, "Int")),
            Ty::Float => self
                .hir
                .module_named("core.float")
                .map(|module| (module, "Float")),
            Ty::CodePoint => self
                .hir
                .module_named("core.code_point")
                .map(|module| (module, "CodePoint")),
            Ty::Byte => self
                .hir
                .module_named("core.byte")
                .map(|module| (module, "Byte")),
            Ty::RawBytes => self
                .hir
                .module_named("core.bytes")
                .map(|module| (module, "RawBytes")),
            Ty::RawByteBuffer => self
                .hir
                .module_named("core.bytes.buffer")
                .map(|module| (module, "RawByteBuffer")),
            _ => None,
        };
        let Some((module, owner)) = owner else {
            return Vec::new();
        };
        self.hir
            .functions_named(module, &format!("{owner}.{name}"))
            .iter()
            .copied()
            .filter(|function| {
                self.hir.functions[*function].receiver.is_some()
                    && (module == self.hir.functions[caller].module
                        || self.hir.functions[*function].public)
            })
            .collect()
    }

    pub(super) fn infer_overloaded_method_call(
        &mut self,
        function: FunctionId,
        call: ExprId,
        callee: ExprId,
        arguments: &[ExprId],
        receiver: Ty,
        overloads: &[FunctionId],
    ) -> Result<Ty, FosterError> {
        let name = self.hir.functions[overloads[0]]
            .name
            .rsplit_once('.')
            .map_or_else(
                || self.hir.functions[overloads[0]].name.clone(),
                |(_, name)| name.to_owned(),
            );
        let receiver = self.resolved(receiver);
        let (receiver, remote, remote_read_only) = match receiver {
            Ty::Remote(value) => match *value {
                Ty::Reference(_, value) => (*value, true, true),
                value => (value, true, false),
            },
            Ty::Reference(_, value) => (*value, false, false),
            value => (value, false, false),
        };
        let argument_types = arguments
            .iter()
            .map(|argument| self.infer_expression(function, *argument))
            .collect::<Result<Vec<_>, _>>()?;
        let arity_matches = overloads
            .iter()
            .copied()
            .filter(|candidate| self.functions[candidate].parameters.len() == arguments.len() + 1)
            .collect::<Vec<_>>();
        if arity_matches.is_empty() {
            return Err(self.error_at_expression(
                self.error(
                    function,
                    format!(
                        "no overload of method `{name}` accepts {} argument(s)",
                        arguments.len()
                    ),
                ),
                function,
                call,
                "argument count does not match any method overload",
            ));
        }
        let initial_substitutions = self.substitutions.clone();
        let initial_next_variable = self.next_variable;
        let mut matches = Vec::new();
        for candidate in arity_matches {
            self.substitutions = initial_substitutions.clone();
            self.next_variable = initial_next_variable;
            let callable = self.type_of_name(ResolvedName::Function(candidate))?;
            let Ty::Callable {
                mut parameters,
                mut parameter_modes,
                result,
                erased,
                effects,
                suspends,
            } = callable
            else {
                unreachable!("declared methods have callable types")
            };
            let expected_receiver = parameters.remove(0);
            parameter_modes.remove(0);
            if self
                .coerce(expected_receiver, receiver.clone(), function)
                .is_err()
            {
                continue;
            }
            if let Some(conversions) =
                self.overload_argument_conversions(function, &parameters, &argument_types)
            {
                matches.push(Ranked {
                    conversions,
                    value: (
                        candidate,
                        Ty::Callable {
                            parameters,
                            parameter_modes,
                            result,
                            erased,
                            effects,
                            suspends,
                        },
                    ),
                    substitutions: self.substitutions.clone(),
                    next_variable: self.next_variable,
                });
            }
        }
        self.substitutions = initial_substitutions;
        self.next_variable = initial_next_variable;
        let selected = match select_best(matches) {
            Selection::None => {
                return Err(self.error_at_expression(
                    self.error(
                        function,
                        format!("no overload of method `{name}` accepts these argument types"),
                    ),
                    function,
                    call,
                    "arguments are incompatible with every method overload",
                ));
            }
            Selection::Ambiguous => {
                return Err(self.error_at_expression(
                    self.error(
                        function,
                        format!("call to overloaded method `{name}` is ambiguous"),
                    ),
                    function,
                    call,
                    "more than one method overload is equally specific",
                ));
            }
            Selection::Selected(selected) => selected,
        };
        self.substitutions = selected.substitutions;
        self.next_variable = selected.next_variable;
        let (selected, callable) = selected.value;
        let callable = if remote {
            self.remote_overloaded_method_type(
                function,
                selected,
                &name,
                callable,
                remote_read_only,
            )?
        } else {
            callable
        };
        self.expressions.insert(callee, callable.clone());
        self.resolved_calls.insert(
            callee,
            ResolvedCall::Method {
                function: selected,
                remote,
            },
        );
        let callable = instantiate_call_groups(callable, &argument_types);
        self.check_argument_modes(function, &callable, arguments, &argument_types)?;
        let result = self.fresh();
        self.unify_call(
            function,
            call,
            callable,
            arguments,
            argument_types,
            result.clone(),
        )?;
        Ok(result)
    }

    fn remote_overloaded_method_type(
        &mut self,
        caller: FunctionId,
        method: FunctionId,
        name: &str,
        callable: Ty,
        remote_read_only: bool,
    ) -> Result<Ty, FosterError> {
        let definition = &self.hir.functions[method];
        if remote_read_only
            && definition.effects.iter().any(|effect| {
                effect.target.root == "self" && effect.kind != crate::ast::EffectKind::Read
            })
        {
            return Err(self.error(
                caller,
                format!("read-only remote loan cannot call mutating method `{name}`"),
            ));
        }
        let Ty::Callable {
            parameters,
            parameter_modes,
            result,
            erased,
            ..
        } = callable
        else {
            unreachable!("declared methods have callable types")
        };
        for (parameter, mode) in definition.parameters.iter().skip(1).zip(&parameter_modes) {
            let parameter_name = &self.hir.locals[*parameter].name;
            if *mode == crate::ast::ParameterMode::Borrow
                && definition.effects.iter().any(|effect| {
                    effect.target.root == *parameter_name
                        && effect.kind != crate::ast::EffectKind::Read
                })
            {
                return Err(self.error(
                    caller,
                    format!(
                        "remote borrowed parameter `{parameter_name}` may only have read effects"
                    ),
                ));
            }
        }
        if !remote_transferable(&self.resolved((*result).clone())) {
            return Err(self.error(
                caller,
                format!(
                    "remote method `{name}` returns `{}`, which cannot cross a remote-object boundary",
                    self.describe(&result)
                ),
            ));
        }
        Ok(Ty::Callable {
            parameters,
            parameter_modes,
            result: Box::new(Ty::Future(result)),
            erased,
            effects: Vec::new(),
            suspends: false,
        })
    }

    pub(super) fn infer_overloaded_function_call(
        &mut self,
        function: FunctionId,
        call: ExprId,
        callee: ExprId,
        arguments: &[ExprId],
        name: &str,
        overloads: &[FunctionId],
    ) -> Result<Ty, FosterError> {
        let argument_types = arguments
            .iter()
            .map(|argument| self.infer_expression(function, *argument))
            .collect::<Result<Vec<_>, _>>()?;
        let arity_matches = overloads
            .iter()
            .copied()
            .filter(|candidate| self.functions[candidate].parameters.len() == arguments.len())
            .collect::<Vec<_>>();
        if arity_matches.is_empty() {
            let mut arities = overloads
                .iter()
                .map(|candidate| self.functions[candidate].parameters.len())
                .collect::<Vec<_>>();
            arities.sort_unstable();
            arities.dedup();
            return Err(self.error_at_expression(
                self.error(
                    function,
                    format!(
                        "no overload of `{name}` accepts {} argument(s); available arities: {}",
                        arguments.len(),
                        arities
                            .iter()
                            .map(usize::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ),
                function,
                call,
                "argument count does not match any overload",
            ));
        }

        let initial_substitutions = self.substitutions.clone();
        let initial_next_variable = self.next_variable;
        let mut matches = Vec::new();
        for candidate in arity_matches {
            self.substitutions = initial_substitutions.clone();
            self.next_variable = initial_next_variable;
            let callable = self.type_of_name(ResolvedName::Function(candidate))?;
            let Ty::Callable { parameters, .. } = self.resolved(callable.clone()) else {
                unreachable!("declared functions have callable types")
            };
            if let Some(conversions) =
                self.overload_argument_conversions(function, &parameters, &argument_types)
            {
                matches.push(Ranked {
                    conversions,
                    value: (candidate, callable),
                    substitutions: self.substitutions.clone(),
                    next_variable: self.next_variable,
                });
            }
        }
        self.substitutions = initial_substitutions;
        self.next_variable = initial_next_variable;
        let selected = match select_best(matches) {
            Selection::None => {
                return Err(self.error_at_expression(
                    self.error(
                        function,
                        format!(
                            "no overload of `{name}` accepts argument types ({})",
                            argument_types
                                .iter()
                                .map(|argument| self.describe(argument))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    ),
                    function,
                    call,
                    "arguments are incompatible with every overload",
                ));
            }
            Selection::Ambiguous => return Err(self.error_at_expression(
                self.error(
                    function,
                    format!(
                        "call to overloaded function `{name}` is ambiguous for argument types ({})",
                        argument_types
                            .iter()
                            .map(|argument| self.describe(argument))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ),
                function,
                call,
                "more than one overload is equally specific",
            )),
            Selection::Selected(selected) => selected,
        };
        self.substitutions = selected.substitutions;
        self.next_variable = selected.next_variable;
        let (selected, callable) = selected.value;
        self.expressions.insert(callee, callable.clone());
        self.resolved_calls
            .insert(callee, ResolvedCall::Function(selected));
        let callable = instantiate_call_groups(callable, &argument_types);
        self.check_argument_modes(function, &callable, arguments, &argument_types)?;
        let result = self.fresh();
        self.unify_call(
            function,
            call,
            callable,
            arguments,
            argument_types,
            result.clone(),
        )?;
        Ok(result)
    }
}
