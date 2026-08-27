use std::collections::{HashMap, HashSet};

use super::*;

#[derive(Debug, Clone)]
pub(super) struct EffectiveField {
    pub(super) name: String,
    pub(super) public: bool,
    pub(super) ty: Ty,
}

#[derive(Debug, Clone)]
pub(super) struct EffectiveMethod {
    pub(super) name: String,
    pub(super) public: bool,
    pub(super) parameters: Vec<Ty>,
    pub(super) parameter_modes: Vec<crate::ast::ParameterMode>,
    pub(super) result: Ty,
    pub(super) effects: Vec<crate::ast::Effect>,
    pub(super) suspends: bool,
    pub(super) required_by_composition: bool,
}

impl Checker<'_> {
    pub(super) fn check_variant_compositions(&mut self) -> Result<(), FosterError> {
        let variants = self
            .hir
            .variant_types
            .iter()
            .map(|(id, definition)| (id, definition.clone()))
            .collect::<Vec<_>>();
        for (variant, definition) in variants {
            let arguments = definition
                .parameters
                .iter()
                .map(|parameter| Ty::Generic(parameter.clone()))
                .collect::<Vec<_>>();
            let generics = definition
                .parameters
                .iter()
                .cloned()
                .zip(arguments.iter().cloned())
                .collect::<HashMap<_, _>>();
            let mut methods = Vec::new();
            for composition in &definition.compositions {
                let contract = self.annotation_type(definition.module, composition, &generics)?;
                if definition.public
                    && let Some(private) = self.private_type_in(&contract)
                {
                    return Err(FosterError::runtime(format!(
                        "public type `{}` composes private type `{private}`",
                        definition.name
                    )));
                }
                self.collect_variant_contract_methods(&definition, contract, &mut methods)?;
            }
            for requirement in &definition.methods {
                let method = self.method_requirement(
                    &definition.name,
                    definition.module,
                    requirement,
                    &generics,
                    false,
                )?;
                Self::merge_variant_method(&definition.name, &mut methods, method)?;
            }
            for method in &methods {
                self.check_variant_method_implementation(variant, &arguments, method)?;
            }
        }
        Ok(())
    }

    fn collect_variant_contract_methods(
        &mut self,
        owner: &hir::VariantType,
        contract: Ty,
        methods: &mut Vec<EffectiveMethod>,
    ) -> Result<(), FosterError> {
        match self.resolved(contract) {
            Ty::Record(record, arguments) => {
                if !self.effective_record_fields(record, &arguments)?.is_empty() {
                    return Err(FosterError::runtime(format!(
                        "{} `{}` cannot compose contract `{}` because it requires stored fields",
                        if owner.kind == crate::ast::VariantKind::Enum {
                            "enum"
                        } else {
                            "union contract"
                        },
                        owner.name,
                        self.hir.records[record].name
                    )));
                }
                for mut method in self.effective_record_methods(record, &arguments)? {
                    if method.public {
                        method.required_by_composition = true;
                        Self::merge_variant_method(&owner.name, methods, method)?;
                    }
                }
                Ok(())
            }
            Ty::Intersection(members) => {
                for member in members {
                    self.collect_variant_contract_methods(owner, member, methods)?;
                }
                Ok(())
            }
            other => Err(FosterError::runtime(format!(
                "{} `{}` cannot compose non-contract type `{}`",
                if owner.kind == crate::ast::VariantKind::Enum {
                    "enum"
                } else {
                    "union contract"
                },
                owner.name,
                self.describe(&other)
            ))),
        }
    }

    fn merge_variant_method(
        owner: &str,
        methods: &mut Vec<EffectiveMethod>,
        incoming: EffectiveMethod,
    ) -> Result<(), FosterError> {
        let Some(existing) = methods
            .iter_mut()
            .find(|method| method.name == incoming.name)
        else {
            methods.push(incoming);
            return Ok(());
        };
        if existing.parameters != incoming.parameters
            || existing.parameter_modes != incoming.parameter_modes
            || existing.result != incoming.result
            || existing.effects != incoming.effects
            || existing.suspends != incoming.suspends
        {
            return Err(FosterError::runtime(format!(
                "type `{owner}` composes incompatible definitions of method `{}`",
                incoming.name
            )));
        }
        existing.public |= incoming.public;
        Ok(())
    }

    fn check_variant_method_implementation(
        &mut self,
        owner: VariantTypeId,
        arguments: &[Ty],
        required: &EffectiveMethod,
    ) -> Result<(), FosterError> {
        let definition = self.hir.variant_types[owner].clone();
        let qualified_name = format!("{}.{name}", definition.name, name = required.name);
        let Some(function) = self.hir.function_named(definition.module, &qualified_name) else {
            return Err(FosterError::runtime(format!(
                "type `{}` is missing required method `{}`",
                definition.name, required.name
            )));
        };
        let implementation = &self.hir.functions[function];
        if definition.public && required.public && !implementation.public {
            return Err(self.error(
                function,
                format!(
                    "public type `{}` requires method `{}` to be public",
                    definition.name, required.name
                ),
            ));
        }
        if implementation.receiver.is_none() {
            return Err(self.error(
                function,
                format!(
                    "required method `{}` must be an instance method with `self` first",
                    required.name
                ),
            ));
        }
        let signature = self.functions[&function].clone();
        if signature.parameters.len() != required.parameters.len() + 1 {
            return Err(self.error(function, format!(
                "method `{}` does not match its composed contract: expected {} argument(s) after `self`",
                required.name, required.parameters.len()
            )));
        }
        self.unify(
            Ty::Variant(owner, arguments.to_vec()),
            signature.parameters[0].clone(),
            function,
        )?;
        for (expected, actual) in required
            .parameters
            .iter()
            .cloned()
            .zip(signature.parameters.iter().skip(1).cloned())
        {
            self.unify(expected, actual, function)?;
        }
        if signature.parameter_modes[1..] != required.parameter_modes {
            return Err(self.error(
                function,
                format!(
                    "method `{}` has incompatible consuming parameters",
                    required.name
                ),
            ));
        }
        self.coerce(required.result.clone(), signature.result, function)?;
        let mut allowed_effects = required.effects.clone();
        allowed_effects.push(crate::ast::Effect {
            kind: crate::ast::EffectKind::Read,
            target: crate::ast::GroupPath::root("self"),
        });
        for (parameter, mode) in implementation
            .parameters
            .iter()
            .skip(1)
            .zip(&required.parameter_modes)
        {
            if *mode == crate::ast::ParameterMode::Borrow {
                allowed_effects.push(crate::ast::Effect {
                    kind: crate::ast::EffectKind::Read,
                    target: crate::ast::GroupPath::root(self.hir.locals[*parameter].name.clone()),
                });
            }
        }
        if !effects_are_subset(&implementation.effects, &allowed_effects) {
            return Err(self.error(function, format!(
                "method `{}` requires effects outside its composed contract: inferred [{}], allowed [{}]",
                required.name,
                describe_effects(&implementation.effects),
                describe_effects(&allowed_effects)
            )));
        }
        if implementation.suspends && !required.suspends {
            return Err(self.error(
                function,
                format!(
                    "method `{}` suspends but its composed contract does not",
                    required.name
                ),
            ));
        }
        Ok(())
    }

    pub(super) fn check_record_compositions(&mut self) -> Result<(), FosterError> {
        let records = self
            .hir
            .records
            .iter()
            .map(|(id, record)| (id, record.clone()))
            .collect::<Vec<_>>();
        for (record, definition) in records {
            let arguments = definition
                .parameters
                .iter()
                .map(|parameter| Ty::Generic(parameter.clone()))
                .collect::<Vec<_>>();
            self.effective_record_fields(record, &arguments)?;
            let methods = self.effective_record_methods(record, &arguments)?;

            let generics = definition
                .parameters
                .iter()
                .cloned()
                .zip(arguments.iter().cloned())
                .collect::<HashMap<_, _>>();
            for composition in &definition.compositions {
                let contract = self.annotation_type(definition.module, composition, &generics)?;
                if definition.public
                    && let Some(private) = self.private_type_in(&contract)
                {
                    return Err(FosterError::runtime(format!(
                        "public type `{}` composes private type `{private}`",
                        definition.name
                    )));
                }
            }
            // A declaration with bodyless methods is itself a contract. It inherits
            // composed requirements without implementing them; concrete composing
            // records have no such declarations and must provide every method.
            if definition.methods.is_empty() {
                for method in methods
                    .iter()
                    .filter(|method| method.required_by_composition)
                {
                    self.check_method_implementation(record, &arguments, method)?;
                }
            }
        }
        Ok(())
    }

    pub(super) fn effective_record_fields(
        &mut self,
        record: RecordId,
        arguments: &[Ty],
    ) -> Result<Vec<EffectiveField>, FosterError> {
        self.collect_record_fields(record, arguments, &mut HashSet::new())
    }

    pub(super) fn effective_record_methods(
        &mut self,
        record: RecordId,
        arguments: &[Ty],
    ) -> Result<Vec<EffectiveMethod>, FosterError> {
        self.collect_record_methods(record, arguments, &mut HashSet::new(), false)
    }

    fn collect_record_fields(
        &mut self,
        record: RecordId,
        arguments: &[Ty],
        visiting: &mut HashSet<RecordId>,
    ) -> Result<Vec<EffectiveField>, FosterError> {
        self.enter_composition(record, visiting)?;
        let definition = self.hir.records[record].clone();
        let generics = record_generics(&definition, arguments);
        let mut fields = Vec::new();

        for composition in &definition.compositions {
            let contract = self.annotation_type(definition.module, composition, &generics)?;
            self.collect_contract_fields(record, contract, visiting, &mut fields)?;
        }
        for field in &definition.fields {
            let ty = self.annotation_type(definition.module, &field.ty, &generics)?;
            self.merge_effective_field(
                record,
                &mut fields,
                EffectiveField {
                    name: field.name.clone(),
                    public: field.public,
                    ty,
                },
            )?;
        }
        visiting.remove(&record);
        Ok(fields)
    }

    fn collect_contract_fields(
        &mut self,
        owner: RecordId,
        contract: Ty,
        visiting: &mut HashSet<RecordId>,
        fields: &mut Vec<EffectiveField>,
    ) -> Result<(), FosterError> {
        match self.resolved(contract) {
            Ty::Record(record, arguments) => {
                if record == owner {
                    return self.self_composition_error(owner);
                }
                for field in self.collect_record_fields(record, &arguments, visiting)? {
                    if field.public {
                        self.merge_effective_field(owner, fields, field)?;
                    }
                }
                Ok(())
            }
            // Sequence is behavioral: its members are accessors, not stored fields.
            Ty::Sequence(_) => Ok(()),
            Ty::Intersection(members) => {
                for member in members {
                    self.collect_contract_fields(owner, member, visiting, fields)?;
                }
                Ok(())
            }
            other => self.non_contract_error(owner, &other),
        }
    }

    fn collect_record_methods(
        &mut self,
        record: RecordId,
        arguments: &[Ty],
        visiting: &mut HashSet<RecordId>,
        inherited: bool,
    ) -> Result<Vec<EffectiveMethod>, FosterError> {
        self.enter_composition(record, visiting)?;
        let definition = self.hir.records[record].clone();
        let generics = record_generics(&definition, arguments);
        let mut methods = Vec::new();

        for composition in &definition.compositions {
            let contract = self.annotation_type(definition.module, composition, &generics)?;
            self.collect_contract_methods(record, contract, visiting, &mut methods)?;
        }
        for requirement in &definition.methods {
            let method = self.method_requirement(
                &definition.name,
                definition.module,
                requirement,
                &generics,
                inherited,
            )?;
            self.merge_effective_method(record, &mut methods, method)?;
        }
        visiting.remove(&record);
        Ok(methods)
    }

    fn collect_contract_methods(
        &mut self,
        owner: RecordId,
        contract: Ty,
        visiting: &mut HashSet<RecordId>,
        methods: &mut Vec<EffectiveMethod>,
    ) -> Result<(), FosterError> {
        match self.resolved(contract) {
            Ty::Record(record, arguments) => {
                if record == owner {
                    return self.self_composition_error(owner);
                }
                for mut method in self.collect_record_methods(record, &arguments, visiting, true)? {
                    if method.public {
                        method.required_by_composition = true;
                        self.merge_effective_method(owner, methods, method)?;
                    }
                }
                Ok(())
            }
            Ty::Sequence(element) => {
                let element = self.resolved(*element);
                for (name, result) in [
                    ("empty?", Ty::Bool),
                    ("length", Ty::Int),
                    ("head", element.clone()),
                    ("rest", Ty::Sequence(Box::new(element))),
                ] {
                    self.merge_effective_method(
                        owner,
                        methods,
                        EffectiveMethod {
                            name: name.into(),
                            public: true,
                            parameters: Vec::new(),
                            parameter_modes: Vec::new(),
                            result,
                            effects: Vec::new(),
                            suspends: false,
                            required_by_composition: true,
                        },
                    )?;
                }
                Ok(())
            }
            Ty::Intersection(members) => {
                for member in members {
                    self.collect_contract_methods(owner, member, visiting, methods)?;
                }
                Ok(())
            }
            other => self.non_contract_error(owner, &other),
        }
    }

    fn method_requirement(
        &mut self,
        owner_name: &str,
        owner_module: hir::ModuleId,
        requirement: &crate::ast::MethodRequirement,
        record_generics: &HashMap<String, Ty>,
        inherited: bool,
    ) -> Result<EffectiveMethod, FosterError> {
        if !requirement.type_parameters.is_empty() || !requirement.groups.is_empty() {
            return Err(FosterError::runtime(format!(
                "required method `{}.{}` cannot yet declare method-level type or group parameters",
                owner_name, requirement.name
            )));
        }
        let Some(receiver) = requirement.parameters.first() else {
            return Err(FosterError::runtime(format!(
                "required method `{}.{}` must declare `self` as its first parameter",
                owner_name, requirement.name
            )));
        };
        if !requirement.receiver || receiver.ty.is_some() {
            return Err(FosterError::runtime(format!(
                "required method `{}.{}` must begin with an untyped `self` parameter",
                owner_name, requirement.name
            )));
        }
        let parameters = requirement
            .parameters
            .iter()
            .skip(1)
            .map(|parameter| {
                let annotation = parameter.ty.as_ref().ok_or_else(|| {
                    FosterError::runtime(format!(
                        "required method `{}.{}` parameter `{}` needs a type",
                        owner_name, requirement.name, parameter.name
                    ))
                })?;
                self.annotation_type(owner_module, annotation, record_generics)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let parameter_modes = requirement
            .parameters
            .iter()
            .skip(1)
            .map(|parameter| {
                if requirement.effects.iter().any(|effect| {
                    effect.kind == crate::ast::EffectKind::Consume
                        && effect.target.root == parameter.name
                }) {
                    crate::ast::ParameterMode::Consume
                } else {
                    crate::ast::ParameterMode::Borrow
                }
            })
            .collect();
        let result = requirement
            .return_type
            .as_ref()
            .map(|ty| self.annotation_type(owner_module, ty, record_generics))
            .transpose()?
            .unwrap_or(Ty::Unit);
        Ok(EffectiveMethod {
            name: requirement.name.clone(),
            public: requirement.public,
            parameters,
            parameter_modes,
            result,
            effects: requirement.effects.clone(),
            suspends: requirement.suspends,
            required_by_composition: inherited,
        })
    }

    fn check_method_implementation(
        &mut self,
        owner: RecordId,
        arguments: &[Ty],
        required: &EffectiveMethod,
    ) -> Result<(), FosterError> {
        let definition = self.hir.records[owner].clone();
        let qualified_name = format!("{}.{name}", definition.name, name = required.name);
        let Some(function) = self.hir.function_named(definition.module, &qualified_name) else {
            return Err(FosterError::runtime(format!(
                "type `{}` is missing required method `{}`",
                definition.name, required.name
            )));
        };
        let implementation = &self.hir.functions[function];
        if definition.public && required.public && !implementation.public {
            return Err(self.error(
                function,
                format!(
                    "public type `{}` requires method `{}` to be public",
                    definition.name, required.name
                ),
            ));
        }
        if implementation.receiver.is_none() {
            return Err(self.error(
                function,
                format!(
                    "required method `{}` must be an instance method with `self` first",
                    required.name
                ),
            ));
        }
        let implementation_effects = implementation.effects.clone();
        let implementation_suspends = implementation.suspends;
        let signature = self.functions[&function].clone();
        if signature.parameters.len() != required.parameters.len() + 1 {
            return Err(self.error(
                function,
                format!(
                    "method `{}` does not match its composed contract: expected {} argument(s) after `self`",
                    required.name,
                    required.parameters.len()
                ),
            ));
        }
        self.unify(
            Ty::Record(owner, arguments.to_vec()),
            signature.parameters[0].clone(),
            function,
        )?;
        for (expected, actual) in required
            .parameters
            .iter()
            .cloned()
            .zip(signature.parameters.iter().skip(1).cloned())
        {
            self.unify(expected, actual, function)?;
        }
        if signature.parameter_modes[1..] != required.parameter_modes {
            return Err(self.error(
                function,
                format!(
                    "method `{}` has incompatible consuming parameters",
                    required.name
                ),
            ));
        }
        self.coerce(required.result.clone(), signature.result, function)?;
        let mut allowed_effects = required.effects.clone();
        allowed_effects.push(crate::ast::Effect {
            kind: crate::ast::EffectKind::Read,
            target: crate::ast::GroupPath::root("self"),
        });
        for (parameter, mode) in implementation
            .parameters
            .iter()
            .skip(1)
            .zip(&required.parameter_modes)
        {
            if *mode == crate::ast::ParameterMode::Borrow {
                allowed_effects.push(crate::ast::Effect {
                    kind: crate::ast::EffectKind::Read,
                    target: crate::ast::GroupPath::root(self.hir.locals[*parameter].name.clone()),
                });
            }
        }
        if !effects_are_subset(&implementation_effects, &allowed_effects) {
            let actual = describe_effects(&implementation_effects);
            let allowed = describe_effects(&allowed_effects);
            return Err(self.error(
                function,
                format!(
                    "method `{}` requires effects outside its composed contract: inferred [{actual}], allowed [{allowed}]",
                    required.name,
                ),
            ));
        }
        if implementation_suspends && !required.suspends {
            return Err(self.error(
                function,
                format!(
                    "method `{}` suspends but its composed contract does not",
                    required.name
                ),
            ));
        }
        Ok(())
    }

    fn merge_effective_field(
        &mut self,
        owner: RecordId,
        fields: &mut Vec<EffectiveField>,
        incoming: EffectiveField,
    ) -> Result<(), FosterError> {
        let Some(existing) = fields.iter_mut().find(|field| field.name == incoming.name) else {
            fields.push(incoming);
            return Ok(());
        };
        if self.resolved(existing.ty.clone()) != self.resolved(incoming.ty.clone()) {
            return Err(FosterError::runtime(format!(
                "type `{}` composes incompatible definitions of field `{}`: `{}` and `{}`",
                self.hir.records[owner].name,
                incoming.name,
                self.describe(&existing.ty),
                self.describe(&incoming.ty)
            )));
        }
        existing.public |= incoming.public;
        Ok(())
    }

    fn merge_effective_method(
        &mut self,
        owner: RecordId,
        methods: &mut Vec<EffectiveMethod>,
        incoming: EffectiveMethod,
    ) -> Result<(), FosterError> {
        let Some(existing) = methods
            .iter_mut()
            .find(|method| method.name == incoming.name)
        else {
            methods.push(incoming);
            return Ok(());
        };
        let compatible = existing.parameters == incoming.parameters
            && existing.parameter_modes == incoming.parameter_modes
            && existing.result == incoming.result
            && existing.effects == incoming.effects
            && existing.suspends == incoming.suspends;
        if !compatible {
            return Err(FosterError::runtime(format!(
                "type `{}` composes incompatible definitions of method `{}`",
                self.hir.records[owner].name, incoming.name
            )));
        }
        existing.public |= incoming.public;
        existing.required_by_composition |= incoming.required_by_composition;
        Ok(())
    }

    fn enter_composition(
        &self,
        record: RecordId,
        visiting: &mut HashSet<RecordId>,
    ) -> Result<(), FosterError> {
        if visiting.insert(record) {
            Ok(())
        } else {
            Err(FosterError::runtime(format!(
                "type `{}` has a cyclic composed contract",
                self.hir.records[record].name
            )))
        }
    }

    fn self_composition_error<T>(&self, owner: RecordId) -> Result<T, FosterError> {
        Err(FosterError::runtime(format!(
            "type `{}` cannot compose itself",
            self.hir.records[owner].name
        )))
    }

    fn non_contract_error<T>(&self, owner: RecordId, ty: &Ty) -> Result<T, FosterError> {
        Err(FosterError::runtime(format!(
            "type `{}` cannot compose non-contract type `{}`",
            self.hir.records[owner].name,
            self.describe(ty)
        )))
    }
}

fn record_generics(record: &hir::Record, arguments: &[Ty]) -> HashMap<String, Ty> {
    record
        .parameters
        .iter()
        .cloned()
        .zip(arguments.iter().cloned())
        .collect()
}

fn describe_effects(effects: &[crate::ast::Effect]) -> String {
    effects
        .iter()
        .map(|effect| format!("{} {}", effect_kind_name(effect.kind), effect.target))
        .collect::<Vec<_>>()
        .join(", ")
}
