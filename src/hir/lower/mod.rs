use super::*;

mod closure;
mod function;
mod resolution;

impl PackageHir {
    pub fn lower(package: &Package) -> Result<Self, FosterError> {
        let mut hir = Self::default();
        let mut test_functions = HashMap::<(ModuleId, usize), FunctionId>::new();

        for name in package.modules.keys() {
            let id = hir.modules.alloc(Module {
                name: name.clone(),
                documentation: package.modules[name]
                    .program
                    .as_ref()
                    .and_then(|program| program.documentation.clone()),
                source_path: package.modules[name].source_path.clone(),
                imports_with_spans: Vec::new(),
                functions: BTreeMap::new(),
                constants: BTreeMap::new(),
                records: BTreeMap::new(),
                variant_types: BTreeMap::new(),
                imports: BTreeMap::new(),
            });
            hir.modules_by_name.insert(name.clone(), id);
        }

        for (module_name, source_module) in &package.modules {
            let Some(program) = &source_module.program else {
                continue;
            };
            let module = hir.modules_by_name[module_name];
            for source in &program.constants {
                if hir.modules[module].constants.contains_key(&source.name)
                    || hir.modules[module].functions.contains_key(&source.name)
                    || hir.modules[module].records.contains_key(&source.name)
                    || hir.modules[module].variant_types.contains_key(&source.name)
                {
                    return Err(FosterError::runtime(format!(
                        "module `{module_name}` defines `{}` more than once",
                        source.name
                    )));
                }
                let constant = hir.constants.alloc(Constant {
                    span: source.span.clone(),
                    documentation: source.documentation.clone(),
                    module,
                    name: source.name.clone(),
                    public: source.public,
                    value: ConstantValue::Unit,
                });
                hir.modules[module]
                    .constants
                    .insert(source.name.clone(), constant);
            }
            for source in &program.variants {
                if hir.modules[module].records.contains_key(&source.name)
                    || hir.modules[module].variant_types.contains_key(&source.name)
                    || hir.modules[module].constants.contains_key(&source.name)
                {
                    return Err(FosterError::runtime(format!(
                        "module `{module_name}` defines `{}` more than once",
                        source.name
                    )));
                }
                let parent = hir.variant_types.alloc(VariantType {
                    span: source.span.clone(),
                    documentation: source.documentation.clone(),
                    module,
                    name: source.name.clone(),
                    public: source.public,
                    kind: source.kind,
                    parameters: source.parameters.clone(),
                    alternatives: Vec::new(),
                    compositions: source.compositions.clone(),
                    methods: source.methods.clone(),
                });
                for alternative in &source.alternatives {
                    let (span, member, name, payload) = match alternative {
                        ast::VariantAlternative::UnionMember { span, ty } => (
                            span.clone(),
                            Some(ty.clone()),
                            variant_member_name(ty),
                            None,
                        ),
                        ast::VariantAlternative::EnumCase {
                            span,
                            name,
                            payload,
                        } => (span.clone(), None, name.clone(), payload.clone()),
                    };
                    if source.kind == ast::VariantKind::Enum
                        && let Some(previous) = hir.variant_types[parent]
                            .alternatives
                            .iter()
                            .find(|existing| hir.variants[**existing].name == name)
                    {
                        return Err(FosterError::runtime(format!(
                            "{} `{}` declares `{name}` more than once",
                            if source.kind == ast::VariantKind::Enum {
                                "enum"
                            } else {
                                "union"
                            },
                            source.name
                        ))
                        .with_source_module(module_name.clone())
                        .with_primary_label(
                            span.clone(),
                            format!(
                                "this is another `{name}` {}",
                                if source.kind == ast::VariantKind::Enum {
                                    "case"
                                } else {
                                    "member"
                                }
                            ),
                        )
                        .with_label(
                            hir.variants[*previous].span.clone(),
                            format!("the first `{name}` is declared here"),
                        ));
                    }
                    let id = hir.variants.alloc(Variant {
                        span,
                        parent,
                        member,
                        name,
                        payload,
                    });
                    hir.variant_types[parent].alternatives.push(id);
                }
                hir.modules[module]
                    .variant_types
                    .insert(source.name.clone(), parent);
            }
            for source_record in &program.records {
                let mut parameters = std::collections::HashSet::new();
                for parameter in &source_record.parameters {
                    if !parameters.insert(parameter.as_str()) {
                        return Err(FosterError::runtime(format!(
                            "record `{}` declares type parameter `{parameter}` more than once",
                            source_record.name
                        )));
                    }
                }
                let mut fields = std::collections::HashSet::new();
                for field in &source_record.fields {
                    if !fields.insert(field.name.as_str()) {
                        return Err(FosterError::runtime(format!(
                            "record `{}` declares field `{}` more than once",
                            source_record.name, field.name
                        )));
                    }
                }
                for method in &source_record.methods {
                    if !fields.insert(method.name.as_str()) {
                        return Err(FosterError::runtime(format!(
                            "type `{}` declares member `{}` more than once",
                            source_record.name, method.name
                        )));
                    }
                }
                if hir.modules[module]
                    .functions
                    .contains_key(&source_record.name)
                    || hir.modules[module]
                        .constants
                        .contains_key(&source_record.name)
                    || hir.modules[module]
                        .records
                        .contains_key(&source_record.name)
                {
                    return Err(FosterError::runtime(format!(
                        "module `{module_name}` defines `{}` more than once",
                        source_record.name
                    )));
                }
                let record = hir.records.alloc(Record {
                    span: source_record.span.clone(),
                    documentation: source_record.documentation.clone(),
                    module,
                    name: source_record.name.clone(),
                    public: source_record.public,
                    intrinsic: source_record.intrinsic,
                    parameters: source_record.parameters.clone(),
                    compositions: source_record.compositions.clone(),
                    fields: source_record
                        .fields
                        .iter()
                        .map(|field| RecordField {
                            name: field.name.clone(),
                            public: field.public,
                            ty: field.ty.clone(),
                        })
                        .collect(),
                    methods: source_record.methods.clone(),
                });
                hir.modules[module]
                    .records
                    .insert(source_record.name.clone(), record);
            }
            for source_function in &program.functions {
                if hir.modules[module]
                    .records
                    .contains_key(&source_function.name)
                    || hir.modules[module]
                        .constants
                        .contains_key(&source_function.name)
                {
                    return Err(FosterError::runtime(format!(
                        "module `{module_name}` defines `{}` more than once",
                        source_function.name
                    )));
                }
                let function = hir.functions.alloc(Function {
                    span: source_function.span.clone(),
                    documentation: source_function.documentation.clone(),
                    module,
                    name: source_function.name.clone(),
                    test_description: None,
                    public: source_function.public,
                    intrinsic: source_function.intrinsic.clone(),
                    type_parameters: source_function.type_parameters.clone(),
                    groups: source_function.groups.clone(),
                    parameters: Vec::new(),
                    parameter_types: source_function
                        .parameters
                        .iter()
                        .map(|parameter| parameter.ty.clone())
                        .collect(),
                    parameter_type_spans: source_function
                        .parameters
                        .iter()
                        .map(|parameter| parameter.type_span.clone())
                        .collect(),
                    return_type: source_function.return_type.clone(),
                    effects_explicit: source_function.effects_explicit,
                    effects: source_function.effects.clone(),
                    effect_spans: source_function.effect_spans.clone(),
                    suspends: source_function.suspends,
                    suspend_span: source_function.suspend_span.clone(),
                    body: crate::block::Block::new(),
                });
                hir.modules[module]
                    .functions
                    .insert(source_function.name.clone(), function);
            }
            let mut descriptions = std::collections::HashSet::new();
            for (index, test) in program.tests.iter().enumerate() {
                if !descriptions.insert(test.description.as_str()) {
                    return Err(FosterError::runtime(format!(
                        "module `{module_name}` defines test `{}` more than once",
                        test.description
                    )));
                }
                let function = hir.functions.alloc(Function {
                    span: test.span.clone(),
                    documentation: None,
                    module,
                    name: format!("test {}", test.description),
                    test_description: Some(test.description.clone()),
                    public: false,
                    intrinsic: None,
                    type_parameters: Vec::new(),
                    groups: Vec::new(),
                    parameters: Vec::new(),
                    parameter_types: Vec::new(),
                    parameter_type_spans: Vec::new(),
                    return_type: Some(ast::TypeExpr::Unit),
                    effects_explicit: false,
                    effects: Vec::new(),
                    effect_spans: Vec::new(),
                    suspends: false,
                    suspend_span: None,
                    body: crate::block::Block::new(),
                });
                hir.tests.push(function);
                test_functions.insert((module, index), function);
            }
        }

        for (module_name, source_module) in &package.modules {
            let Some(program) = &source_module.program else {
                continue;
            };
            let module = hir.modules_by_name[module_name];
            let imports = resolve_imports(&hir, module, program)?;
            hir.modules[module].imports = imports
                .iter()
                .map(|(name, module)| (name.clone(), *module))
                .collect();
            hir.modules[module].imports_with_spans = program
                .imports
                .iter()
                .map(|import| {
                    let name = import.alias.clone().unwrap_or_else(|| {
                        import.path.last().expect("imports have a path").clone()
                    });
                    ImportBinding {
                        target: imports[&name],
                        name,
                        span: import.span.clone(),
                    }
                })
                .collect();
            for source in &program.constants {
                let constant = hir.modules[module].constants[&source.name];
                hir.constants[constant].value =
                    lower_constant_value(&hir, module, &imports, &source.value)?;
            }
            for source_function in &program.functions {
                let function = hir.modules[module].functions[&source_function.name];
                let mut lowerer = FunctionLowerer {
                    hir: &mut hir,
                    module,
                    function,
                    imports: &imports,
                    locals: HashMap::new(),
                    captures: Vec::new(),
                    self_name: None,
                    loop_depth: 0,
                };
                lowerer.lower_function(source_function)?;
            }
            for (index, test) in program.tests.iter().enumerate() {
                let function = test_functions[&(module, index)];
                let source = ast::Function {
                    span: test.span.clone(),
                    documentation: None,
                    name: hir.functions[function].name.clone(),
                    public: false,
                    intrinsic: None,
                    type_parameters: Vec::new(),
                    groups: Vec::new(),
                    parameters: Vec::new(),
                    return_type: Some(ast::TypeExpr::Unit),
                    effects_explicit: false,
                    effects: Vec::new(),
                    effect_spans: Vec::new(),
                    suspends: false,
                    suspend_span: None,
                    body: test.body.clone(),
                };
                let mut lowerer = FunctionLowerer {
                    hir: &mut hir,
                    module,
                    function,
                    imports: &imports,
                    locals: HashMap::new(),
                    captures: Vec::new(),
                    self_name: None,
                    loop_depth: 0,
                };
                lowerer.lower_function(&source)?;
            }
        }

        Ok(hir)
    }

