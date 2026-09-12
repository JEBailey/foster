use super::*;

pub(super) fn build(
    compilation: &Compilation,
    code: &vm::Program,
) -> Result<Interface, FosterError> {
    let definitions = code
        .symbols
        .modules
        .iter()
        .flat_map(|m| &m.definitions)
        .map(|d| (d.function, d))
        .collect::<BTreeMap<_, _>>();
    let identities = compilation
        .hir
        .modules
        .iter()
        .map(|(id, m)| {
            let identity = compilation
                .package
                .symbol_modules
                .get(&m.name)
                .map(|(package, path)| symbols::ModuleName {
                    package: package.clone(),
                    path: path.clone(),
                })
                .unwrap_or(symbols::ModuleName {
                    package: if compilation.package.modules[&m.name].origin
                        == ModuleOrigin::Embedded
                    {
                        "foster"
                    } else {
                        "local"
                    }
                    .into(),
                    path: m.name.clone(),
                });
            (id, identity)
        })
        .collect::<HashMap<_, _>>();
    let names = identities
        .iter()
        .map(|(id, identity)| (identity.clone(), compilation.hir.modules[*id].name.clone()))
        .collect::<BTreeMap<_, _>>();
    let package = compilation
        .package
        .modules
        .values()
        .find(|m| m.origin == ModuleOrigin::Input && m.program.is_some())
        .and_then(|m| compilation.hir.module_named(&m.name))
        .map(|id| identities[&id].package.clone())
        .ok_or_else(|| error("library has no input modules"))?;
    if package == "local" {
        return Err(error(
            "a library needs an explicit package identity (use a project or --package-name)",
        ));
    }
    let mut modules = Vec::new();
    let mut contexts = BTreeMap::new();
    let mut embedded = Vec::new();
    for (module_id, module) in compilation.hir.modules.iter() {
        let source = &compilation.package.modules[&module.name];
        let Some(program) = &source.program else {
            continue;
        };
        if source.origin == ModuleOrigin::Embedded {
            embedded.push(module.name.clone());
            continue;
        }
        let mut declarations = program.clone();
        declarations.functions.clear();
        declarations.tests.clear();
        for constant in &mut declarations.constants {
            constant.value = constant_value(
                &compilation.hir.constants[module.constants[&constant.name]].value,
                &compilation.hir,
            );
        }
        let mut functions = Vec::new();
        for (id, function) in compilation
            .hir
            .functions
            .iter()
            .filter(|(_, f)| f.module == module_id)
        {
            let binding = definitions
                .get(&id.into_raw().into_u32())
                .ok_or_else(|| error("function missing from compiled symbols"))?;
            let descriptor = &binding.descriptor;
            contexts.insert(
                binding.function,
                FunctionContext {
                    default_template: compilation
                        .hir
                        .external_functions
                        .get(&id)
                        .and_then(|external| external.context.default_template.clone())
                        .or_else(|| {
                            program
                                .functions
                                .iter()
                                .find(|source| {
                                    source.name == function.name
                                        && source.span == function.span
                                        && source.public
                                        && source.receiver
                                        && source.intrinsic.is_none()
                                        && !source.body_is_recovery_stub
                                        && source.owner.as_ref().is_some_and(|owner| {
                                            module.records.get(owner).is_some_and(|record| {
                                                !compilation.hir.records[*record]
                                                    .fields
                                                    .iter()
                                                    .any(|field| !field.public)
                                            })
                                        })
                                })
                                .cloned()
                        }),
                    composition_owner: compilation
                        .hir
                        .composition_owners
                        .get(&id)
                        .map(|m| compilation.hir.modules[*m].name.clone()),
                    dispatch: compilation.hir.composition_dispatch.contains(&id),
                    registrations: compilation
                        .hir
                        .modules
                        .iter()
                        .filter(|(m, _)| *m != module_id)
                        .flat_map(|(_, m)| {
                            m.functions
                                .iter()
                                .filter(|(_, ids)| ids.contains(&id))
                                .map(|(name, _)| (m.name.clone(), name.clone()))
                        })
                        .collect(),
                },
            );
            let annotation = |ty| annotation(ty, &binding.generic_names, &names, &module.name);
            let parameters = descriptor
                .parameters
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    Ok(ast::Parameter {
                        span: 0..0,
                        name: format!("p{i}"),
                        ty: Some(annotation(&p.ty)?),
                        type_span: None,
                    })
                })
                .collect::<Result<Vec<_>, FosterError>>()?;
            let groups = descriptor
                .groups
                .iter()
                .map(|(name, ty)| {
                    Ok(ast::GroupParameter {
                        name: name.clone(),
                        element: annotation(ty)?,
                    })
                })
                .collect::<Result<Vec<_>, FosterError>>()?;
            let mut effects = effects(&descriptor.effects)?;
            // Parameter ownership is expressed by consume effects in declarations.
            for (i, p) in descriptor.parameters.iter().enumerate() {
                if p.mode == symbols::Mode::Consume
                    && !effects.iter().any(|e| {
                        e.kind == ast::EffectKind::Consume && e.target.root == format!("p{i}")
                    })
                {
                    effects.push(ast::Effect {
                        kind: ast::EffectKind::Consume,
                        target: ast::GroupPath::root(format!("p{i}")),
                    });
                }
            }
            declarations.functions.push(ast::Function {
                body_is_recovery_stub: true,
                span: 0..0,
                documentation: function.documentation.clone(),
                name: binding.symbol.name.name.clone(),
                owner: function.owner.clone(),
                receiver: descriptor.receiver,
                public: binding.public,
                intrinsic: function.intrinsic.clone(),
                type_parameters: binding.generic_names.clone(),
                groups,
                parameters,
                return_type: Some(annotation(&descriptor.result)?),
                effects_explicit: true,
                effects,
                effect_spans: Vec::new(),
                suspends: descriptor.suspends,
                suspend_span: None,
                body: crate::block::Block::new(),
            });
            functions.push((*binding).clone());
        }
        modules.push(Module {
            path: module.name.clone(),
            identity: identities[&module_id].clone(),
            declarations,
            functions,
        });
    }
    modules.sort_by(|a, b| a.path.cmp(&b.path));
    embedded.sort();
    embedded.dedup();
    let slots = super::dispatch::slots(compilation, code)?;
    Ok(Interface {
        language_version: crate::ownership::LANGUAGE_VERSION,
        ownership_version: crate::ownership::MODEL_VERSION,
        package,
        modules,
        embedded,
        slots,
        contexts,
    })
}

