use super::*;

impl FunctionLowerer<'_> {
    pub(super) fn resolve_associated_function(
        &mut self,
        path: &[&str],
    ) -> Result<Option<FunctionId>, FosterError> {
        let (module, type_name, member, imported) = match path {
            [type_name, member] => {
                if matches!(
                    *type_name,
                    "Byte" | "Bytes" | "ByteBuffer" | "CodePoint" | "String"
                ) {
                    let qualified_name = format!("{type_name}.{member}");
                    let mut candidates = std::iter::once(self.module)
                        .chain(self.imports.values().copied())
                        .filter_map(|module| {
                            if module == self.module {
                                self.hir.function_named(module, &qualified_name)
                            } else {
                                self.hir.public_function_named(module, &qualified_name)
                            }
                        })
                        .collect::<Vec<_>>();
                    candidates.sort();
                    candidates.dedup();
                    return match candidates.as_slice() {
                        [function] => Ok(Some(*function)),
                        [] => Ok(None),
                        _ => Err(self.error(format!(
                            "associated function `{qualified_name}` is ambiguous"
                        ))),
                    };
                }
                let resolved = self.resolve_name(type_name)?;
                let type_module = match resolved {
                    ResolvedName::Record(record) => self.hir.records[record].module,
                    ResolvedName::Variant(variant) => {
                        self.hir.variant_types[self.hir.variants[variant].parent].module
                    }
                    _ => return Ok(None),
                };
                (type_module, *type_name, *member, type_module != self.module)
            }
            [module_alias, type_name, member] => {
                let Some(module) = self.imports.get(*module_alias).copied() else {
                    return Ok(None);
                };
                let names_type = self.hir.record_named(module, type_name).is_some()
                    || self.hir.variant_type_named(module, type_name).is_some()
                    || (matches!(
                        *type_name,
                        "Byte" | "Bytes" | "ByteBuffer" | "CodePoint" | "String"
                    ) && self
                        .hir
                        .functions_named(module, &format!("{type_name}.{member}"))
                        .iter()
                        .any(|function| self.hir.functions[*function].public));
                if !names_type {
                    return Ok(None);
                }
                (module, *type_name, *member, true)
            }
            _ => return Ok(None),
        };

        let qualified_name = format!("{type_name}.{member}");
        let function = if imported {
            self.hir.public_function_named(module, &qualified_name)
        } else {
            self.hir.function_named(module, &qualified_name)
        };
        let Some(function) = function else {
            if imported && !self.hir.functions_named(module, &qualified_name).is_empty() {
                return Err(
                    self.error(format!("associated function `{qualified_name}` is private"))
                );
            }
            return Ok(None);
        };
        Ok(Some(function))
    }

    pub(super) fn resolve_variant_constructor(
        &self,
        type_name: &str,
        case: &str,
    ) -> Result<Option<VariantId>, FosterError> {
        let parent = if let Some(parent) = self.hir.variant_type_named(self.module, type_name) {
            Some(parent)
        } else {
            let mut imported = Vec::new();
            for module in self.imports.values() {
                if let Some(parent) = self.hir.variant_type_named(*module, type_name)
                    && self.hir.variant_types[parent].public
                    && !imported.contains(&parent)
                {
                    imported.push(parent);
                }
            }
            match imported.as_slice() {
                [parent] => Some(*parent),
                [_, _, ..] => {
                    return Err(self.error(format!(
                        "imported type `{type_name}` is ambiguous; qualify it with its module"
                    )));
                }
                [] => None,
            }
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
                [] => {
                    let mut imported = Vec::new();
                    for module in self.imports.values() {
                        for parent in self.hir.modules[*module].variant_types.values() {
                            if !self.hir.variant_types[*parent].public
                                || self.hir.variant_types[*parent].kind != ast::VariantKind::Enum
                            {
                                continue;
                            }
                            for variant in &self.hir.variant_types[*parent].alternatives {
                                if self.hir.variants[*variant].name == path[0]
                                    && !imported.contains(variant)
                                {
                                    imported.push(*variant);
                                }
                            }
                        }
                    }
                    match imported.as_slice() {
                        [variant] => Ok(*variant),
                        [] => Err(self.error(format!("unknown enum case `{}`", path[0]))),
                        _ => Err(self.error(format!("imported enum case `{}` is ambiguous; qualify it with its module or enum type", path[0]))),
                    }
                }
            };
        }
        if path.len() != 2 {
            if path.len() == 3
                && enum_accessor
                && let Some(module) = self.imports.get(&path[0]).copied()
                && let Some(parent) = self.hir.variant_type_named(module, &path[1])
            {
                if !self.hir.variant_types[parent].public {
                    return Err(self.error(format!("type `{}.{}` is private", path[0], path[1])));
                }
                if self.hir.variant_types[parent].kind != ast::VariantKind::Enum {
                    return Err(self.error(format!(
                        "type union `{}.{}` has no enum cases to pattern match",
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
                "type union `{}` has no enum cases to pattern match",
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
        let mut imported = Vec::new();
        for module in self.imports.values() {
            if let Some(constant) = self.hir.constant_named(*module, name)
                && self.hir.constants[constant].public
                && !imported.contains(&ResolvedName::Constant(constant))
            {
                imported.push(ResolvedName::Constant(constant));
            }
            if let Some(function) = self.hir.public_function_named(*module, name)
                && !imported.contains(&ResolvedName::Function(function))
            {
                imported.push(ResolvedName::Function(function));
            }
            let matching_variants = self.hir.modules[*module]
                .variant_types
                .values()
                .filter(|parent| {
                    self.hir.variant_types[**parent].public
                        && self.hir.variant_types[**parent].kind == ast::VariantKind::Enum
                })
                .flat_map(|parent| self.hir.variant_types[*parent].alternatives.iter().copied())
                .filter(|variant| self.hir.variants[*variant].name == name)
                .collect::<Vec<_>>();
            for variant in matching_variants {
                let resolved = ResolvedName::Variant(variant);
                if !imported.contains(&resolved) {
                    imported.push(resolved);
                }
            }
            if let Some(record) = self.hir.record_named(*module, name)
                && self.hir.records[record].public
                && !self.hir.modules[*module]
                    .variant_types
                    .values()
                    .filter(|parent| {
                        self.hir.variant_types[**parent].kind == ast::VariantKind::Enum
                    })
                    .flat_map(|parent| self.hir.variant_types[*parent].alternatives.iter())
                    .any(|variant| self.hir.variants[*variant].name == name)
                && !imported.contains(&ResolvedName::Record(record))
            {
                imported.push(ResolvedName::Record(record));
            }
        }
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
    let mut path = qualified_path(object)?;
    path.push(name);
    Some(path)
}