    pub fn module_named(&self, name: &str) -> Option<ModuleId> {
        self.modules_by_name.get(name).copied()
    }

    pub fn function_named(&self, module: ModuleId, name: &str) -> Option<FunctionId> {
        self.modules[module].functions.get(name).copied()
    }

    pub fn constant_named(&self, module: ModuleId, name: &str) -> Option<ConstantId> {
        self.modules[module].constants.get(name).copied()
    }

    pub fn record_named(&self, module: ModuleId, name: &str) -> Option<RecordId> {
        self.modules[module].records.get(name).copied()
    }
    pub fn variant_type_named(&self, module: ModuleId, name: &str) -> Option<VariantTypeId> {
        self.modules[module].variant_types.get(name).copied()
    }
}

fn variant_member_name(member: &ast::TypeExpr) -> String {
    match member {
        ast::TypeExpr::Unit => "()".into(),
        ast::TypeExpr::Named(name, _) => name.rsplit('.').next().unwrap_or(name).to_owned(),
        ast::TypeExpr::Intersection(members) => members
            .iter()
            .map(variant_member_name)
            .collect::<Vec<_>>()
            .join(" & "),
        ast::TypeExpr::Reference { group, value } => {
            format!("ref[{group}] {}", variant_member_name(value))
        }
        ast::TypeExpr::Function { .. } => "func".into(),
    }
}

