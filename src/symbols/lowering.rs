use super::*;

impl Table {
    /// Construct module imports and definitions from the checked semantic compilation.
    pub fn from_compilation(
        compilation: &Compilation,
        program: &Program,
    ) -> Result<Self, FosterError> {
        let mut modules = BTreeMap::<ModuleName, Module>::new();
        for (id, _) in compilation.hir.modules.iter() {
            let name = Context::new(compilation, id).module(id);
            modules.entry(name.clone()).or_insert(Module {
                name,
                types: Vec::new(),
                definitions: Vec::new(),
                imports: Vec::new(),
            });
        }
        for (id, record) in compilation.hir.records.iter() {
            let module = Context::new(compilation, record.module).module(record.module);
            modules.get_mut(&module).unwrap().types.push(TypeBinding {
                name: Name {
                    module,
                    name: record.name.clone(),
                },
                variant: false,
                id: id.into_raw().into_u32(),
            });
        }
        for (id, variant) in compilation.hir.variant_types.iter() {
            let module = Context::new(compilation, variant.module).module(variant.module);
            modules.get_mut(&module).unwrap().types.push(TypeBinding {
                name: Name {
                    module,
                    name: variant.name.clone(),
                },
                variant: true,
                id: id.into_raw().into_u32(),
            });
        }
        let mut synthetic_counts = BTreeMap::new();
        for (id, function) in compilation.hir.functions.iter() {
            if !program.functions.contains_key(&id) {
                continue;
            }
            if let Some(external) = compilation.hir.external_functions.get(&id) {
                let mut definition = external.definition.clone();
                definition.function = raw(id);
                modules
                    .get_mut(&definition.symbol.name.module)
                    .ok_or_else(|| error("external function has no module"))?
                    .definitions
                    .push(definition);
                continue;
            }
            let Some(signature) = compilation.types.function_type(id) else {
                continue;
            };
            let mut context = Context::new(compilation, function.module);
            for name in &function.type_parameters {
                context.generic(name);
            }
            for (index, group) in function.groups.iter().enumerate() {
                context
                    .roots
                    .insert(group.name.clone(), format!("g{index}"));
            }
            for (index, parameter) in function.parameters.iter().enumerate() {
                context.roots.insert(
                    compilation.hir.locals[*parameter].name.clone(),
                    format!("p{index}"),
                );
            }
            let mut descriptor = context.signature(signature);
            descriptor.receiver = function.receiver.is_some();
            descriptor.groups = function
                .groups
                .iter()
                .map(|group| {
                    (
                        context.root(&group.name),
                        context.annotation(&group.element),
                    )
                })
                .collect();
            descriptor.generics = context.generics.len() as u32;
            if let Some(ownership) = compilation.ownership.functions.get(&id) {
                descriptor.result_dependencies = ownership
                    .result_provenance
                    .parameters
                    .iter()
                    .map(|p| *p as u32)
                    .collect();
                descriptor.fresh_result = ownership.result_provenance.fresh_owned;
            }
            let mut local_name = function.name.clone();
            if local_name.contains('$') {
                let ordinal = synthetic_counts
                    .entry((function.module, local_name.clone()))
                    .or_insert(0_u32);
                local_name = format!("{local_name}${ordinal}");
                *ordinal += 1;
            }
            let name = Name {
                module: context.module(function.module),
                name: local_name,
            };
            let symbol = Symbol {
                name: name.clone(),
                receiver: descriptor.receiver,
                overload: descriptor.overload(),
            };
            let module = modules
                .entry(name.module.clone())
                .or_insert_with(|| Module {
                    name: name.module,
                    types: Vec::new(),
                    definitions: Vec::new(),
                    imports: Vec::new(),
                });
            module.definitions.push(Definition {
                symbol,
                descriptor,
                function: raw(id),
                generic_names: {
                    let mut names = context.generics.iter().collect::<Vec<_>>();
                    names.sort_by_key(|(_, index)| **index);
                    names.into_iter().map(|(name, _)| name.clone()).collect()
                },
                public: function.public,
            });
        }
        let definitions = modules
            .values()
            .flat_map(|m| &m.definitions)
            .map(|d| (d.function, d.clone()))
            .collect::<BTreeMap<_, _>>();
        for module in modules.values_mut() {
            module.types.sort_by(|a, b| a.name.cmp(&b.name));
            let mut targets = BTreeSet::new();
            for definition in &module.definitions {
                for instruction in
                    &program.functions[&function_id(definition.function)].instructions
                {
                    if let Some(target) = target(instruction) {
                        targets.insert(raw(target));
                    }
                }
            }
            for target in targets {
                let Some(definition) = definitions.get(&target) else {
                    continue;
                };
                if definition.symbol.name.module != module.name {
                    module.imports.push(Import {
                        symbol: definition.symbol.clone(),
                        required: definition.descriptor.clone(),
                        function: target,
                        generic_names: definition.generic_names.clone(),
                    });
                }
            }
            module.definitions.sort_by(|a, b| a.symbol.cmp(&b.symbol));
            module.imports.sort_by(|a, b| a.symbol.cmp(&b.symbol));
        }
        let table = Self {
            version: FORMAT_VERSION,
            modules: modules.into_values().collect(),
        };
        table.validate(program)?;
        Ok(table)
    }
}

