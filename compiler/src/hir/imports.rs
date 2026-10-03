use super::*;
use crate::types::NominalTypeId;

impl PackageHir {
    /// Resolve a type through local declarations, explicit imports, or an imported namespace.
    pub fn visible_types(&self, module: ModuleId, name: &str) -> Vec<NominalTypeId> {
        let local = |module, name: &str| {
            self.record_named(module, name)
                .map(NominalTypeId::Record)
                .or_else(|| {
                    self.variant_type_named(module, name)
                        .map(NominalTypeId::Variant)
                })
        };
        if let Some(found) = local(module, name) {
            return vec![found];
        }
        if let Some(found) = self.modules[module].imported_types.get(name) {
            return found.clone();
        }
        for (path, target) in &self.modules_by_name {
            if let Some(tail) = name.strip_prefix(&format!("{path}."))
                && let Some(id) = local(*target, tail)
                && self.type_public(id)
                && (self.modules[module].imports.values().any(|m| m == target)
                    || self.modules[module]
                        .imported_types
                        .values()
                        .flatten()
                        .any(|item| *item == id))
            {
                return vec![id];
            }
        }
        if let Some((head, tail)) = name.split_once('.') {
            if let Some(target) = self.modules[module].imports.get(head) {
                return local(*target, tail)
                    .filter(|id| self.type_public(*id))
                    .into_iter()
                    .collect();
            }
            let mut found = Vec::new();
            for owner in self.modules[module]
                .imported_types
                .get(head)
                .into_iter()
                .flatten()
            {
                let (target, owner_name) = self.type_location(*owner);
                if let Some(id) = local(target, &format!("{owner_name}.{tail}"))
                    && self.type_public(id)
                    && !found.contains(&id)
                {
                    found.push(id);
                }
            }
            return found;
        }
        Vec::new()
    }

    pub fn type_public(&self, id: NominalTypeId) -> bool {
        let public = match id {
            NominalTypeId::Record(id) => self.records[id].public,
            NominalTypeId::Variant(id) => self.variant_types[id].public,
        };
        let (module, name) = self.type_location(id);
        public
            && name.rsplit_once('.').is_none_or(|(parent, _)| {
                self.record_named(module, parent)
                    .map(NominalTypeId::Record)
                    .or_else(|| {
                        self.variant_type_named(module, parent)
                            .map(NominalTypeId::Variant)
                    })
                    .is_some_and(|parent| self.type_public(parent))
            })
    }

    pub fn type_location(&self, id: NominalTypeId) -> (ModuleId, &str) {
        match id {
            NominalTypeId::Record(id) => (self.records[id].module, &self.records[id].name),
            NominalTypeId::Variant(id) => {
                (self.variant_types[id].module, &self.variant_types[id].name)
            }
        }
    }