fn resolve_imports(
    hir: &PackageHir,
    current_module: ModuleId,
    program: &ast::Program,
) -> Result<HashMap<String, ModuleId>, FosterError> {
    let mut imports = HashMap::new();
    for import in &program.imports {
        let path = import.path.join(".");
        let module = hir.module_named(&path).ok_or_else(|| {
            FosterError::runtime(format!(
                "module `{}` imports unknown module `{path}`",
                hir.modules[current_module].name
            ))
        })?;
        let local_name = import
            .alias
            .clone()
            .unwrap_or_else(|| import.path.last().expect("import path is nonempty").clone());
        imports.insert(local_name, module);
    }
    Ok(imports)
}

fn lower_constant_value(
    hir: &PackageHir,
    module: ModuleId,
    imports: &HashMap<String, ModuleId>,
    expression: &ast::Expr,
) -> Result<ConstantValue, FosterError> {
    use ast::{Expr, UnaryOp};

    Ok(match expression.unspanned() {
        Expr::Unit => ConstantValue::Unit,
        Expr::Bool(value) => ConstantValue::Bool(*value),
        Expr::Integer(value) => ConstantValue::Integer(*value),
        Expr::Float(value) => ConstantValue::Float(*value),
        Expr::String(value) => ConstantValue::String(value.clone()),
        Expr::CodePoint(value) => ConstantValue::CodePoint(
            value
                .chars()
                .next()
                .expect("parsed CodePoint constants contain one scalar value"),
        ),
        Expr::Symbol(value) => ConstantValue::Symbol(value.clone()),
        Expr::List(values) => ConstantValue::List(
            values
                .iter()
                .map(|value| lower_constant_value(hir, module, imports, value))
                .collect::<Result<_, _>>()?,
        ),
        Expr::Unary {
            operator: UnaryOp::Negate,
            operand,
        } => match lower_constant_value(hir, module, imports, operand)? {
            ConstantValue::Integer(value) => ConstantValue::Integer(-value),
            ConstantValue::Float(value) => ConstantValue::Float(-value),
            _ => {
                return Err(FosterError::runtime(
                    "constant negation requires an Int or Float literal",
                ));
            }
        },
        Expr::Name(name) => {
            ConstantValue::Constant(resolve_constant_name(hir, module, imports, name)?)
        }
        Expr::Member { object, name } => {
            let Expr::Name(import) = object.unspanned() else {
                return Err(FosterError::runtime(
                    "constant initializer contains a non-constant member expression",
                ));
            };
            let imported = imports.get(import).ok_or_else(|| {
                FosterError::runtime(format!("unknown imported module `{import}`"))
            })?;
            let constant = hir.constant_named(*imported, name).ok_or_else(|| {
                FosterError::runtime(format!(
                    "module `{}` has no constant `{name}`",
                    hir.modules[*imported].name
                ))
            })?;
            if !hir.constants[constant].public {
                return Err(FosterError::runtime(format!(
                    "constant `{}.{name}` is private",
                    hir.modules[*imported].name
                )));
            }
            ConstantValue::Constant(constant)
        }
        _ => {
            return Err(FosterError::runtime(
                "constant initializer must contain only literals, constant names, and lists",
            ));
        }
    })
}

