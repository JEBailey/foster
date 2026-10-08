use super::*;

impl FunctionLowerer<'_> {
    pub(super) fn resolve_associated_function(
        &mut self,
        path: &[&str],
    ) -> Result<Option<FunctionId>, FosterError> {
        if path.len() < 2 || self.locals.contains_key(path[0]) {
            return Ok(None);
        }
        let member = path[path.len() - 1];
        let owner = path[..path.len() - 1].join(".");
        if path.len() == 2
            && matches!(
                path[0],
                "Int" | "Float" | "Byte" | "Bytes" | "ByteBuffer" | "CodePoint" | "String"
            )
        {
            let name = format!("{owner}.{member}");
            let mut candidates = std::iter::once(self.module)
                .chain(self.hir.scope_modules(self.module))
                .filter_map(|module| {
                    if module == self.module {
                        self.hir.function_named(module, &name)
                    } else {
                        self.hir.public_function_named(module, &name)
                    }
                })
                .collect::<Vec<_>>();
            candidates.sort();
            candidates.dedup();
            return match candidates.as_slice() {
                [id] => Ok(Some(*id)),
                [] => Ok(None),
                _ => Err(self.error(format!("associated function `{name}` is ambiguous"))),
            };
        }
        let types = self.hir.visible_types(self.module, &owner);
        let [ty] = types.as_slice() else {
            return Ok(None);
        };
        let (module, actual_name) = self.hir.type_location(*ty);
        let name = format!("{actual_name}.{member}");
        if module == self.module {
            return Ok(self.hir.function_named(module, &name));
        }
        if let Some(function) = self.hir.public_function_named(module, &name) {
            return Ok(Some(function));
        }
        if !self.hir.functions_named(module, &name).is_empty() {
            return Err(self.error(format!("associated function `{name}` is private")));
        }
        Ok(None)
    }

    pub(super) fn resolve_variant_constructor(
        &self,
        type_name: &str,
        case: &str,
    ) -> Result<Option<VariantId>, FosterError> {
        let imported = self
            .hir
            .visible_types(self.module, type_name)
            .into_iter()
            .filter_map(|ty| match ty {
                crate::types::NominalTypeId::Variant(id) => Some(id),
                _ => None,
            })
            .collect::<Vec<_>>();
        let parent = match imported.as_slice() {
            [id] => Some(*id),
            [] => None,
            _ => return Err(self.error(format!("imported type `{type_name}` is ambiguous"))),
        };
        let Some(parent) = parent else {
            return Ok(None);
        };
        if self.hir.variant_types[parent].kind != ast::VariantKind::Enum {
            return Ok(None);
        }
        self.hir.variant_types[parent]
            .alternatives
            .iter()
            .copied()
            .find(|variant| self.hir.variants[*variant].name == case)
            .map(Some)
            .ok_or_else(|| self.error(format!("enum `{type_name}` has no case `{case}`")))
    }

    pub(super) fn resolve_variant(
        &self,
        path: &[String],
        enum_accessor: bool,
    ) -> Result<VariantId, FosterError> {
        if enum_accessor
            && path.len() >= 2
            && let Some(variant) = self.resolve_variant_constructor(
                &path[..path.len() - 1].join("."),
                &path[path.len() - 1],
            )?
        {
            return Ok(variant);
        }
        if path.len() == 1 {
            let local = self.hir.modules[self.module]
                .variant_types
                .values()
                .filter(|parent| self.hir.variant_types[**parent].kind == ast::VariantKind::Enum)
                .flat_map(|parent| self.hir.variant_types[*parent].alternatives.iter().copied())
                .filter(|variant| self.hir.variants[*variant].name == path[0])
                .collect::<Vec<_>>();
            return match local.as_slice() {
                [variant] => Ok(*variant),
                [_, _, ..] => Err(self.error(format!(
                    "enum case `{}` is ambiguous; qualify it with its enum type",
                    path[0]
                ))),
                [] => Err(self.error(format!(
                    "unknown enum case `{}`; qualify it with its enum type",
                    path[0]
                ))),
            };
        }
        if path.len() != 2 {
            if path.len() == 3
                && enum_accessor
                && let Some(module) = self.imports.get(&path[0]).copied().or_else(|| {
                    (path[0] == ast::ITERATION_OPTION_MODULE)
                        .then(|| self.hir.module_named("core.option"))
                        .flatten()
                })
                && let Some(parent) = self.hir.variant_type_named(module, &path[1])
            {
                if !self.hir.variant_types[parent].public {
                    return Err(self.error(format!("type `{}.{}` is private", path[0], path[1])));
                }
                if self.hir.variant_types[parent].kind != ast::VariantKind::Enum {
                    return Err(self.error(format!(
                        "type alias `{}.{}` has no enum cases to pattern match",
                        path[0], path[1]
                    )));
                }
                return self.hir.variant_types[parent]
                    .alternatives
                    .iter()
                    .copied()
                    .find(|id| self.hir.variants[*id].name == path[2])
                    .ok_or_else(|| {
                        self.error(format!(
                            "enum `{}.{}` has no case `{}`",
                            path[0], path[1], path[2]
                        ))
                    });
            }
            return Err(self.error("enum pattern must name a case"));
        }
        if !enum_accessor && let Some(module) = self.imports.get(&path[0]).copied() {
            let matches = self.hir.modules[module]
                .variant_types
                .values()
                .filter(|parent| {
                    self.hir.variant_types[**parent].public
                        && self.hir.variant_types[**parent].kind == ast::VariantKind::Enum
                })
                .flat_map(|parent| self.hir.variant_types[*parent].alternatives.iter().copied())
                .filter(|variant| self.hir.variants[*variant].name == path[1])
                .collect::<Vec<_>>();
            return match matches.as_slice() {
                [variant] => Ok(*variant),
                [] => Err(self.error(format!(
                    "module `{}` has no public enum case `{}`",
                    path[0], path[1]
                ))),
                _ => Err(self.error(format!(
                    "enum case `{}.{}` is ambiguous; include its enum type name",
                    path[0], path[1]
                ))),
            };
        }
        if !enum_accessor {
            return Err(self.error(format!(
                "enum type access uses `.`; write `{}.{}`",
                path[0], path[1]
            )));
        }
        if let Some(variant) = self.resolve_variant_constructor(&path[0], &path[1])? {
            return Ok(variant);
        }
        let parent = self
            .hir
            .variant_type_named(self.module, &path[0])
            .ok_or_else(|| self.error(format!("unknown enum type `{}`", path[0])))?;
        if self.hir.variant_types[parent].kind != ast::VariantKind::Enum {
            return Err(self.error(format!(
                "type alias `{}` has no enum cases to pattern match",
                path[0]
            )));
        }
        self.hir.variant_types[parent]
            .alternatives
            .iter()
            .copied()
            .find(|id| self.hir.variants[*id].name == path[1])
            .ok_or_else(|| self.error(format!("enum `{}` has no case `{}`", path[0], path[1])))
    }

    pub(super) fn resolve_name(&mut self, name: &str) -> Result<ResolvedName, FosterError> {
        if self.self_name.as_deref() == Some(name) {
            return Ok(ResolvedName::Function(self.function));
        }
        if let Some(local) = self.locals.get(name) {
            if self.hir.locals[*local].function != self.function && !self.captures.contains(local) {
                self.captures.push(*local);
            }
            return Ok(ResolvedName::Local(*local));
        }
        if let Some(constant) = self.hir.constant_named(self.module, name) {
            return Ok(ResolvedName::Constant(constant));
        }
        if let Some(function) = self.hir.function_named(self.module, name) {
            return Ok(ResolvedName::Function(function));
        }
        let variants = self.hir.modules[self.module]
            .variant_types
            .values()
            .filter(|parent| self.hir.variant_types[**parent].kind == ast::VariantKind::Enum)
            .flat_map(|parent| self.hir.variant_types[*parent].alternatives.iter().copied())
            .filter(|variant| self.hir.variants[*variant].name == name)
            .collect::<Vec<_>>();
        match variants.as_slice() {
            [variant] => return Ok(ResolvedName::Variant(*variant)),
            [_, _, ..] => {
                return Err(self.error(format!(
                    "enum case `{name}` is ambiguous; qualify it with its enum type"
                )));
            }
            [] => {}
        }
        if let Some(record) = self.hir.record_named(self.module, name) {
            return Ok(ResolvedName::Record(record));
        }
        if let Some(module) = self.imports.get(name) {
            return Ok(ResolvedName::Module(*module));
        }
        let mut imported = self.hir.modules[self.module]
            .imported_values
            .get(name)
            .cloned()
            .unwrap_or_default();
        for ty in self.hir.visible_types(self.module, name) {
            if let crate::types::NominalTypeId::Record(record) = ty {
                let resolved = ResolvedName::Record(record);
                if !imported.contains(&resolved) {
                    imported.push(resolved);
                }
            }
        }
        imported.sort_by_key(|value| format!("{value:?}"));
        imported.dedup();
        match imported.as_slice() {
            [resolved] => Ok(*resolved),
            [_, _, ..] => Err(self.error(format!(
                "imported name `{name}` is ambiguous; qualify it with its module"
            ))),
            [] => Builtin::from_source_name(name)
                .map(ResolvedName::Builtin)
                .ok_or_else(|| self.error(format!("unknown name `{name}`"))),
        }
    }

    pub(super) fn resolve_qualified(&self, path: &[&str]) -> Result<ResolvedName, FosterError> {
        let mut module = self.imports[path[0]];
        for (index, component) in path.iter().enumerate().skip(1) {
            let last = index + 1 == path.len();
            if last && let Some(constant) = self.hir.constant_named(module, component) {
                if !self.hir.constants[constant].public {
                    return Err(self.error(format!(
                        "constant `{}.{component}` is private",
                        self.hir.modules[module].name
                    )));
                }
                return Ok(ResolvedName::Constant(constant));
            }
            if last && let Some(function) = self.hir.public_function_named(module, component) {
                return Ok(ResolvedName::Function(function));
            }
            if last && !self.hir.functions_named(module, component).is_empty() {
                return Err(self.error(format!(
                    "function `{}.{component}` is private",
                    self.hir.modules[module].name
                )));
            }
            if last && let Some(record) = self.hir.record_named(module, component) {
                if !self.hir.records[record].public {
                    return Err(self.error(format!(
                        "type `{}.{component}` is private",
                        self.hir.modules[module].name
                    )));
                }
                return Ok(ResolvedName::Record(record));
            }
            if last {
                let matches = self.hir.modules[module]
                    .variant_types
                    .values()
                    .filter(|parent| {
                        self.hir.variant_types[**parent].public
                            && self.hir.variant_types[**parent].kind == ast::VariantKind::Enum
                    })
                    .flat_map(|parent| self.hir.variant_types[*parent].alternatives.iter().copied())
                    .filter(|variant| self.hir.variants[*variant].name == *component)
                    .collect::<Vec<_>>();
                match matches.as_slice() {
                    [variant] => return Ok(ResolvedName::Variant(*variant)),
                    [_, _, ..] => {
                        return Err(self.error(format!(
                            "enum case `{}` is ambiguous; include its enum type name",
                            component
                        )));
                    }
                    [] => {}
                }
            }
            let child_name = format!("{}.{}", self.hir.modules[module].name, component);
            if let Some(child) = self.hir.module_named(&child_name) {
                module = child;
                if last {
                    return Ok(ResolvedName::Module(module));
                }
            } else {
                return Err(self.error(format!(
                    "module `{}` has no member `{component}`",
                    self.hir.modules[module].name
                )));
            }
        }
        Ok(ResolvedName::Module(module))
    }

    pub(super) fn error(&self, message: impl Into<String>) -> FosterError {
        FosterError::runtime(format!(
            "in `{}.{}`: {}",
            self.hir.modules[self.module].name,
            self.hir.functions[self.function].name,
            message.into()
        ))
    }
}

pub(super) fn qualified_path(expression: &ast::Expr) -> Option<Vec<&str>> {
    fn collect<'a>(expression: &'a ast::Expr, path: &mut Vec<&'a str>) -> bool {
        match expression.unspanned() {
            ast::Expr::Name(name) => {
                path.push(name);
                true
            }
            ast::Expr::Qualified { namespace, name } if collect(namespace, path) => {
                path.push(name);
                true
            }
            _ => false,
        }
    }

    let mut path = Vec::new();
    collect(expression, &mut path).then_some(path)
}

pub(super) fn accessor_path(expression: &ast::Expr) -> Option<Vec<&str>> {
    let ast::Expr::Member { object, name } = expression.unspanned() else {
        return None;
    };
    let mut path = qualified_path(object).or_else(|| accessor_path(object))?;
    path.push(name);
    Some(path)
}
