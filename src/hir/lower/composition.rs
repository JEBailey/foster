//! Materialize ordered defaults before inference, preserving their lexical scope.
use super::*;
use crate::types::NominalTypeId;
use std::collections::HashSet;

#[derive(Clone)]
struct Candidate {
    module: ModuleId,
    source: ast::Function,
    original: FunctionId,
    substitutions: HashMap<String, ast::TypeExpr>,
}

struct Owner {
    nominal: NominalTypeId,
    module: ModuleId,
    name: String,
    public: bool,
    parameters: Vec<String>,
    compositions: Vec<ast::TypeExpr>,
}

pub(super) fn materialize(
    hir: &mut PackageHir,
    package: &Package,
    source_functions: &HashMap<(ModuleId, usize), FunctionId>,
) -> Result<(), FosterError> {
    let owners = hir
        .records
        .iter()
        .map(|(id, record)| Owner {
            nominal: NominalTypeId::Record(id),
            module: record.module,
            name: record.name.clone(),
            public: record.public,
            parameters: record.parameters.clone(),
            compositions: record.compositions.clone(),
        })
        .chain(hir.variant_types.iter().map(|(id, variant)| Owner {
            nominal: NominalTypeId::Variant(id),
            module: variant.module,
            name: variant.name.clone(),
            public: variant.public,
            parameters: variant.parameters.clone(),
            compositions: variant.compositions.clone(),
        }))
        .collect::<Vec<_>>();
    let mut pending = Vec::new();
    for record in owners {
        // Compiled modules already contain their materialized implementations.
        let source = &package.modules[&hir.modules[record.module].name];
        if source.origin == crate::package::ModuleOrigin::Dependency && source.source_path.is_none()
        {
            continue;
        }
        if record.compositions.is_empty() {
            continue;
        }
        let arguments = record
            .parameters
            .iter()
            .map(|name| ast::TypeExpr::Named(name.clone(), vec![]))
            .collect::<Vec<_>>();
        let mut candidates = Vec::new();
        for composition in &record.compositions {
            collect_type(
                hir,
                package,
                source_functions,
                record.module,
                composition,
                &mut HashSet::new(),
                &mut candidates,
            )?;
        }
        if let Some(program) = &package.modules[&hir.modules[record.module].name].program {
            for (index, source) in program.functions.iter().enumerate() {
                if source.owner.as_deref() == Some(record.name.as_str()) && source.receiver {
                    candidates.push(Candidate {
                        module: record.module,
                        source: source.clone(),
                        original: source_functions[&(record.module, index)],
                        substitutions: HashMap::new(),
                    });
                }
            }
        }
        let mut selected = BTreeMap::<(String, String), FunctionId>::new();
        for mut candidate in candidates {
            let member = candidate.source.name.rsplit('.').next().unwrap().to_owned();
            let mut generic_names = record.parameters.clone();
            for name in &candidate.source.type_parameters {
                if !candidate.substitutions.contains_key(name) && !generic_names.contains(name) {
                    generic_names.push(name.clone());
                }
            }
            let canonical = generic_names
                .iter()
                .enumerate()
                .map(|(index, name)| {
                    (
                        name.clone(),
                        ast::TypeExpr::Named(format!("$generic{index}"), vec![]),
                    )
                })
                .collect();
            let parameters = candidate
                .source
                .parameters
                .iter()
                .skip(1)
                .map(|parameter| {
                    parameter.ty.as_ref().map(|ty| {
                        let ty = substitute(&substitute(ty, &candidate.substitutions), &canonical);
                        qualify(hir, candidate.module, &ty)
                    })
                })
                .collect::<Vec<_>>();
            let key = (member.clone(), format!("{parameters:?}"));
            let own = hir.functions[candidate.original].module == record.module
                && candidate.source.owner.as_deref() == Some(record.name.as_str());
            // A compiled method has no source body to adapt to a new receiver layout.
            // Client compositions must supply their implementations explicitly.
            if !own && hir.external_functions.contains_key(&candidate.original) {
                continue;
            }
            let function = if own {
                candidate.original
            } else {
                // Keep donor methods in their original module: names, imports, and private
                // helpers in a default body must not resolve in the recipient's scope.
                let alias = type_identity(hir, record.nominal);
                let mut definition = hir.functions[candidate.original].clone();
                definition.public &= record.public;
                definition.name =
                    format!("{}.{member}$default{}", record.name, hir.functions.len());
                definition.owner = Some(record.name.clone());
                definition.body = crate::block::Block::new();
                definition.parameters.clear();
                definition.receiver = None;
                definition.type_parameters = generic_names;
                for annotation in definition.parameter_types.iter_mut().flatten() {
                    *annotation = substitute(annotation, &candidate.substitutions);
                }
                definition.parameter_types[0] =
                    Some(ast::TypeExpr::Named(alias, arguments.clone()));
                if let Some(result) = &mut definition.return_type {
                    *result = substitute(result, &candidate.substitutions);
                }
                for group in &mut definition.groups {
                    group.element = substitute(&group.element, &candidate.substitutions);
                }
                candidate.source.type_parameters = definition.type_parameters.clone();
                let function = hir.functions.alloc(definition);
                hir.composition_owners.insert(function, record.module);
                hir.composition_dispatch.insert(candidate.original);
                hir.composition_dispatch.insert(function);
                pending.push((function, candidate));
                function
            };
            if let Some(previous) = selected.insert(key, function) {
                hir.composition_checks.push((previous, function));
            }
        }
        for ((member, _), function) in selected {
            if hir.functions[function].name.contains("$default") {
                let name = format!("{}.{member}", record.name);
                hir.functions[function].name = name.clone();
                hir.modules[record.module]
                    .functions
                    .entry(name)
                    .or_default()
                    .push(function);
            }
        }
    }
    // All winners are registered before any inherited body is lowered.
    for (function, candidate) in pending {
        let imports = hir.modules[candidate.module]
            .imports
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        let first_closure = hir.functions.len();
        FunctionLowerer {
            hir,
            module: candidate.module,
            function,
            imports: &imports,
            locals: HashMap::new(),
            captures: Vec::new(),
            self_name: None,
            loop_depth: 0,
        }
        .lower_function(&candidate.source)?;
        // Nested closures retain the donor's lexical scope and specialize owner parameters.
        let owner_module = hir.composition_owners[&function];
        for (id, closure) in hir.functions.iter_mut().skip(first_closure) {
            hir.composition_owners.insert(id, owner_module);
            for annotation in closure.parameter_types.iter_mut().flatten() {
                *annotation = substitute(annotation, &candidate.substitutions);
            }
            if let Some(result) = &mut closure.return_type {
                *result = substitute(result, &candidate.substitutions);
            }
            for group in &mut closure.groups {
                group.element = substitute(&group.element, &candidate.substitutions);
            }
        }
    }
    Ok(())
}