fn constant_value(value: &hir::ConstantValue, hir: &hir::PackageHir) -> ast::Expr {
    use hir::ConstantValue as C;
    match value {
        C::Unit => ast::Expr::Unit,
        C::Bool(v) => ast::Expr::Bool(*v),
        C::Integer(v) => ast::Expr::Integer(*v),
        C::Float(v) => ast::Expr::Float(*v),
        C::String(v) => ast::Expr::String(v.clone()),
        C::CodePoint(v) => ast::Expr::CodePoint(v.to_string()),
        C::Symbol(v) => ast::Expr::Symbol(v.clone()),
        C::List(v) => ast::Expr::List(v.iter().map(|v| constant_value(v, hir)).collect()),
        C::Constant(id) => constant_value(&hir.constants[*id].value, hir),
    }
}

fn effects(source: &[symbols::Effect]) -> Result<Vec<ast::Effect>, FosterError> {
    source
        .iter()
        .map(|effect| {
            Ok(ast::Effect {
                kind: match effect.kind.as_str() {
                    "read" => ast::EffectKind::Read,
                    "mut" => ast::EffectKind::Mut,
                    "reshape" => ast::EffectKind::Reshape,
                    "consume" => ast::EffectKind::Consume,
                    _ => return Err(error("unknown effect kind")),
                },
                target: ast::GroupPath {
                    root: effect.root.clone(),
                    children: effect.path.clone(),
                },
            })
        })
        .collect()
}