    pub fn install_item_imports(&mut self, package: &Package) -> Result<(), FosterError> {
        for (_, module) in self.modules.iter() {
            for name in module
                .records
                .keys()
                .chain(module.variant_types.keys())
                .chain(module.constants.keys())
            {
                if let Some((parent, _)) = name.rsplit_once('.')
                    && !module.records.contains_key(parent)
                    && !module.variant_types.contains_key(parent)
                {
                    return Err(FosterError::runtime(format!(
                        "child declaration `{name}` has unknown parent type `{parent}`"
                    )));
                }
            }
        }
        for (module_name, source) in &package.modules {
            let Some(program) = &source.program else {
                continue;
            };
            let module = self.modules_by_name[module_name];
            for import in &program.imports {
                if import.alias.as_deref() == Some(ast::ITERATION_OPTION_MODULE) {
                    continue;
                }
                let path = import.path.join(".");
                if !import.static_ && !import.wildcard && self.module_named(&path).is_some() {
                    continue;
                }
                let (target, prefix) = (1..=import.path.len())
                    .rev()
                    .find_map(|count| {
                        self.module_named(&import.path[..count].join("."))
                            .map(|target| (target, import.path[count..].join(".")))
                    })
                    .ok_or_else(|| FosterError::runtime(format!("unknown import `{path}`")))?;
                let target_program = package.modules[&self.modules[target].name].program.as_ref();
                let owner = if import.wildcard {
                    prefix.as_str()
                } else {
                    prefix.rsplit_once('.').map_or("", |(owner, _)| owner)
                };
                if !owner.is_empty() {
                    let ty = self
                        .record_named(target, owner)
                        .map(NominalTypeId::Record)
                        .or_else(|| {
                            self.variant_type_named(target, owner)
                                .map(NominalTypeId::Variant)
                        });
                    if !ty.is_some_and(|ty| self.type_public(ty)) {
                        return Err(FosterError::runtime(format!(
                            "import `{path}` has no public parent type `{owner}`"
                        )));
                    }
                }
                let mut selected = 0;
                if import.static_ {
                    let matches = |name: &str| {
                        if import.wildcard {
                            let (owner, _) = name.rsplit_once('.').unwrap_or(("", name));
                            owner == prefix
                        } else {
                            name == prefix
                        }
                    };
                    let functions = self.modules[target].functions.clone();
                    for (name, ids) in functions {
                        if !matches(&name) {
                            continue;
                        }
                        for id in ids {
                            let function = &self.functions[id];
                            let receiver = self.external_functions.get(&id).map_or_else(
                                || {
                                    target_program
                                        .and_then(|p| {
                                            p.functions.iter().find(|f| f.span == function.span)
                                        })
                                        .is_some_and(|f| f.receiver)
                                },
                                |external| external.definition.descriptor.receiver,
                            );
                            if !function.public || receiver {
                                continue;
                            }
                            let local = import
                                .alias
                                .clone()
                                .unwrap_or_else(|| name.rsplit('.').next().unwrap().into());
                            self.modules[module]
                                .imported_values
                                .entry(local)
                                .or_default()
                                .push(ResolvedName::Function(id));
                            selected += 1;
                        }
                    }
                    let constants = self.modules[target].constants.clone();
                    for (name, id) in constants {
                        if matches(&name) && self.constants[id].public {
                            let local = import
                                .alias
                                .clone()
                                .unwrap_or_else(|| name.rsplit('.').next().unwrap().into());
                            self.modules[module]
                                .imported_values
                                .entry(local)
                                .or_default()
                                .push(ResolvedName::Constant(id));
                            selected += 1;
                        }
                    }
                } else {
                    let types = self.modules[target]
                        .records
                        .iter()
                        .map(|(n, id)| (n.clone(), NominalTypeId::Record(*id)))
                        .chain(
                            self.modules[target]
                                .variant_types
                                .iter()
                                .map(|(n, id)| (n.clone(), NominalTypeId::Variant(*id))),
                        )
                        .collect::<Vec<_>>();
                    for (name, id) in types {
                        let matches = if import.wildcard {
                            name.rsplit_once('.')
                                .map_or(prefix.is_empty(), |(owner, _)| owner == prefix)
                        } else {
                            name == prefix
                        };
                        if matches && self.type_public(id) {
                            let local = import
                                .alias
                                .clone()
                                .unwrap_or_else(|| name.rsplit('.').next().unwrap().into());
                            self.modules[module]
                                .imported_types
                                .entry(local)
                                .or_default()
                                .push(id);
                            selected += 1;
                        }
                    }
                }
                if !import.wildcard && selected == 0 {
                    return Err(FosterError::runtime(format!(
                        "import `{path}` does not name a public {}",
                        if import.static_ {
                            "function without self or constant"
                        } else {
                            "type"
                        }
                    )));
                }
                if !import.wildcard || !prefix.is_empty() {
                    self.modules[module].imports_with_spans.push(ImportBinding {
                        target,
                        name: import
                            .alias
                            .clone()
                            .unwrap_or_else(|| prefix.rsplit('.').next().unwrap().into()),
                        span: import.span.clone(),
                        item_name: Some(prefix),
                    });
                }
            }
        }
        for (_, module) in self.modules.iter_mut() {
            for types in module.imported_types.values_mut() {
                types.sort();
                types.dedup();
            }
            for values in module.imported_values.values_mut() {
                values.sort_by_key(|value| format!("{value:?}"));
                values.dedup();
            }
        }
        Ok(())
    }

