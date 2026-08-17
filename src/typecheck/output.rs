use std::collections::HashMap;

use super::*;

impl Checker<'_> {
    pub(super) fn finish(self) -> Result<TypeInformation, FosterError> {
        let mut information = TypeInformation {
            record_names: self
                .hir
                .records
                .iter()
                .map(|(record, definition)| (record, definition.name.clone()))
                .collect(),
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
        Ok(information)
    }

    pub(super) fn require_concrete(&self, ty: Ty, context: &str) -> Result<Ty, FosterError> {
        let ty = self.resolved(ty);
        if contains_variable(&ty) {
            Err(FosterError::runtime(format!(
                "cannot infer the type of {context}; add a type annotation"
            )))
        } else {
            Ok(ty)
        }
    }

    pub(super) fn describe(&self, ty: &Ty) -> String {
        match self.resolved(ty.clone()) {
            Ty::Variable(_) => "unknown".into(),
            Ty::Generic(name) => name,
            Ty::Unit => "Unit".into(),
            Ty::Bool => "Bool".into(),
            Ty::Int => "Int".into(),
            Ty::Float => "Float".into(),
            Ty::String => "String".into(),
            Ty::CodePoint => "CodePoint".into(),
            Ty::Symbol => "Symbol".into(),
            Ty::List(element) => format!("List<{}>", self.describe(&element)),
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
                erased,
                effects,
                suspends,
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
                    "{}func({}) -> {}{effects}",
                    if erased { "any " } else { "" },
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
    }
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
        Ty::String => Type::String,
        Ty::CodePoint => Type::CodePoint,
        Ty::Symbol => Type::Symbol,
        Ty::List(element) => {
            let element = intern_type(information, interner, *element);
            Type::List(element)
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
