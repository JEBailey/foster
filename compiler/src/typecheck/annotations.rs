use super::*;

impl Checker<'_> {
    pub(super) fn annotation_type(
        &mut self,
        module: hir::ModuleId,
        annotation: &crate::ast::TypeExpr,
        generics: &HashMap<String, Ty>,
    ) -> Result<Ty, FosterError> {
        use crate::ast::TypeExpr;
        match annotation {
            TypeExpr::Unit => Ok(Ty::Unit),
            TypeExpr::Intersection(members) => {
                if members.len() < 2 {
                    return Err(FosterError::runtime(
                        "an intersection type requires at least two members",
                    ));
                }
                let members = members
                    .iter()
                    .map(|member| self.annotation_type(module, member, generics))
                    .collect::<Result<Vec<_>, _>>()?;
                if members
                    .iter()
                    .any(|member| !matches!(member, Ty::Record(_, _) | Ty::Sequence(_)))
                {
                    return Err(FosterError::runtime(
                        "intersection members must be structural contract types",
                    ));
                }
                Ok(Ty::Intersection(members))
            }
            TypeExpr::Named(name, arguments) => {
                if name == "Self" && !generics.contains_key(name) {
                    return Err(FosterError::runtime("`Self` is only available in implementation signatures and required methods"));
                }
                if let Some(generic) = generics.get(name) {
                    if !arguments.is_empty() {
                        return Err(FosterError::runtime(format!(
                            "type parameter `{name}` does not accept arguments"
                        )));
                    }
                    return Ok(generic.clone());
                }
                let builtin = match (name.as_str(), arguments.as_slice()) {
                    ("Never", []) => Some(Ty::Never),
                    ("Bool", []) => Some(Ty::Bool),
                    ("Int", []) => Some(Ty::Int),
                    ("RawInt", []) if self.hir.modules[module].name == "core.int" => {
                        Some(Ty::RawInt)
                    }
                    ("Float", []) => Some(Ty::Float),
                    ("String", []) => Some(self.string_type()),
                    ("CodePoint", []) => Some(Ty::CodePoint),
                    ("Byte", []) => Some(Ty::Byte),
                    ("Bytes", []) => Some(self.bytes_type()),
                    ("RawBytes", []) if self.hir.modules[module].name == "core.bytes" => {
                        Some(Ty::RawBytes)
                    }
                    ("ByteBuffer", []) => Some(self.byte_buffer_type()),
                    ("RawByteBuffer", [])
                        if self.hir.modules[module].name == "core.bytes.buffer" =>
                    {
                        Some(Ty::RawByteBuffer)
                    }
                    ("Symbol", []) => Some(self.symbol_type()),
                    ("List", [element]) => {
                        let element = self.annotation_type(module, element, generics)?;
                        Some(self.list_type(element))
                    }
                    ("RawList", [element]) if self.hir.modules[module].name == "core.list" => Some(
                        Ty::RawList(Box::new(self.annotation_type(module, element, generics)?)),
                    ),
                    ("Sequence", [element]) => Some(Ty::Sequence(Box::new(
                        self.annotation_type(module, element, generics)?,
                    ))),
                    ("Remote", [value]) => Some(Ty::Remote(Box::new(
                        self.annotation_type(module, value, generics)?,
                    ))),
                    ("RawFuture", [value]) if self.hir.modules[module].name == "core.future" => {
                        Some(Ty::Future(Box::new(
                            self.annotation_type(module, value, generics)?,
                        )))
                    }
                    ("Future", [value]) => {
                        let future_module = self
                            .hir
                            .module_named("core.future")
                            .ok_or_else(|| FosterError::runtime("Future requires core.future"))?;
                        let record =
                            self.hir
                                .record_named(future_module, "Future")
                                .ok_or_else(|| {
                                    FosterError::runtime("Future requires core.future.Future")
                                })?;
                        Some(Ty::Record(
                            record,
                            vec![self.annotation_type(module, value, generics)?],
                        ))
                    }
                    (builtin, _)
                        if matches!(
                            builtin,
                            "Never"
                                | "Bool"
                                | "Int"
                                | "Float"
                                | "String"
                                | "CodePoint"
                                | "Byte"
                                | "Bytes"
                                | "ByteBuffer"
                                | "Symbol"
                        ) =>
                    {
                        return Err(FosterError::runtime(format!(
                            "type `{builtin}` does not accept type arguments"
                        )));
                    }
                    ("List" | "Sequence" | "Remote" | "Future", _) => {
                        return Err(FosterError::runtime(format!(
                            "type `{name}` expects one type argument"
                        )));
                    }
                    _ => None,
                };
                if let Some(builtin) = builtin {
                    return Ok(builtin);
                }
                let nominal = self.resolve_nominal_type(module, name)?;
                let expected = match nominal {
                    NominalTypeId::Record(record) => self.hir.records[record].parameters.len(),
                    NominalTypeId::Variant(variant) => {
                        self.hir.variant_types[variant].parameters.len()
                    }
                };
                if arguments.len() != expected {
                    return Err(FosterError::runtime(format!(
                        "type `{name}` expects {expected} type argument(s), received {}",
                        arguments.len()
                    )));
                }
                let arguments = arguments
                    .iter()
                    .map(|argument| self.annotation_type(module, argument, generics))
                    .collect::<Result<Vec<_>, _>>()?;
                if let NominalTypeId::Variant(variant) = nominal {
                    let definition = self.hir.variant_types[variant].clone();
                    if definition.kind == crate::ast::VariantKind::Alias
                        && definition.alternatives.len() == 1
                        && definition.compositions.is_empty()
                        && definition.methods.is_empty()
                    {
                        if self.resolving_aliases.contains(&variant) {
                            return Err(FosterError::runtime(format!(
                                "type alias `{}` recursively refers to itself",
                                definition.name
                            )));
                        }
                        let member = self.hir.variants[definition.alternatives[0]]
                            .member
                            .clone()
                            .expect("a type alias has a target type");
                        let alias_generics = definition
                            .parameters
                            .iter()
                            .cloned()
                            .zip(arguments.iter().cloned())
                            .collect::<HashMap<_, _>>();
                        self.resolving_aliases.push(variant);
                        let resolved =
                            self.annotation_type(definition.module, &member, &alias_generics);
                        self.resolving_aliases.pop();
                        return resolved;
                    }
                }
                Ok(match nominal {
                    NominalTypeId::Record(record) => Ty::Record(record, arguments),
                    NominalTypeId::Variant(variant) => Ty::Variant(variant, arguments),
                })
            }
            TypeExpr::Reference { group, value } => Ok(Ty::Reference(
                group.clone(),
                Box::new(self.annotation_type(module, value, generics)?),
            )),
            TypeExpr::Function {
                parameters,
                parameter_modes,
                result,
                effects,
                suspends,
            } => Ok(Ty::Callable {
                parameters: crate::types::Parameter::try_from_parts(
                    parameters
                        .iter()
                        .map(|parameter| self.annotation_type(module, parameter, generics))
                        .collect::<Result<_, _>>()?,
                    parameter_modes.clone(),
                )
                .map_err(|error| FosterError::runtime(error.to_string()))?,
                result: Box::new(self.annotation_type(module, result, generics)?),
                // A source-level callable type is a contract. The compiler chooses
                // an erased representation when a concrete callable flows into it.
                erased: true,
                effects: effects.iter().map(|effect| {
                    let mut effect = effect.clone();
                    effect.capture = !parameters.iter().any(|parameter|
                        matches!(parameter, TypeExpr::Reference { group, .. } if group == &effect.target.root));
                    effect
                }).collect(),
                suspends: *suspends,
            }),
        }
    }

    pub(super) fn resolve_nominal_type(
        &self,
        current_module: hir::ModuleId,
        name: &str,
    ) -> Result<NominalTypeId, FosterError> {
        if let Some(nominal) = self.hir.composition_types.get(name) {
            return Ok(*nominal);
        }
        if self.hir.compiled_modules.contains(&current_module) {
            for (offset, _) in name.rmatch_indices('.') {
                if let Some(module) = self.hir.module_named(&name[..offset]) {
                    let local = &name[offset + 1..];
                    if let Some(record) = self.hir.record_named(module, local) {
                        return Ok(NominalTypeId::Record(record));
                    }
                    if let Some(variant) = self.hir.variant_type_named(module, local) {
                        return Ok(NominalTypeId::Variant(variant));
                    }
                }
            }
        }
        let imported = self.hir.visible_types(current_module, name);
        match imported.as_slice() {
            [found] => Ok(*found),
            [_, _, ..] => Err(FosterError::runtime(format!(
                "imported type `{name}` is ambiguous; qualify it with its module"
            ))),
            [] => Err(FosterError::runtime(format!("unknown type `{name}`"))),
        }
    }

    pub(super) fn private_type_in(&self, ty: &Ty) -> Option<String> {
        match ty {
            Ty::RawInt => Some("RawInt".into()),
            Ty::Record(record, arguments) => (!self.hir.records[*record].public)
                .then(|| self.hir.records[*record].name.clone())
                .or_else(|| arguments.iter().find_map(|ty| self.private_type_in(ty))),
            Ty::Intersection(members) => members.iter().find_map(|ty| self.private_type_in(ty)),
            Ty::Variant(variant, arguments) => (!self.hir.variant_types[*variant].public)
                .then(|| self.hir.variant_types[*variant].name.clone())
                .or_else(|| arguments.iter().find_map(|ty| self.private_type_in(ty))),
            Ty::RawList(element)
            | Ty::Sequence(element)
            | Ty::Remote(element)
            | Ty::Future(element)
            | Ty::Reference(_, element) => self.private_type_in(element),
            Ty::Function(parameters, result) => parameters
                .iter()
                .find_map(|ty| self.private_type_in(ty))
                .or_else(|| self.private_type_in(result)),
            Ty::Callable {
                parameters, result, ..
            } => parameters
                .iter()
                .find_map(|ty| self.private_type_in(&ty.ty))
                .or_else(|| self.private_type_in(result)),
            _ => None,
        }
    }
}
