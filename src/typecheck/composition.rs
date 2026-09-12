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
    pub(super) type_parameters: Vec<String>,
    pub(super) name: String,
    pub(super) public: bool,
    pub(super) parameters: Vec<Ty>,
    pub(super) parameter_modes: Vec<crate::ast::ParameterMode>,
    pub(super) result: Ty,
    pub(super) returns_self: bool,
    /// Unbound nested receiver result, retained when adapting a contract to another receiver.
    pub(super) receiver_result: Option<Ty>,
    pub(super) effects: Vec<crate::ast::Effect>,
    pub(super) suspends: bool,
    pub(super) requirement: Option<(RecordId, usize)>,
}

impl Checker<'_> {
    pub(super) fn method_result_for_receiver(
        &mut self,
        method: &EffectiveMethod,
        receiver: Ty,
    ) -> Ty {
        if method.returns_self {
            return receiver;
        }
        if let Some(template) = &method.receiver_result {
            let mut generics = HashMap::new();
            preserve_generics(template, &mut generics);
            generics.insert("$receiver".into(), receiver);
            return self.instantiate(template.clone(), &mut generics);
        }
        method.result.clone()
    }

    pub(super) fn can_access_module(&self, caller: FunctionId, module: hir::ModuleId) -> bool {
        self.hir.functions[caller].module == module
            || self.hir.composition_owners.get(&caller) == Some(&module)
    }
    /// Ordering selects behavior, never permission to weaken an earlier contract.
    pub(super) fn check_composed_implementations(&mut self) -> Result<(), FosterError> {
        for &(earlier, later) in &self.hir.composition_checks {
            let left = self.functions[&earlier].clone();
            let right = self.functions[&later].clone();
            let canonical = |checker: &mut Self, function: FunctionId, ty: Ty| {
                let mut generics = checker.hir.functions[function]
                    .type_parameters
                    .iter()
                    .enumerate()
                    .map(|(index, name)| (name.clone(), Ty::Generic(format!("$parameter{index}"))))
                    .collect();
                let ty = checker.resolved(ty);
                checker.instantiate(ty, &mut generics)
            };
            let left_parameters = left
                .parameters
                .iter()
                .cloned()
                .map(|ty| canonical(self, earlier, ty))
                .collect::<Vec<_>>();
            let right_parameters = right
                .parameters
                .iter()
                .cloned()
                .map(|ty| canonical(self, later, ty))
                .collect::<Vec<_>>();
            let compatible = left_parameters == right_parameters
                && left.parameter_modes == right.parameter_modes
                && canonical(self, earlier, left.result) == canonical(self, later, right.result);
            // A declared requirement is the public bound; a default body may use
            // fewer effects without narrowing that requirement for later defaults.
            let contract_checked =
                if let Ty::Record(owner, arguments) = self.resolved(right.parameters[0].clone()) {
                    let name = self.hir.functions[later]
                        .name
                        .rsplit('.')
                        .next()
                        .unwrap()
                        .split('$')
                        .next()
                        .unwrap()
                        .to_owned();
                    let parameters = right
                        .parameters
                        .iter()
                        .skip(1)
                        .cloned()
                        .map(|ty| self.resolved(ty))
                        .collect::<Vec<_>>();
                    let required = self
                        .effective_record_methods(owner, &arguments)?
                        .into_iter()
                        .find(|method| method.name == name && method.parameters == parameters);
                    if let Some(required) = required {
                        self.check_method_implementation(later, owner, &arguments, &required)?;
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };
            let normalize_effects = |function: FunctionId| {
                let definition = &self.hir.functions[function];
                let mut effects = definition.effects.clone();
                for effect in &mut effects {
                    if let Some(index) = definition
                        .parameters
                        .iter()
                        .position(|local| self.hir.locals[*local].name == effect.target.root)
                    {
                        effect.target.root = format!("argument{index}");
                    }
                }
                effects
            };
            if !compatible
                || (self.hir.functions[earlier].public && !self.hir.functions[later].public)
                || (!contract_checked
                    && (!effects_are_subset(
                        &normalize_effects(later),
                        &normalize_effects(earlier),
                    ) || (self.hir.functions[later].suspends
                        && !self.hir.functions[earlier].suspends)))
            {
                return Err(self.error(
                    later,
                    format!(
                        "composes incompatible implementations of method `{}`",
                        self.hir.functions[later].name
                    ),
                ));
            }
        }
        Ok(())
    }
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
                    None,
                )?;
                Self::merge_variant_method(&definition.name, &mut methods, method)?;
            }
            for method in &mut methods {
                method.result = self
                    .method_result_for_receiver(method, Ty::Variant(variant, arguments.clone()));
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
                            "type alias"
                        },
                        owner.name,
                        self.hir.records[record].name
                    )));
                }
                for method in self.effective_record_methods(record, &arguments)? {
                    if method.public {
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
                    "type alias"
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
        let Some(existing) = methods.iter_mut().find(|method| {
            method.name == incoming.name && method.parameters == incoming.parameters
        }) else {
            methods.push(incoming);
            return Ok(());
        };
        if existing.parameter_modes != incoming.parameter_modes
            || existing.result != incoming.result
            || existing.returns_self != incoming.returns_self
            || existing.receiver_result != incoming.receiver_result
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
        let Some(function) = self.matching_method_implementation(
            definition.module,
            &qualified_name,
            Ty::Variant(owner, arguments.to_vec()),
            &required.parameters,
            &required.parameter_modes,
        )?
        else {
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
        let raw_signature = self.functions[&function].clone();
        let mut generics = HashMap::new();
        let signature = Signature {
            parameters: raw_signature
                .parameters
                .into_iter()
                .map(|ty| self.instantiate(ty, &mut generics))
                .collect(),
            parameter_modes: raw_signature.parameter_modes,
            result: self.instantiate(raw_signature.result, &mut generics),
        };
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
            self.effective_record_methods(record, &arguments)?;

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
        }
        Ok(())
    }

    pub(super) fn effective_record_fields(
        &mut self,
        record: RecordId,
        arguments: &[Ty],
    ) -> Result<Vec<EffectiveField>, FosterError> {
        let key = (record, arguments.to_vec());
        if let Some(fields) = self.record_fields_cache.get(&key) {
            return Ok(fields.clone());
        }
        let fields = self.collect_record_fields(record, arguments, &mut HashSet::new())?;
        // Inference variables can be rebound by overload backtracking. Cache only contracts
        // whose inputs and outputs are independent of the current substitution state.
        if !arguments.iter().any(contains_variable)
            && !fields.iter().any(|field| contains_variable(&field.ty))
        {
            self.record_fields_cache.insert(key, fields.clone());
        }
        Ok(fields)
    }

    pub(super) fn effective_record_methods(
        &mut self,
        record: RecordId,
        arguments: &[Ty],
    ) -> Result<Vec<EffectiveMethod>, FosterError> {
        let key = (record, arguments.to_vec());
        if let Some(methods) = self.record_methods_cache.get(&key) {
            return Ok(methods.clone());
        }
        let mut methods = self.collect_record_methods(record, arguments, &mut HashSet::new())?;
        for method in &mut methods {
            method.result =
                self.method_result_for_receiver(method, Ty::Record(record, arguments.to_vec()));
        }
        if !arguments.iter().any(contains_variable)
            && !methods.iter().any(|method| {
                method.parameters.iter().any(contains_variable) || contains_variable(&method.result)
            })
        {
            self.record_methods_cache.insert(key, methods.clone());
        }
        Ok(methods)
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
    ) -> Result<Vec<EffectiveMethod>, FosterError> {
        self.enter_composition(record, visiting)?;
        let definition = self.hir.records[record].clone();
        let generics = record_generics(&definition, arguments);
        let mut methods = Vec::new();

        for composition in &definition.compositions {
            let contract = self.annotation_type(definition.module, composition, &generics)?;
            self.collect_contract_methods(record, contract, visiting, &mut methods)?;
        }
        for (method_index, requirement) in definition.methods.iter().enumerate() {
            let method = self.method_requirement(
                &definition.name,
                definition.module,
                requirement,
                &generics,
                Some((record, method_index)),
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
                for method in self.collect_record_methods(record, &arguments, visiting)? {
                    if method.public {
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
                            type_parameters: Vec::new(),
                            name: name.into(),
                            public: true,
                            parameters: Vec::new(),
                            parameter_modes: Vec::new(),
                            result,
                            returns_self: false,
                            receiver_result: None,
                            effects: Vec::new(),
                            suspends: false,
                            requirement: None,
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
        origin: Option<(RecordId, usize)>,
    ) -> Result<EffectiveMethod, FosterError> {
        if !requirement.groups.is_empty() {
            return Err(FosterError::runtime(format!(
                "required method `{}.{}` cannot yet declare method-level group parameters",
                owner_name, requirement.name
            )));
        }
        let mut generics = record_generics.clone();
        let mut type_parameters = Vec::new();
        let mut occupied = HashMap::new();
        for ty in record_generics.values() {
            preserve_generics(ty, &mut occupied);
        }
        let mut index = 0;
        for parameter in &requirement.type_parameters {
            if generics.contains_key(parameter) {
                return Err(FosterError::runtime(format!(
                    "required method `{owner_name}.{}` shadows type parameter `{parameter}`",
                    requirement.name
                )));
            }
            let name = loop {
                let name = format!("$required{index}");
                index += 1;
                if !occupied.contains_key(&name) {
                    break name;
                }
            };
            generics.insert(parameter.clone(), Ty::Generic(name.clone()));
            type_parameters.push(name);
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
                self.annotation_type(owner_module, annotation, &generics)
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
        let returns_self = matches!(requirement.return_type.as_ref(),
            Some(crate::ast::TypeExpr::Named(name, arguments)) if name == "self" && arguments.is_empty());
        // Only result annotations bind `self`; it is not a user-declared generic.
        generics.insert("self".into(), Ty::Generic("$receiver".into()));
        let result = requirement
            .return_type
            .as_ref()
            .filter(|_| !returns_self)
            .map(|ty| self.annotation_type(owner_module, ty, &generics))
            .transpose()?
            .unwrap_or(Ty::Unit);
        let mut result_generics = HashMap::new();
        preserve_generics(&result, &mut result_generics);
        let receiver_result = result_generics
            .contains_key("$receiver")
            .then(|| result.clone());
        Ok(EffectiveMethod {
            type_parameters,
            name: requirement.name.clone(),
            public: requirement.public,
            parameters,
            parameter_modes,
            result,
            returns_self,
            receiver_result,
            effects: requirement.effects.clone(),
            suspends: requirement.suspends,
            requirement: origin,
        })
    }

    pub(super) fn instantiate_required_method(
        &mut self,
        mut method: EffectiveMethod,
    ) -> EffectiveMethod {
        let mut generics = HashMap::new();
        for parameter in &method.parameters {
            preserve_generics(parameter, &mut generics);
        }
        preserve_generics(&method.result, &mut generics);
        if let Some(template) = &method.receiver_result {
            preserve_generics(template, &mut generics);
        }
        for name in &method.type_parameters {
            generics.insert(name.clone(), self.fresh());
        }
        method.parameters = method
            .parameters
            .into_iter()
            .map(|ty| self.instantiate(ty, &mut generics))
            .collect();
        method.result = self.instantiate(method.result, &mut generics);
        method.receiver_result = method
            .receiver_result
            .map(|ty| self.instantiate(ty, &mut generics));
        method
    }

    pub(super) fn check_method_implementation(
        &mut self,
        site: FunctionId,
        owner: RecordId,
        arguments: &[Ty],
        required: &EffectiveMethod,
    ) -> Result<(), FosterError> {
        // This check observes implementation contracts beyond the selected direct callee.
        self.body_cacheable = false;
        // Rigid contracts need validation only once in this checker pass. Never
        // cache unresolved arguments or signatures: later inference can change them.
        let resolved_arguments = arguments
            .iter()
            .cloned()
            .map(|ty| self.resolved(ty))
            .collect::<Vec<_>>();
        let cache_key = required
            .requirement
            .filter(|_| {
                resolved_arguments
                    .iter()
                    .chain(&required.parameters)
                    .chain(std::iter::once(&required.result))
                    .all(|ty| !contains_variable(ty))
            })
            .map(|(record, index)| (owner, resolved_arguments, record, index));
        if cache_key
            .as_ref()
            .is_some_and(|key| self.checked_requirements.contains(key))
        {
            return Ok(());
        }
        let definition = self.hir.records[owner].clone();
        if let Some(Ty::Callable {
            parameters, result, ..
        }) =
            self.builtin_collection_method(&Ty::Record(owner, arguments.to_vec()), &required.name)
            && required.parameters == parameters
            && required.result == *result
            && !required.returns_self
        {
            return Ok(());
        }
        let qualified_name = format!("{}.{name}", definition.name, name = required.name);
        let Some(function) = self.matching_method_implementation(
            definition.module,
            &qualified_name,
            Ty::Record(owner, arguments.to_vec()),
            &required.parameters,
            &required.parameter_modes,
        )?
        else {
            return Err(self.error(
                site,
                format!(
                    "record `{}` cannot be instantiated because it is missing required method `{}`",
                    definition.name, required.name
                ),
            ));
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
        let raw_signature = self.functions[&function].clone();
        let cache_key = cache_key.filter(|_| {
            raw_signature
                .parameters
                .iter()
                .chain(std::iter::once(&raw_signature.result))
                .all(|ty| !contains_variable(ty))
        });
        let mut generics = HashMap::new();
        let signature = Signature {
            parameters: raw_signature
                .parameters
                .into_iter()
                .map(|ty| self.instantiate(ty, &mut generics))
                .collect(),
            parameter_modes: raw_signature.parameter_modes,
            result: self.instantiate(raw_signature.result, &mut generics),
        };
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
        if let Some(key) = cache_key {
            self.checked_requirements.insert(key);
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
        let Some(existing) = methods.iter_mut().find(|method| {
            method.name == incoming.name && method.parameters == incoming.parameters
        }) else {
            methods.push(incoming);
            return Ok(());
        };
        let compatible = existing.parameter_modes == incoming.parameter_modes
            && existing.result == incoming.result
            && existing.returns_self == incoming.returns_self
            && existing.receiver_result == incoming.receiver_result
            && existing.effects == incoming.effects
            && existing.suspends == incoming.suspends;
        if !compatible {
            return Err(FosterError::runtime(format!(
                "type `{}` composes incompatible definitions of method `{}`",
                self.hir.records[owner].name, incoming.name
            )));
        }
        existing.public |= incoming.public;
        Ok(())
    }

    fn matching_method_implementation(
        &mut self,
        module: crate::hir::ModuleId,
        qualified_name: &str,
        receiver: Ty,
        required_parameters: &[Ty],
        required_modes: &[crate::ast::ParameterMode],
    ) -> Result<Option<FunctionId>, FosterError> {
        let initial_substitutions = self.substitutions.clone();
        let initial_next_variable = self.next_variable;
        let mut found = Vec::new();
        for function in self.hir.functions_named(module, qualified_name) {
            self.substitutions = initial_substitutions.clone();
            self.next_variable = initial_next_variable;
            let raw_signature = self.functions[function].clone();
            let mut generics = HashMap::new();
            let signature = Signature {
                parameters: raw_signature
                    .parameters
                    .into_iter()
                    .map(|ty| self.instantiate(ty, &mut generics))
                    .collect(),
                parameter_modes: raw_signature.parameter_modes,
                result: self.instantiate(raw_signature.result, &mut generics),
            };
            if self.hir.functions[*function].receiver.is_none()
                || signature.parameters.len() != required_parameters.len() + 1
                || signature.parameter_modes[1..] != *required_modes
            {
                continue;
            }
            let compatible_receiver = self
                .unify(receiver.clone(), signature.parameters[0].clone(), *function)
                .is_ok();
            let compatible_parameters = compatible_receiver
                && required_parameters
                    .iter()
                    .cloned()
                    .zip(signature.parameters.iter().skip(1).cloned())
                    .all(|(expected, actual)| self.unify(expected, actual, *function).is_ok());
            if compatible_parameters {
                found.push((*function, self.substitutions.clone(), self.next_variable));
            }
        }
        self.substitutions = initial_substitutions;
        self.next_variable = initial_next_variable;
        match found.as_slice() {
            [] => Ok(None),
            [(function, substitutions, next_variable)] => {
                self.substitutions = substitutions.clone();
                self.next_variable = *next_variable;
                Ok(Some(*function))
            }
            _ => Err(FosterError::runtime(format!(
                "method implementation `{qualified_name}` is ambiguous"
            ))),
        }
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

fn preserve_generics(ty: &Ty, generics: &mut HashMap<String, Ty>) {
    match ty {
        Ty::Generic(name) => {
            generics.entry(name.clone()).or_insert_with(|| ty.clone());
        }
        Ty::Record(_, args) | Ty::Variant(_, args) | Ty::Intersection(args) => {
            for arg in args {
                preserve_generics(arg, generics);
            }
        }
        Ty::RawList(inner)
        | Ty::Sequence(inner)
        | Ty::Remote(inner)
        | Ty::Future(inner)
        | Ty::Reference(_, inner) => preserve_generics(inner, generics),
        Ty::Function(parameters, result)
        | Ty::Callable {
            parameters, result, ..
        } => {
            for parameter in parameters {
                preserve_generics(parameter, generics);
            }
            preserve_generics(result, generics);
        }
        _ => {}
    }
}