fn annotation(
    ty: &symbols::SymbolType,
    generics: &[String],
    modules: &BTreeMap<symbols::ModuleName, String>,
    lexical: &str,
) -> Result<ast::TypeExpr, FosterError> {
    use symbols::SymbolType as T;
    let nested = |t| annotation(t, generics, modules, lexical);
    Ok(match ty {
        T::Primitive(name) => match name.as_str() {
            "unit" => ast::TypeExpr::Unit,
            name => ast::TypeExpr::Named(
                match name {
                    "bool" => "Bool",
                    "int" => "Int",
                    "float" => "Float",
                    "byte" => "Byte",
                    "code_point" => "CodePoint",
                    "raw_bytes" => "RawBytes",
                    "raw_byte_buffer" => "RawByteBuffer",
                    _ => return Err(error("unknown primitive type")),
                }
                .into(),
                vec![],
            ),
        },
        T::Generic(index) => ast::TypeExpr::Named(
            generics
                .get(*index as usize)
                .ok_or_else(|| error("invalid generic index"))?
                .clone(),
            vec![],
        ),
        T::Nominal(name, arguments) => {
            let module = modules
                .get(&name.module)
                .ok_or_else(|| error("unknown nominal module"))?;
            let name = if module == lexical {
                name.name.clone()
            } else {
                format!("{module}.{}", name.name)
            };
            ast::TypeExpr::Named(
                name,
                arguments.iter().map(nested).collect::<Result<_, _>>()?,
            )
        }
        T::Reference(group, value) => ast::TypeExpr::Reference {
            group: group.clone(),
            value: Box::new(nested(value)?),
        },
        T::Applied(name, arguments) => ast::TypeExpr::Named(
            match name.as_str() {
                "raw_list" => "RawList",
                "sequence" => "Sequence",
                "remote" => "Remote",
                "future" => "Future",
                _ => return Err(error("unknown type constructor")),
            }
            .into(),
            arguments.iter().map(nested).collect::<Result<_, _>>()?,
        ),
        T::Intersection(members) => {
            ast::TypeExpr::Intersection(members.iter().map(nested).collect::<Result<_, _>>()?)
        }
        T::Function(signature) => ast::TypeExpr::Function {
            parameters: signature
                .parameters
                .iter()
                .map(|p| nested(&p.ty))
                .collect::<Result<_, _>>()?,
            parameter_modes: signature
                .parameters
                .iter()
                .map(|p| match p.mode {
                    symbols::Mode::Borrow => ast::ParameterMode::Borrow,
                    symbols::Mode::Consume => ast::ParameterMode::Consume,
                })
                .collect(),
            result: Box::new(nested(&signature.result)?),
            effects: effects(&signature.effects)?,
            suspends: signature.suspends,
        },
        T::Module(_) => {
            return Err(error(
                "module values cannot cross a compiled library interface",
            ));
        }
    })
}