fn collect(
    hir: &mut PackageHir,
    package: &Package,
    sources: &HashMap<(ModuleId, usize), FunctionId>,
    record: RecordId,
    arguments: &[ast::TypeExpr],
    visiting: &mut HashSet<RecordId>,
) -> Result<Vec<Candidate>, FosterError> {
    if !visiting.insert(record) {
        return Err(FosterError::runtime(format!(
            "type `{}` has a cyclic composed contract",
            hir.records[record].name
        )));
    }
    let definition = hir.records[record].clone();
    let substitutions = definition
        .parameters
        .iter()
        .cloned()
        .zip(arguments.iter().cloned())
        .collect::<HashMap<_, _>>();
    let mut candidates = Vec::new();
    for composition in &definition.compositions {
        collect_type(
            hir,
            package,
            sources,
            definition.module,
            &substitute(composition, &substitutions),
            visiting,
            &mut candidates,
        )?;
    }
    // A concrete implementation's private representation is not part of its
    // composable contract. Its methods remain implementations of that concrete
    // type, rather than default bodies transplanted onto a storage-free view.
    if !definition.fields.iter().any(|field| !field.public)
        && let Some(program) = &package.modules[&hir.modules[definition.module].name].program
    {
        for (index, source) in program.functions.iter().enumerate() {
            if source.owner.as_deref() != Some(definition.name.as_str())
                || !source.receiver
                || !source.public
                || source.intrinsic.is_some()
            {
                continue;
            }
            let mut method_substitutions = substitutions.clone();
            if let Some(ast::TypeExpr::Named(_, parameters)) =
                source.parameters.first().and_then(|p| p.ty.as_ref())
            {
                for (parameter, argument) in parameters.iter().zip(arguments) {
                    if let ast::TypeExpr::Named(name, nested) = parameter
                        && nested.is_empty()
                        && source.type_parameters.contains(name)
                    {
                        method_substitutions.insert(name.clone(), argument.clone());
                    }
                }
            }
            candidates.push(Candidate {
                module: definition.module,
                source: source.clone(),
                original: sources[&(definition.module, index)],
                substitutions: method_substitutions,
            });
        }
    }
    visiting.remove(&record);
    Ok(candidates)
}