    pub fn scope_modules(&self, module: ModuleId) -> Vec<ModuleId> {
        let mut modules = self.modules[module]
            .imports
            .values()
            .copied()
            .collect::<Vec<_>>();
        modules.extend(
            self.modules[module]
                .imported_types
                .values()
                .flatten()
                .map(|ty| self.type_location(*ty).0),
        );
        modules.extend(
            self.modules[module]
                .imported_values
                .values()
                .flatten()
                .filter_map(|value| match value {
                    ResolvedName::Function(id) => Some(self.functions[*id].module),
                    ResolvedName::Constant(id) => Some(self.constants[*id].module),
                    _ => None,
                }),
        );
        modules.sort();
        modules.dedup();
        modules
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        compiler,
        package::{Module, ModuleOrigin, Package},
    };

    fn check(
        imports: &str,
        body: &str,
    ) -> Result<compiler::Compilation, crate::error::FosterError> {
        let mut package = Package::from_program_with_core(
            "main",
            crate::parse("func main() -> Int { 0 }").unwrap(),
        )
        .unwrap();
        package.modules.get_mut("main").unwrap().program =
            Some(crate::parse(&format!("{imports}\nfunc main() -> Int {{ {body} }}")).unwrap());
        package.modules.insert("items".into(), Module {
            name: "items".into(), source_path: None, source: None, origin: ModuleOrigin::Dependency,
            program: Some(crate::parse("pub type Foo = { pub value: Int }\npub type Foo.Child = { pub value: Int }\ntype Hidden = {}\npub const ANSWER = 42\npub const Foo.ANSWER = 42\npub enum Foo.Choice = Value(Int) | Empty\nimpl Foo.Child { pub func create() -> Foo::Child { Foo.Child { value: 42 } } }\npub func answer() -> Int { 42 }\nimpl Foo {\npub func create() -> Foo { Foo { value: 42 } }\npub func read(self) -> Int [read self] { self.value }\n}\n").unwrap()),
        });
        compiler::check(package)
    }

    #[test]
    fn named_types_aliases_and_children_preserve_identity() {
        check("import items.Foo as Thing", "Thing.create().read()").unwrap();
        check(
            "import items.Foo.Child as Child",
            "Child { value: 42 }.value",
        )
        .unwrap();
        check("import items.Foo.*", "Child { value: 42 }.value").unwrap();
        check("import items.Foo", "Foo.Child.create().value").unwrap();
        check("import items.Foo", "Foo.ANSWER").unwrap();
        check("import items.Foo", "branch Foo.Choice.Value(42) { Foo::Choice.Value(value) -> value\nFoo::Choice.Empty -> 0 }").unwrap();
        check(
            "import items.Foo\nimport items.Foo.*",
            "Foo.Child { value: 42 }.value",
        )
        .unwrap();
    }

    #[test]
    fn wildcard_kinds_are_disjoint() {
        check("import items.*", "Foo.create().read()").unwrap();
        assert!(
            check("import items.*", "answer()")
                .unwrap_err()
                .message
                .contains("unknown name")
        );
        assert!(check("import static items.*", "Foo { value: 42 }.value").is_err());
        check("import static items.*", "answer() + ANSWER - 42").unwrap();
        check("import static items.Foo.*", "create().read()").unwrap();
        check("import static items.Foo.ANSWER as value", "value").unwrap();
        let compilation = check("import static items.Foo.create as make", "make().read()").unwrap();
        assert_eq!(
            crate::vm::run(&compilation).unwrap(),
            crate::vm::Value::Integer(42)
        );
        assert!(check("import static items.Foo.*", "Child { value: 42 }.value").is_err());
        assert!(check("import static items.Foo.read", "42").is_err());
    }

    #[test]
    fn namespace_imports_do_not_expose_unqualified_items() {
        check("import items", "items::answer()").unwrap();
        assert!(check("import items", "answer()").is_err());
        assert!(check("import items", "Foo { value: 42 }.value").is_err());
        assert!(check("import items.Hidden", "42").is_err());
        assert!(check("import items.Missing.*", "42").is_err());
        check("import static items.answer as reply", "reply()").unwrap();
    }
}