pub(crate) fn mount(
    package: &mut Package,
    alias: &str,
    library: Arc<Library>,
) -> Result<(), FosterError> {
    library.validate()?;
    let index = package.libraries.len();
    let names = library
        .interface
        .modules
        .iter()
        .map(|m| {
            (
                m.path.clone(),
                if m.path == "main" {
                    alias.to_owned()
                } else {
                    format!("{alias}.{}", m.path)
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    for module in &library.interface.modules {
        if package.symbol_modules.values().any(|(package, path)| {
            package == &module.identity.package && path == &module.identity.path
        }) {
            return Err(error(format!(
                "duplicate package module {} / {}; compiled dependencies must have a single definition",
                module.identity.package, module.identity.path
            )));
        }
        let mounted = &names[&module.path];
        if package
            .modules
            .get(mounted)
            .is_some_and(|m| m.program.is_some())
        {
            return Err(error(format!("module `{mounted}` already exists")));
        }
        let mut declarations = module.declarations.clone();
        rewrite(&mut declarations, &names);
        for (position, binding) in module.functions.iter().enumerate() {
            let mut context = library
                .interface
                .contexts
                .get(&binding.function)
                .cloned()
                .unwrap_or_default();
            if let Some(template) = &mut context.default_template {
                let mut carrier = module.declarations.clone();
                carrier.functions = vec![template.clone()];
                rewrite(&mut carrier, &names);
                *template = carrier.functions.remove(0);
            }
            if let Some(owner) = &mut context.composition_owner {
                *owner = names[owner].clone();
            }
            for (module, _) in &mut context.registrations {
                *module = names[module].clone();
            }
            package.library_bindings.insert(
                (mounted.clone(), position),
                ExternalFunction {
                    library: index,
                    definition: binding.clone(),
                    context,
                },
            );
        }
        package.symbol_modules.insert(
            mounted.clone(),
            (
                module.identity.package.clone(),
                module.identity.path.clone(),
            ),
        );
        package.modules.insert(
            mounted.clone(),
            crate::package::Module {
                name: mounted.clone(),
                source_path: None,
                source: None,
                program: Some(declarations),
                origin: ModuleOrigin::Dependency,
            },
        );
        let parts = mounted.split('.').collect::<Vec<_>>();
        for end in 1..parts.len() {
            let name = parts[..end].join(".");
            package
                .modules
                .entry(name.clone())
                .or_insert(crate::package::Module {
                    name,
                    source_path: None,
                    source: None,
                    program: None,
                    origin: ModuleOrigin::Dependency,
                });
        }
    }
    package.libraries.push(library);
    Ok(())
}

fn rewrite(program: &mut ast::Program, names: &BTreeMap<String, String>) {
    fn ty(value: &mut ast::TypeExpr, names: &BTreeMap<String, String>) {
        match value {
            ast::TypeExpr::Named(name, args) => {
                if let Some((module, local)) = name.rsplit_once('.')
                    && let Some(replacement) = names.get(module)
                {
                    *name = format!("{replacement}.{local}");
                }
                for arg in args {
                    ty(arg, names);
                }
            }
            ast::TypeExpr::Reference { value, .. } => ty(value, names),
            ast::TypeExpr::Intersection(values) => {
                for value in values {
                    ty(value, names);
                }
            }
            ast::TypeExpr::Function {
                parameters, result, ..
            } => {
                for p in parameters {
                    ty(p, names);
                }
                ty(result, names);
            }
            ast::TypeExpr::Unit => {}
        }
    }
    fn method(m: &mut ast::MethodRequirement, names: &BTreeMap<String, String>) {
        for p in &mut m.parameters {
            if let Some(t) = &mut p.ty {
                ty(t, names);
            }
        }
        if let Some(t) = &mut m.return_type {
            ty(t, names);
        }
        for g in &mut m.groups {
            ty(&mut g.element, names);
        }
    }
    fn function(f: &mut ast::Function, names: &BTreeMap<String, String>) {
        for p in &mut f.parameters {
            if let Some(t) = &mut p.ty {
                ty(t, names);
            }
        }
        if let Some(t) = &mut f.return_type {
            ty(t, names);
        }
        for g in &mut f.groups {
            ty(&mut g.element, names);
        }
        block(&mut f.body, names);
    }
    fn block(body: &mut crate::block::Block<ast::Stmt>, names: &BTreeMap<String, String>) {
        let original = std::mem::take(body);
        for (statement, span) in original.iter_spanned() {
            let mut statement = statement.clone();
            match &mut statement {
                ast::Stmt::Return { value, guard } => {
                    expression(value, names);
                    if let Some(guard) = guard {
                        expression(guard, names);
                    }
                }
                ast::Stmt::Assert { condition, message } => {
                    expression(condition, names);
                    if let Some(message) = message {
                        expression(message, names);
                    }
                }
                ast::Stmt::Loop { body } => block(body, names),
                ast::Stmt::Break { guard } | ast::Stmt::Continue { guard } => {
                    if let Some(guard) = guard {
                        expression(guard, names);
                    }
                }
                ast::Stmt::Bind { value, .. }
                | ast::Stmt::Assign { value, .. }
                | ast::Stmt::Expr(value) => expression(value, names),
                ast::Stmt::Set { place, value } => {
                    expression(place, names);
                    expression(value, names);
                }
                ast::Stmt::Function(f) => function(f, names),
            }
            body.push(statement, span.clone());
        }
    }
    fn expression(value: &mut ast::Expr, names: &BTreeMap<String, String>) {
        use ast::Expr as E;
        match value {
            E::Spanned {
                expression: inner, ..
            }
            | E::Reference(inner)
            | E::MoveOut(inner)
            | E::Remote(inner)
            | E::Await(inner)
            | E::Try(inner)
            | E::Unary { operand: inner, .. }
            | E::Member { object: inner, .. }
            | E::Qualified {
                namespace: inner, ..
            } => expression(inner, names),
            E::List(items) => {
                for item in items {
                    expression(item, names);
                }
            }
            E::Call { callee, arguments } | E::PartialApplication { callee, arguments } => {
                expression(callee, names);
                for argument in arguments {
                    expression(argument, names);
                }
            }
            E::Index {
                object: left,
                index: right,
            }
            | E::Binary { left, right, .. }
            | E::Logical { left, right, .. } => {
                expression(left, names);
                expression(right, names);
            }
            E::Record {
                constructor,
                fields,
            } => {
                expression(constructor, names);
                for field in fields {
                    expression(&mut field.value, names);
                }
            }
            E::Branch { subject, arms } => {
                if let Some(subject) = subject {
                    expression(subject, names);
                }
                for arm in arms {
                    if let ast::BranchTest::Condition(condition) = &mut arm.test {
                        expression(condition, names);
                    }
                    block(&mut arm.body, names);
                }
            }
            E::Closure {
                parameters, body, ..
            } => {
                for parameter in parameters {
                    if let Some(t) = &mut parameter.ty {
                        ty(t, names);
                    }
                }
                match body {
                    ast::ClosureBody::Expression(value) => expression(value, names),
                    ast::ClosureBody::Block(body) => block(body, names),
                }
            }
            E::Unit
            | E::Bool(_)
            | E::Integer(_)
            | E::Float(_)
            | E::String(_)
            | E::CodePoint(_)
            | E::Symbol(_)
            | E::Name(_)
            | E::Placeholder => {}
        }
    }
    for import in &mut program.imports {
        if let Some(name) = names.get(&import.path.join(".")) {
            if import.alias.is_none() {
                import.alias = import.path.last().cloned();
            }
            import.path = name.split('.').map(str::to_owned).collect();
        }
    }
    for r in &mut program.records {
        for f in &mut r.fields {
            ty(&mut f.ty, names);
        }
        for t in &mut r.compositions {
            ty(t, names);
        }
        for m in &mut r.methods {
            method(m, names);
        }
    }
    for v in &mut program.variants {
        for a in &mut v.alternatives {
            match a {
                ast::VariantAlternative::AliasTarget { ty: t, .. } => ty(t, names),
                ast::VariantAlternative::EnumCase { payload, .. } => {
                    if let Some(t) = payload {
                        ty(t, names);
                    }
                }
            }
        }
        for t in &mut v.compositions {
            ty(t, names);
        }
        for m in &mut v.methods {
            method(m, names);
        }
    }
    for f in &mut program.functions {
        function(f, names);
    }
}

pub(super) fn validate_function(
    module: &Module,
    declaration: &ast::Function,
    binding: &symbols::Definition,
    names: &BTreeMap<symbols::ModuleName, String>,
) -> Result<(), FosterError> {
    let descriptor = &binding.descriptor;
    let annotation = |t| annotation(t, &binding.generic_names, names, &module.path);
    let parameters = descriptor
        .parameters
        .iter()
        .enumerate()
        .map(|(i, p)| Ok((format!("p{i}"), annotation(&p.ty)?)))
        .collect::<Result<Vec<_>, FosterError>>()?;
    let mut expected_effects = effects(&descriptor.effects)?;
    for (i, p) in descriptor.parameters.iter().enumerate() {
        if p.mode == symbols::Mode::Consume
            && !expected_effects
                .iter()
                .any(|e| e.kind == ast::EffectKind::Consume && e.target.root == format!("p{i}"))
        {
            expected_effects.push(ast::Effect {
                kind: ast::EffectKind::Consume,
                target: ast::GroupPath::root(format!("p{i}")),
            });
        }
    }
    if declaration.name != binding.symbol.name.name
        || declaration.public != binding.public
        || declaration.receiver != descriptor.receiver
        || declaration.type_parameters != binding.generic_names
        || declaration.suspends != descriptor.suspends
        || declaration.return_type.as_ref() != Some(&annotation(&descriptor.result)?)
        || declaration.effects != expected_effects
        || declaration.parameters.len() != parameters.len()
        || declaration
            .parameters
            .iter()
            .zip(parameters)
            .any(|(a, (name, ty))| a.name != name || a.ty.as_ref() != Some(&ty))
        || declaration.groups.len() != descriptor.groups.len()
    {
        return Err(error("declaration does not match its symbolic descriptor"));
    }
    for (a, (name, t)) in declaration.groups.iter().zip(&descriptor.groups) {
        if &a.name != name || a.element != annotation(t)? {
            return Err(error(
                "group declaration does not match its symbolic descriptor",
            ));
        }
    }
    Ok(())
}
