use std::collections::{HashMap, HashSet};

use super::*;

impl Checker<'_> {
    pub(super) fn finish(mut self) -> Result<TypeInformation, FosterError> {
        let called = self
            .hir
            .expressions
            .iter()
            .filter_map(|(_, expression)| match expression {
                hir::Expr::Call { callee, .. } => Some(*callee),
                _ => None,
            })
            .collect::<HashSet<_>>();
        for expression in self.bare_method_members.clone() {
            if !called.contains(&expression) {
                let hir::Expr::Member { name, .. } = &self.hir.expressions[expression] else {
                    unreachable!()
                };
                let function = self.hir.expression_functions[&expression];
                return Err(self.error_at_expression(
                    self.error(
                        function,
                        format!("method `{name}` must be called with parentheses"),
                    ),
                    function,
                    expression,
                    format!("method `{name}` must be called with parentheses"),
                ));
            }
        }

        let records = self
            .hir
            .records
            .iter()
            .map(|(record, definition)| {
                let arguments = definition
                    .parameters
                    .iter()
                    .map(|parameter| Ty::Generic(parameter.clone()))
                    .collect::<Vec<_>>();
                (record, arguments)
            })
            .collect::<Vec<_>>();
        let mut record_fields = HashMap::new();
        let mut record_methods = HashMap::new();
        for (record, arguments) in records {
            let fields = self
                .effective_record_fields(record, &arguments)?
                .into_iter()
                .map(|field| field.name)
                .collect::<HashSet<_>>();
            record_fields.insert(record, fields);
            let methods = self.effective_record_methods(record, &arguments)?;
            record_methods.insert(
                record,
                methods.into_iter().map(|method| method.name).collect(),
            );
        }
        let mut information = TypeInformation {
            resolved_calls: self.resolved_calls.clone(),
            integer_promotions: self.integer_promotions.clone(),
            record_names: self
                .hir
                .records
                .iter()
                .map(|(record, definition)| (record, definition.name.clone()))
                .collect(),
            record_fields,
            record_methods,
            variant_names: self
                .hir
                .variant_types
                .iter()
                .map(|(variant, definition)| (variant, definition.name.clone()))
                .collect(),
            ..TypeInformation::default()
        };
        let mut interner = HashMap::new();

        for (expression, ty) in &self.expressions {
            let ty = self.require_concrete(ty.clone(), "expression")?;
            let id = intern_type(&mut information, &mut interner, ty);
            information.expressions.insert(*expression, id);
        }
        for (local, ty) in &self.locals {
            let name = &self.hir.locals[*local].name;
            let ty = self.require_concrete(ty.clone(), &format!("local `{name}`"))?;
            let id = intern_type(&mut information, &mut interner, ty);
            information.locals.insert(*local, id);
        }
        for (constant, ty) in &self.constants {
            let name = &self.hir.constants[*constant].name;
            let ty = self.require_concrete(ty.clone(), &format!("constant `{name}`"))?;
            let id = intern_type(&mut information, &mut interner, ty);
            information.constants.insert(*constant, id);
        }
        for (function, signature) in &self.functions {
            let name = &self.hir.functions[*function].name;
            let parameters = signature
                .parameters
                .iter()
                .map(|ty| {
                    self.require_concrete(ty.clone(), &format!("parameter of `{name}`"))
                        .map(|ty| intern_type(&mut information, &mut interner, ty))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let result =
                self.require_concrete(signature.result.clone(), &format!("result of `{name}`"))?;
            let result = intern_type(&mut information, &mut interner, result);
            information.functions.insert(
                *function,
                FunctionType {
                    parameters,
                    parameter_modes: signature.parameter_modes.clone(),
                    result,
                    erased: false,
                    effects: callable_effects(self.hir, *function),
                    suspends: self.hir.functions[*function].suspends,
                },
            );
        }
        for (index, dispatch) in self.dispatch_keys.iter().enumerate() {
            let slot = DispatchSlot(index as u32);
            for (record, _) in self.hir.records.iter() {
                if let Some(function) =
                    best_dispatch_method(self.hir, &information, dispatch, |function| {
                        receiver_is_record(&information, function, record)
                    })
                {
                    information.record_dispatch.insert((record, slot), function);
                }
            }
            for (variant, definition) in self.hir.variant_types.iter() {
                if definition.kind == crate::ast::VariantKind::Enum
                    && let Some(function) =
                        best_dispatch_method(self.hir, &information, dispatch, |function| {
                            receiver_is_variant(&information, function, variant)
                        })
                {
                    information
                        .variant_dispatch
                        .insert((variant, slot), function);
                }
            }
        }
        Ok(information)
    }

    pub(super) fn require_concrete(&self, ty: Ty, context: &str) -> Result<Ty, FosterError> {
        let ty = self.resolved(ty);
        if contains_variable(&ty) {
            Err(FosterError::runtime(format!(
                "cannot infer the type of = {context}; add a type annotation"
            )))
        } else {
            Ok(ty)
        }
    }

    pub(super) fn describe(&self, ty: &Ty) -> String {
        match self.resolved(ty.clone()) {
            Ty::Variable(_) => "unknown".into(),
            Ty::Generic(name) => name,
            Ty::Unit => "()".into(),
            Ty::Bool => "Bool".into(),
            Ty::Int => "Int".into(),
            Ty::Float => "Float".into(),
            Ty::CodePoint => "CodePoint".into(),
            Ty::Byte => "Byte".into(),
            Ty::RawBytes => "RawBytes".into(),
            Ty::RawByteBuffer => "RawByteBuffer".into(),
            Ty::RawList(element) => format!("RawList<{}>", self.describe(&element)),
            Ty::Sequence(element) => format!("Sequence<{}>", self.describe(&element)),
            Ty::Remote(value) => format!("Remote<{}>", self.describe(&value)),
            Ty::Future(value) => format!("Future<{}>", self.describe(&value)),
            Ty::Function(parameters, result) => format!(
                "func({}) -> {}",
                parameters
                    .iter()
                    .map(|parameter| self.describe(parameter))
                    .collect::<Vec<_>>()
                    .join(", "),
                self.describe(&result)
            ),
            Ty::Callable {
                parameters,
                parameter_modes,
                result,
                effects,
                suspends,
                ..
            } => {
                let mut effects = effects
                    .iter()
                    .map(|effect| format!("{:?} {}", effect.kind, effect.target).to_lowercase())
                    .collect::<Vec<_>>();
                if suspends {
                    effects.push("suspend".into());
                }
                let effects = if effects.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", effects.join(", "))
                };
                format!(
                    "func({}) -> {}{effects}",
                    parameters
                        .iter()
                        .zip(&parameter_modes)
                        .map(|(parameter, mode)| match mode {
                            crate::ast::ParameterMode::Borrow => self.describe(parameter),
                            crate::ast::ParameterMode::Consume => {
                                format!("consume {}", self.describe(parameter))
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(", "),
                    self.describe(&result),
                )
            }
            Ty::Reference(group, value) => format!("ref[{group}] {}", self.describe(&value)),
            Ty::Record(record, arguments) => {
                let name = &self.hir.records[record].name;
                if arguments.is_empty() {
                    name.clone()
                } else {
                    format!(
                        "{name}<{}>",
                        arguments
                            .iter()
                            .map(|argument| self.describe(argument))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            }
            Ty::Intersection(members) => members
                .iter()
                .map(|member| self.describe(member))
                .collect::<Vec<_>>()
                .join(" & "),
            Ty::Variant(variant, arguments) => {
                let name = &self.hir.variant_types[variant].name;
                if arguments.is_empty() {
                    name.clone()
                } else {
                    format!(
                        "{name}<{}>",
                        arguments
                            .iter()
                            .map(|a| self.describe(a))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            }
            Ty::Module(name) => format!("module {name}"),
        }
    }

    pub(super) fn error(&self, function: FunctionId, message: impl Into<String>) -> FosterError {
        let function = &self.hir.functions[function];
        FosterError::runtime(format!(
            "in `{}.{}`: {}",
            self.hir.modules[function.module].name,
            function.name,
            message.into()
        ))
        .with_source_module(self.hir.modules[function.module].name.clone())
    }

    pub(super) fn error_at_expression(
        &self,
        mut error: FosterError,
        function: FunctionId,
        expression: ExprId,
        label: impl Into<String>,
    ) -> FosterError {
        if error.labels.is_empty()
            && let Some(span) = self.hir.expression_spans.get(&expression)
        {
            error = error.with_primary_label(span.clone(), label);
        }
        if error.source_module.is_none() {
            let module = self.hir.functions[function].module;
            error = error.with_source_module(self.hir.modules[module].name.clone());
        }
        error
    }
}

fn receiver_is_record(
    information: &TypeInformation,
    function: FunctionId,
    record: RecordId,
) -> bool {
    information
        .function_type(function)
        .and_then(|signature| signature.parameters.first())
        .is_some_and(|ty| matches!(information.types[*ty], Type::Record { record: receiver, .. } if receiver == record))
}

fn receiver_is_variant(
    information: &TypeInformation,
    function: FunctionId,
    variant: VariantTypeId,
) -> bool {
    information
        .function_type(function)
        .and_then(|signature| signature.parameters.first())
        .is_some_and(|ty| matches!(information.types[*ty], Type::Variant { variant: receiver, .. } if receiver == variant))
}

fn best_dispatch_method(
    hir: &hir::PackageHir,
    information: &TypeInformation,
    dispatch: &MethodKey,
    receiver_matches: impl Fn(FunctionId) -> bool,
) -> Option<FunctionId> {
    hir.functions
        .iter()
        .filter(|(function, definition)| {
            definition.receiver.is_some() && receiver_matches(*function)
        })
        .filter_map(|(function, definition)| {
            let name = definition
                .name
                .rsplit_once('.')
                .map_or(definition.name.as_str(), |(_, member)| member);
            let key = information.method_dispatch_key(function, name)?;
            method_key_matches(&key, dispatch).then_some((dispatch_generics(&key), function))
        })
        .min_by_key(|(generics, function)| (*generics, function.into_raw().into_u32()))
        .map(|(_, function)| function)
}

fn method_key_matches(pattern: &MethodKey, concrete: &MethodKey) -> bool {
    let mut generics = HashMap::new();
    pattern.name == concrete.name
        && pattern.parameters.len() == concrete.parameters.len()
        && pattern.parameters.iter().zip(&concrete.parameters).all(
            |((left_mode, left), (right_mode, right))| {
                left_mode == right_mode && dispatch_type_matches(left, right, &mut generics)
            },
        )
}

fn dispatch_type_matches(
    pattern: &DispatchTypeKey,
    concrete: &DispatchTypeKey,
    generics: &mut HashMap<u32, DispatchTypeKey>,
) -> bool {
    if let DispatchTypeKey::Generic(index) = pattern {
        return generics.entry(*index).or_insert_with(|| concrete.clone()) == concrete;
    }
    match (pattern, concrete) {
        (DispatchTypeKey::Reference(left), DispatchTypeKey::Reference(right))
        | (DispatchTypeKey::RawList(left), DispatchTypeKey::RawList(right))
        | (DispatchTypeKey::Sequence(left), DispatchTypeKey::Sequence(right))
        | (DispatchTypeKey::Remote(left), DispatchTypeKey::Remote(right))
        | (DispatchTypeKey::Future(left), DispatchTypeKey::Future(right)) => {
            dispatch_type_matches(left, right, generics)
        }
        (DispatchTypeKey::Record(left, left_args), DispatchTypeKey::Record(right, right_args)) => {
            left == right && dispatch_types_match(left_args, right_args, generics)
        }
        (
            DispatchTypeKey::Variant(left, left_args),
            DispatchTypeKey::Variant(right, right_args),
        ) => left == right && dispatch_types_match(left_args, right_args, generics),
        (DispatchTypeKey::Intersection(left), DispatchTypeKey::Intersection(right)) => {
            dispatch_types_match(left, right, generics)
        }
        (
            DispatchTypeKey::Function(left, left_result),
            DispatchTypeKey::Function(right, right_result),
        ) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|((lm, lt), (rm, rt))| lm == rm && dispatch_type_matches(lt, rt, generics))
                && dispatch_type_matches(left_result, right_result, generics)
        }
        _ => pattern == concrete,
    }
}

fn dispatch_types_match(
    pattern: &[DispatchTypeKey],
    concrete: &[DispatchTypeKey],
    generics: &mut HashMap<u32, DispatchTypeKey>,
) -> bool {
    pattern.len() == concrete.len()
        && pattern
            .iter()
            .zip(concrete)
            .all(|(left, right)| dispatch_type_matches(left, right, generics))
}

fn dispatch_generics(key: &MethodKey) -> usize {
    fn count(ty: &DispatchTypeKey) -> usize {
        match ty {
            DispatchTypeKey::Generic(_) => 1,
            DispatchTypeKey::Reference(value)
            | DispatchTypeKey::RawList(value)
            | DispatchTypeKey::Sequence(value)
            | DispatchTypeKey::Remote(value)
            | DispatchTypeKey::Future(value) => count(value),
            DispatchTypeKey::Record(_, values)
            | DispatchTypeKey::Intersection(values)
            | DispatchTypeKey::Variant(_, values) => values.iter().map(count).sum(),
            DispatchTypeKey::Function(parameters, result) => {
                parameters.iter().map(|(_, ty)| count(ty)).sum::<usize>() + count(result)
            }
            _ => 0,
        }
    }
    key.parameters.iter().map(|(_, ty)| count(ty)).sum()
}

fn intern_type(
    information: &mut TypeInformation,
    interner: &mut HashMap<Type, TypeId>,
    ty: Ty,
) -> TypeId {
    let ty = match ty {
        Ty::Unit => Type::Unit,
        Ty::Generic(name) => Type::Generic(name),
        Ty::Bool => Type::Bool,
        Ty::Int => Type::Int,
        Ty::Float => Type::Float,
        Ty::CodePoint => Type::CodePoint,
        Ty::Byte => Type::Byte,
        Ty::RawBytes => Type::RawBytes,
        Ty::RawByteBuffer => Type::RawByteBuffer,
        Ty::RawList(element) => {
            let element = intern_type(information, interner, *element);
            Type::RawList(element)
        }
        Ty::Sequence(element) => {
            let element = intern_type(information, interner, *element);
            Type::Sequence(element)
        }
        Ty::Remote(value) => {
            let value = intern_type(information, interner, *value);
            Type::Remote(value)
        }
        Ty::Future(value) => {
            let value = intern_type(information, interner, *value);
            Type::Future(value)
        }
        Ty::Function(parameters, result) => {
            let parameters = parameters
                .into_iter()
                .map(|parameter| intern_type(information, interner, parameter))
                .collect::<Vec<_>>();
            let parameter_modes = vec![crate::ast::ParameterMode::Borrow; parameters.len()];
            let result = intern_type(information, interner, *result);
            Type::Function(FunctionType {
                parameters,
                parameter_modes,
                result,
                erased: false,
                effects: Vec::new(),
                suspends: false,
            })
        }
        Ty::Callable {
            parameters,
            parameter_modes,
            result,
            erased,
            effects,
            suspends,
        } => {
            let parameters = parameters
                .into_iter()
                .map(|parameter| intern_type(information, interner, parameter))
                .collect();
            let result = intern_type(information, interner, *result);
            Type::Function(FunctionType {
                parameters,
                parameter_modes,
                result,
                erased,
                effects,
                suspends,
            })
        }
        Ty::Reference(group, value) => {
            let value = intern_type(information, interner, *value);
            Type::Reference { group, value }
        }
        Ty::Record(record, arguments) => {
            let arguments = arguments
                .into_iter()
                .map(|argument| intern_type(information, interner, argument))
                .collect();
            Type::Record { record, arguments }
        }
        Ty::Intersection(members) => {
            let members = members
                .into_iter()
                .map(|member| intern_type(information, interner, member))
                .collect();
            Type::Intersection(members)
        }
        Ty::Variant(variant, arguments) => {
            let arguments = arguments
                .into_iter()
                .map(|a| intern_type(information, interner, a))
                .collect();
            Type::Variant { variant, arguments }
        }
        Ty::Module(name) => Type::Module(name),
        Ty::Variable(_) => unreachable!("unresolved types are rejected before interning"),
    };
    if let Some(existing) = interner.get(&ty) {
        *existing
    } else {
        let id = information.types.alloc(ty.clone());
        interner.insert(ty, id);
        id
    }
}
