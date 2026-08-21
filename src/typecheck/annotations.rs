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
                if let Some(generic) = generics.get(name) {
                    if !arguments.is_empty() {
                        return Err(FosterError::runtime(format!(
                            "type parameter `{name}` does not accept arguments"
                        )));
                    }
                    return Ok(generic.clone());
                }
                let builtin = match (name.as_str(), arguments.as_slice()) {
                    ("Unit", []) => Some(Ty::Unit),
                    ("Bool", []) => Some(Ty::Bool),
                    ("Int", []) => Some(Ty::Int),
                    ("Float", []) => Some(Ty::Float),
                    ("String", []) => Some(self.string_type()),
                    ("CodePoint", []) => Some(Ty::CodePoint),
                    ("Byte", []) => Some(Ty::Byte),
                    ("Bytes", []) => Some(Ty::Bytes),
                    ("ByteBuffer", []) => Some(Ty::ByteBuffer),
                    ("Symbol", []) => Some(Ty::Symbol),
                    ("List", [element]) => Some(Ty::List(Box::new(
                        self.annotation_type(module, element, generics)?,
                    ))),
                    ("Sequence", [element]) => Some(Ty::Sequence(Box::new(
                        self.annotation_type(module, element, generics)?,
                    ))),
                    ("Remote", [value]) => Some(Ty::Remote(Box::new(
                        self.annotation_type(module, value, generics)?,
                    ))),
                    ("Future", [value]) => Some(Ty::Future(Box::new(
                        self.annotation_type(module, value, generics)?,
                    ))),
                    (builtin, _)
                        if matches!(
                            builtin,
                            "Unit"
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
                    NominalType::Record(record) => self.hir.records[record].parameters.len(),
                    NominalType::Variant(variant) => {
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
                    .collect::<Result<_, _>>()?;
                Ok(match nominal {
                    NominalType::Record(record) => Ty::Record(record, arguments),
                    NominalType::Variant(variant) => Ty::Variant(variant, arguments),
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
                parameters: parameters
                    .iter()
                    .map(|parameter| self.annotation_type(module, parameter, generics))
                    .collect::<Result<_, _>>()?,
                parameter_modes: parameter_modes.clone(),
                result: Box::new(self.annotation_type(module, result, generics)?),
                // A source-level callable type is a contract. The compiler chooses
                // an erased representation when a concrete callable flows into it.
                erased: true,
                effects: effects.clone(),
                suspends: *suspends,
            }),
        }
    }

    pub(super) fn resolve_nominal_type(
        &self,
        current_module: hir::ModuleId,
        name: &str,
    ) -> Result<NominalType, FosterError> {
        if let Some((qualifier, local_name)) = name.rsplit_once('.') {
            let first = qualifier.split('.').next().unwrap_or(qualifier);
            let module = self.hir.modules[current_module]
                .imports
                .get(first)
                .copied()
                .or_else(|| self.hir.module_named(qualifier))
                .ok_or_else(|| {
                    FosterError::runtime(format!("unknown type module `{qualifier}`"))
                })?;
            let found = self
                .nominal_type_in(module, local_name)
                .ok_or_else(|| FosterError::runtime(format!("unknown type `{name}`")))?;
            let public = match found {
                NominalType::Record(id) => self.hir.records[id].public,
                NominalType::Variant(id) => self.hir.variant_types[id].public,
            };
            if module != current_module && !public {
                return Err(FosterError::runtime(format!("type `{name}` is private")));
            }
            return Ok(found);
        }
        if let Some(found) = self.nominal_type_in(current_module, name) {
            return Ok(found);
        }
        let mut imported = Vec::new();
        for imported_module in self.hir.modules[current_module].imports.values() {
            if let Some(found) = self.nominal_type_in(*imported_module, name) {
                let public = match found {
                    NominalType::Record(id) => self.hir.records[id].public,
                    NominalType::Variant(id) => self.hir.variant_types[id].public,
                };
                if public && !imported.contains(&found) {
                    imported.push(found);
                }
            }
        }
        match imported.as_slice() {
            [found] => Ok(*found),
            [_, _, ..] => Err(FosterError::runtime(format!(
                "imported type `{name}` is ambiguous; qualify it with its module"
            ))),
            [] => Err(FosterError::runtime(format!("unknown type `{name}`"))),
        }
    }

    pub(super) fn nominal_type_in(&self, module: hir::ModuleId, name: &str) -> Option<NominalType> {
        self.hir
            .record_named(module, name)
            .map(NominalType::Record)
            .or_else(|| {
                self.hir
                    .variant_type_named(module, name)
                    .map(NominalType::Variant)
            })
    }

    pub(super) fn private_type_in(&self, ty: &Ty) -> Option<String> {
        match ty {
            Ty::Record(record, arguments) => (!self.hir.records[*record].public)
                .then(|| self.hir.records[*record].name.clone())
                .or_else(|| arguments.iter().find_map(|ty| self.private_type_in(ty))),
            Ty::Intersection(members) => members.iter().find_map(|ty| self.private_type_in(ty)),
            Ty::Variant(variant, arguments) => (!self.hir.variant_types[*variant].public)
                .then(|| self.hir.variant_types[*variant].name.clone())
                .or_else(|| arguments.iter().find_map(|ty| self.private_type_in(ty))),
            Ty::List(element)
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
                .find_map(|ty| self.private_type_in(ty))
                .or_else(|| self.private_type_in(result)),
            _ => None,
        }
    }
}