struct Context<'a> {
    compilation: &'a Compilation,
    lexical: ModuleId,
    generics: BTreeMap<String, u32>,
    roots: BTreeMap<String, String>,
}

impl<'a> Context<'a> {
    fn new(compilation: &'a Compilation, lexical: ModuleId) -> Self {
        Self {
            compilation,
            lexical,
            generics: BTreeMap::new(),
            roots: BTreeMap::new(),
        }
    }
    fn module(&self, id: ModuleId) -> ModuleName {
        let name = &self.compilation.hir.modules[id].name;
        if let Some((package, path)) = self.compilation.package.symbol_modules.get(name) {
            return ModuleName {
                package: package.clone(),
                path: path.clone(),
            };
        }
        let embedded = self
            .compilation
            .package
            .modules
            .get(name)
            .is_some_and(|m| m.origin == crate::package::ModuleOrigin::Embedded);
        ModuleName {
            package: if embedded { "foster" } else { "local" }.into(),
            path: name.clone(),
        }
    }
    fn generic(&mut self, name: &str) -> u32 {
        let next = self.generics.len() as u32;
        *self.generics.entry(name.to_owned()).or_insert(next)
    }
    fn root(&self, name: &str) -> String {
        self.roots
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.to_owned())
    }
    fn effects(&self, effects: &[ast::Effect]) -> Vec<Effect> {
        let mut result = effects
            .iter()
            .map(|effect| Effect {
                kind: match effect.kind {
                    ast::EffectKind::Read => "read",
                    ast::EffectKind::Mut => "mut",
                    ast::EffectKind::Reshape => "reshape",
                    ast::EffectKind::Consume => "consume",
                }
                .into(),
                root: self.root(&effect.target.root),
                path: effect.target.children.clone(),
            })
            .collect::<Vec<_>>();
        result.sort();
        result.dedup();
        result
    }
    fn signature(&mut self, signature: &crate::types::FunctionType) -> Descriptor {
        let parameters = signature
            .parameters
            .iter()
            .zip(&signature.parameter_modes)
            .map(|(ty, mode)| Parameter {
                ty: self.ty(*ty),
                mode: (*mode).into(),
            })
            .collect();
        let result = self.ty(signature.result);
        Descriptor {
            generics: self.generics.len() as u32,
            receiver: false,
            parameters,
            result,
            groups: Vec::new(),
            effects: self.effects(&signature.effects),
            suspends: signature.suspends,
            result_dependencies: (0..signature.parameters.len() as u32).collect(),
            fresh_result: false,
        }
    }
    fn ty(&mut self, id: TypeId) -> SymbolType {
        match &self.compilation.types.types[id] {
            Type::Generic(name) => SymbolType::Generic(self.generic(name)),
            Type::Unit => SymbolType::Primitive("unit".into()),
            Type::Bool => SymbolType::Primitive("bool".into()),
            Type::Int => SymbolType::Primitive("int".into()),
            Type::Float => SymbolType::Primitive("float".into()),
            Type::CodePoint => SymbolType::Primitive("code_point".into()),
            Type::Byte => SymbolType::Primitive("byte".into()),
            Type::RawBytes => SymbolType::Primitive("raw_bytes".into()),
            Type::RawByteBuffer => SymbolType::Primitive("raw_byte_buffer".into()),
            Type::Reference { group, value } => {
                SymbolType::Reference(self.root(group), Box::new(self.ty(*value)))
            }
            Type::RawList(value) => SymbolType::Applied("raw_list".into(), vec![self.ty(*value)]),
            Type::Sequence(value) => SymbolType::Applied("sequence".into(), vec![self.ty(*value)]),
            Type::Remote(value) => SymbolType::Applied("remote".into(), vec![self.ty(*value)]),
            Type::Future(value) => SymbolType::Applied("future".into(), vec![self.ty(*value)]),
            Type::Function(signature) => SymbolType::Function(Box::new(self.signature(signature))),
            Type::Record { record, arguments } => {
                let record = &self.compilation.hir.records[*record];
                SymbolType::Nominal(
                    Name {
                        module: self.module(record.module),
                        name: record.name.clone(),
                    },
                    arguments.iter().map(|t| self.ty(*t)).collect(),
                )
            }
            Type::Variant { variant, arguments } => {
                let variant = &self.compilation.hir.variant_types[*variant];
                SymbolType::Nominal(
                    Name {
                        module: self.module(variant.module),
                        name: variant.name.clone(),
                    },
                    arguments.iter().map(|t| self.ty(*t)).collect(),
                )
            }
            Type::Intersection(members) => {
                let mut members = members.iter().map(|t| self.ty(*t)).collect::<Vec<_>>();
                members.sort();
                members.dedup();
                SymbolType::Intersection(members)
            }
            Type::Module(name) => SymbolType::Module(
                self.compilation
                    .hir
                    .module_named(name)
                    .map(|id| self.module(id))
                    .unwrap_or(ModuleName {
                        package: "local".into(),
                        path: name.clone(),
                    }),
            ),
        }
    }
    fn annotation(&mut self, expression: &ast::TypeExpr) -> SymbolType {
        match expression {
            ast::TypeExpr::Unit => SymbolType::Primitive("unit".into()),
            ast::TypeExpr::Reference { group, value } => {
                SymbolType::Reference(self.root(group), Box::new(self.annotation(value)))
            }
            ast::TypeExpr::Intersection(members) => {
                let mut members = members
                    .iter()
                    .map(|t| self.annotation(t))
                    .collect::<Vec<_>>();
                members.sort();
                members.dedup();
                SymbolType::Intersection(members)
            }
            ast::TypeExpr::Named(name, arguments) => {
                if let Some(index) = self.generics.get(name) {
                    return SymbolType::Generic(*index);
                }
                let arguments = arguments
                    .iter()
                    .map(|t| self.annotation(t))
                    .collect::<Vec<_>>();
                let primitive = match name.as_str() {
                    "Bool" => Some("bool"),
                    "Int" => Some("int"),
                    "Float" => Some("float"),
                    "Byte" => Some("byte"),
                    "CodePoint" => Some("code_point"),
                    _ => None,
                };
                if let Some(primitive) = primitive {
                    return SymbolType::Primitive(primitive.into());
                }
                if matches!(name.as_str(), "Sequence" | "Remote" | "Future") {
                    return SymbolType::Applied(name.to_lowercase(), arguments);
                }
                let hir = &self.compilation.hir;
                let lexical = &hir.modules[self.lexical];
                let (module, short) = name
                    .rsplit_once("::")
                    .and_then(|(prefix, short)| {
                        lexical
                            .imports
                            .get(prefix)
                            .copied()
                            .or_else(|| hir.module_named(prefix))
                            .map(|m| (m, short))
                    })
                    .unwrap_or((self.lexical, name.as_str()));
                let resolve = |module| {
                    hir.record_named(module, short)
                        .map(|id| {
                            let r = &hir.records[id];
                            Name {
                                module: self.module(r.module),
                                name: r.name.clone(),
                            }
                        })
                        .or_else(|| {
                            hir.variant_type_named(module, short).map(|id| {
                                let v = &hir.variant_types[id];
                                Name {
                                    module: self.module(v.module),
                                    name: v.name.clone(),
                                }
                            })
                        })
                };
                let resolved = resolve(module)
                    .or_else(|| lexical.imports.values().find_map(|id| resolve(*id)))
                    .unwrap_or(Name {
                        module: self.module(module),
                        name: short.into(),
                    });
                SymbolType::Nominal(resolved, arguments)
            }
            ast::TypeExpr::Function {
                parameters,
                parameter_modes,
                result,
                effects,
                suspends,
            } => {
                let parameters = parameters
                    .iter()
                    .zip(parameter_modes)
                    .map(|(ty, mode)| Parameter {
                        ty: self.annotation(ty),
                        mode: (*mode).into(),
                    })
                    .collect::<Vec<_>>();
                SymbolType::Function(Box::new(Descriptor {
                    generics: self.generics.len() as u32,
                    receiver: false,
                    result: self.annotation(result),
                    groups: Vec::new(),
                    effects: self.effects(effects),
                    suspends: *suspends,
                    result_dependencies: (0..parameters.len() as u32).collect(),
                    fresh_result: false,
                    parameters,
                }))
            }
        }
    }
}