fn resolve_constant_name(
    hir: &PackageHir,
    module: ModuleId,
    imports: &HashMap<String, ModuleId>,
    name: &str,
) -> Result<ConstantId, FosterError> {
    if let Some(constant) = hir.constant_named(module, name) {
        return Ok(constant);
    }
    let imported = imports
        .values()
        .filter_map(|module| hir.constant_named(*module, name))
        .filter(|constant| hir.constants[*constant].public)
        .collect::<Vec<_>>();
    match imported.as_slice() {
        [constant] => Ok(*constant),
        [_, _, ..] => Err(FosterError::runtime(format!(
            "imported constant `{name}` is ambiguous; qualify it with its module"
        ))),
        [] => Err(FosterError::runtime(format!(
            "constant initializer references unknown constant `{name}`"
        ))),
    }
}

struct FunctionLowerer<'a> {
    hir: &'a mut PackageHir,
    module: ModuleId,
    function: FunctionId,
    imports: &'a HashMap<String, ModuleId>,
    locals: HashMap<String, LocalId>,
    captures: Vec<LocalId>,
    self_name: Option<String>,
    loop_depth: usize,
}

struct ClosureSource<'a> {
    name: &'a str,
    parameters: &'a [ast::Parameter],
    return_type: Option<ast::TypeExpr>,
    body: ast::ClosureBody,
    captures: &'a [ast::CaptureSpec],
    named: bool,
    effects: &'a [ast::Effect],
    suspends: bool,
}