fn collect_type(
    hir: &mut PackageHir,
    package: &Package,
    sources: &HashMap<(ModuleId, usize), FunctionId>,
    module: ModuleId,
    ty: &ast::TypeExpr,
    visiting: &mut HashSet<RecordId>,
    result: &mut Vec<Candidate>,
) -> Result<(), FosterError> {
    match ty {
        ast::TypeExpr::Intersection(members) => {
            for member in members {
                collect_type(hir, package, sources, module, member, visiting, result)?;
            }
        }
        ast::TypeExpr::Named(name, arguments) => {
            if let Some(NominalTypeId::Record(record)) = resolve(hir, module, name) {
                let arguments = arguments
                    .iter()
                    .map(|argument| qualify(hir, module, argument))
                    .collect::<Vec<_>>();
                result.extend(collect(
                    hir, package, sources, record, &arguments, visiting,
                )?);
            }
        }
        _ => {}
    }
    Ok(())
}

fn resolve(hir: &PackageHir, module: ModuleId, name: &str) -> Option<NominalTypeId> {
    if let Some(found) = hir.composition_types.get(name) {
        return Some(*found);
    }
    let lookup = |module, name: &str| {
        hir.record_named(module, name)
            .map(NominalTypeId::Record)
            .or_else(|| {
                hir.variant_type_named(module, name)
                    .map(NominalTypeId::Variant)
            })
    };
    if let Some((qualifier, name)) = name.rsplit_once('.') {
        let module = hir.modules[module]
            .imports
            .get(qualifier)
            .copied()
            .or_else(|| hir.module_named(qualifier))?;
        return lookup(module, name);
    }
    if let Some(found) = lookup(module, name) {
        return Some(found);
    }
    let mut found = hir.modules[module]
        .imports
        .values()
        .filter_map(|module| lookup(*module, name))
        .collect::<Vec<_>>();
    found.sort();
    found.dedup();
    (found.len() == 1).then(|| found[0])
}

fn type_identity(hir: &mut PackageHir, nominal: NominalTypeId) -> String {
    let name = format!("$composition{nominal:?}");
    hir.composition_types.insert(name.clone(), nominal);
    name
}

fn qualify(hir: &mut PackageHir, module: ModuleId, ty: &ast::TypeExpr) -> ast::TypeExpr {
    match ty {
        ast::TypeExpr::Named(name, arguments) => {
            let name = resolve(hir, module, name)
                .map(|nominal| type_identity(hir, nominal))
                .unwrap_or_else(|| name.clone());
            ast::TypeExpr::Named(
                name,
                arguments
                    .iter()
                    .map(|argument| qualify(hir, module, argument))
                    .collect(),
            )
        }
        ast::TypeExpr::Intersection(members) => {
            ast::TypeExpr::Intersection(members.iter().map(|ty| qualify(hir, module, ty)).collect())
        }
        ast::TypeExpr::Reference { group, value } => ast::TypeExpr::Reference {
            group: group.clone(),
            value: Box::new(qualify(hir, module, value)),
        },
        ast::TypeExpr::Function {
            parameters,
            parameter_modes,
            result,
            effects,
            suspends,
        } => ast::TypeExpr::Function {
            parameters: parameters
                .iter()
                .map(|ty| qualify(hir, module, ty))
                .collect(),
            parameter_modes: parameter_modes.clone(),
            result: Box::new(qualify(hir, module, result)),
            effects: effects.clone(),
            suspends: *suspends,
        },
        ast::TypeExpr::Unit => ast::TypeExpr::Unit,
    }
}

fn substitute(ty: &ast::TypeExpr, substitutions: &HashMap<String, ast::TypeExpr>) -> ast::TypeExpr {
    use ast::TypeExpr;
    match ty {
        TypeExpr::Named(name, arguments) => {
            if arguments.is_empty()
                && let Some(value) = substitutions.get(name)
            {
                return value.clone();
            }
            TypeExpr::Named(
                name.clone(),
                arguments
                    .iter()
                    .map(|ty| substitute(ty, substitutions))
                    .collect(),
            )
        }
        TypeExpr::Intersection(members) => TypeExpr::Intersection(
            members
                .iter()
                .map(|ty| substitute(ty, substitutions))
                .collect(),
        ),
        TypeExpr::Reference { group, value } => TypeExpr::Reference {
            group: group.clone(),
            value: Box::new(substitute(value, substitutions)),
        },
        TypeExpr::Function {
            parameters,
            parameter_modes,
            result,
            effects,
            suspends,
        } => TypeExpr::Function {
            parameters: parameters
                .iter()
                .map(|ty| substitute(ty, substitutions))
                .collect(),
            parameter_modes: parameter_modes.clone(),
            result: Box::new(substitute(result, substitutions)),
            effects: effects.clone(),
            suspends: *suspends,
        },
        TypeExpr::Unit => TypeExpr::Unit,
    }
}
