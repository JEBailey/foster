use super::*;

impl Table {
    /// Canonical container ordering does not depend on the caller's vector insertion order.
    pub fn canonical(&self) -> Self {
        let mut table = self.clone();
        table.modules.sort_by(|a, b| a.name.cmp(&b.name));
        for module in &mut table.modules {
            module.types.sort_by(|a, b| a.name.cmp(&b.name));
            module.definitions.sort_by(|a, b| a.symbol.cmp(&b.symbol));
            module.imports.sort_by(|a, b| a.symbol.cmp(&b.symbol));
        }
        table
    }

    /// Resolve one external request against exports. The returned ID belongs to this executable.
    pub fn resolve(
        &self,
        symbol: &Symbol,
        required: &Descriptor,
    ) -> Result<FunctionId, FosterError> {
        let mut matches = self
            .modules
            .iter()
            .flat_map(|m| &m.definitions)
            .filter(|d| &d.symbol == symbol);
        let definition = matches
            .next()
            .ok_or_else(|| error(format!("unresolved symbol {symbol:?}")))?;
        if matches.next().is_some() {
            return Err(error("ambiguous symbol"));
        }
        if !definition.public {
            return Err(error("symbol is not exported"));
        }
        if required.overload() != symbol.overload
            || required.receiver != symbol.receiver
            || !required.accepts(&definition.descriptor)
        {
            return Err(error(format!(
                "incompatible descriptor for {}",
                symbol.name.name
            )));
        }
        Ok(function_id(definition.function))
    }

    /// Validate all bindings without mutating instructions. Internal imports may refer to private
    /// helpers introduced by inlining; only `resolve` exposes public exports to an external caller.
    pub fn validate(&self, program: &Program) -> Result<(), FosterError> {
        if self.version != FORMAT_VERSION {
            return Err(error("unsupported descriptor version"));
        }
        let mut definitions = BTreeMap::new();
        let mut bindings = BTreeMap::new();
        let mut types = BTreeMap::new();
        let mut type_ids = BTreeSet::new();
        for module in &self.modules {
            for ty in &module.types {
                if ty.name.module != module.name
                    || types.insert(&ty.name, ty).is_some()
                    || !type_ids.insert((ty.variant, ty.id))
                {
                    return Err(error("duplicate or misplaced nominal type binding"));
                }
                if !ty.variant
                    && !program
                        .records
                        .contains_key(&la_arena::Idx::from_raw(la_arena::RawIdx::from_u32(ty.id)))
                {
                    return Err(error("missing nominal type implementation"));
                }
            }
        }
        let mut module_names = BTreeSet::new();
        for module in &self.modules {
            if module.name.package.is_empty()
                || module.name.path.is_empty()
                || !module_names.insert(&module.name)
            {
                return Err(error("empty or duplicate module identity"));
            }
            for definition in &module.definitions {
                if definition.symbol.name.module != module.name
                    || definition.symbol.overload != definition.descriptor.overload()
                    || definition.symbol.receiver != definition.descriptor.receiver
                {
                    return Err(error(
                        "symbol identity disagrees with its descriptor or module",
                    ));
                }
                if definitions.insert(&definition.symbol, definition).is_some()
                    || bindings.insert(definition.function, definition).is_some()
                {
                    return Err(error("duplicate symbol or function binding"));
                }
                let function = program
                    .functions
                    .get(&function_id(definition.function))
                    .ok_or_else(|| error("missing implementation"))?;
                if definition.descriptor.parameters.len() != usize::from(function.parameters)
                    || definition.generic_names.len() != definition.descriptor.generics as usize
                    || definition
                        .generic_names
                        .iter()
                        .collect::<BTreeSet<_>>()
                        .len()
                        != definition.generic_names.len()
                    || definition
                        .descriptor
                        .parameters
                        .iter()
                        .map(|p| p.mode)
                        .ne(function.parameter_modes.iter().copied().map(Mode::from))
                    || definition
                        .descriptor
                        .result_dependencies
                        .iter()
                        .any(|p| *p as usize >= definition.descriptor.parameters.len())
                {
                    return Err(error("descriptor disagrees with implementation parameters"));
                }
                let mut generics = BTreeMap::new();
                if !definition
                    .descriptor
                    .parameters
                    .iter()
                    .zip(&function.parameter_types)
                    .all(|(p, wire)| wire_matches(&p.ty, wire, &types, program, &mut generics))
                    || !wire_matches(
                        &definition.descriptor.result,
                        &function.result_type,
                        &types,
                        program,
                        &mut generics,
                    )
                {
                    return Err(error(format!(
                        "descriptor type disagrees with implementation of {}",
                        definition.symbol.name.name
                    )));
                }
                if generics.iter().any(|(index, name)| {
                    definition.generic_names.get(*index as usize) != Some(name)
                }) {
                    return Err(error(
                        "generic implementation names disagree with descriptor",
                    ));
                }
            }
        }
        for module in &self.modules {
            let mut imports = BTreeMap::new();
            for import in &module.imports {
                let definition = definitions
                    .get(&import.symbol)
                    .ok_or_else(|| error("unresolved module import"))?;
                if import.function != definition.function
                    || import.generic_names != definition.generic_names
                    || !import.required.accepts(&definition.descriptor)
                    || import.required.overload() != import.symbol.overload
                    || imports.insert(import.function, import).is_some()
                {
                    return Err(error("incompatible or duplicate module import"));
                }
            }
            for definition in &module.definitions {
                for instruction in
                    &program.functions[&function_id(definition.function)].instructions
                {
                    if let Some(target) = target(instruction)
                        && let Some(target) = bindings.get(&raw(target))
                        && target.symbol.name.module != module.name
                        && !imports.contains_key(&target.function)
                    {
                        return Err(error("cross-module call is missing its symbolic import"));
                    }
                }
            }
        }
        Ok(())
    }
}

/// Resolve symbolic module imports and patch their implementation bindings transactionally.
/// All definitions are already in this executable's ID space. This does not merge independently
/// allocated type arenas or load native libraries.
pub fn link(program: &mut Program) -> Result<(), FosterError> {
    // The normal compiler path has already assigned the final IDs. Avoid copying its code.
    if program.symbols.validate(program).is_ok() {
        return crate::vm::verify(program);
    }
    let mut linked = program.clone();
    let definitions = linked
        .symbols
        .modules
        .iter()
        .flat_map(|m| &m.definitions)
        .map(|d| (d.symbol.clone(), d.clone()))
        .collect::<BTreeMap<_, _>>();
    for module in &mut linked.symbols.modules {
        let mut relocations = BTreeMap::new();
        for import in &mut module.imports {
            let implementation = definitions
                .get(&import.symbol)
                .ok_or_else(|| error(format!("unresolved symbol {}", import.symbol.name.name)))?;
            if !import.required.accepts(&implementation.descriptor) {
                return Err(error(format!(
                    "incompatible descriptor for {}",
                    import.symbol.name.name
                )));
            }
            if import.generic_names.len() != implementation.generic_names.len()
                || import.generic_names.iter().collect::<BTreeSet<_>>().len()
                    != import.generic_names.len()
            {
                return Err(error("invalid generic import binding"));
            }
            let renames = import
                .generic_names
                .iter()
                .cloned()
                .zip(implementation.generic_names.iter().cloned())
                .collect::<BTreeMap<_, _>>();
            if relocations
                .insert(import.function, (implementation.function, renames))
                .is_some()
            {
                return Err(error("duplicate import binding"));
            }
            import.function = implementation.function;
            import
                .generic_names
                .clone_from(&implementation.generic_names);
        }
        for definition in &module.definitions {
            if relocations
                .get(&definition.function)
                .is_some_and(|(target, _)| *target != definition.function)
            {
                return Err(error("import binding collides with a local definition"));
            }
            let function = linked
                .functions
                .get_mut(&function_id(definition.function))
                .ok_or_else(|| error("missing implementation"))?;
            for instruction in &mut function.instructions {
                let Some(old_target) = target(instruction) else {
                    continue;
                };
                let Some((replacement, renames)) = relocations.get(&raw(old_target)) else {
                    continue;
                };
                match instruction {
                    Instruction::Call { specialization, .. }
                    | Instruction::CallMethod { specialization, .. }
                    | Instruction::MakeClosure { specialization, .. }
                    | Instruction::CallClosure { specialization, .. } => {
                        for (name, _) in specialization.iter_mut() {
                            if let Some(new) = renames.get(name) {
                                name.clone_from(new);
                            }
                        }
                        specialization.sort_by(|a, b| a.0.cmp(&b.0));
                    }
                    _ => {}
                }
                let target = match instruction {
                    Instruction::Call { function, .. }
                    | Instruction::CallMethod { function, .. }
                    | Instruction::RemoteCall { function, .. }
                    | Instruction::MakeClosure { function, .. }
                    | Instruction::CallClosure { function, .. } => function,
                    _ => continue,
                };
                *target = function_id(*replacement);
            }
        }
    }
    crate::vm::verify(&linked)?;
    *program = linked;
    Ok(())
}

fn wire_matches(
    ty: &SymbolType,
    wire: &crate::vm::VerificationType,
    types: &BTreeMap<&Name, &TypeBinding>,
    program: &Program,
    generics: &mut BTreeMap<u32, String>,
) -> bool {
    use crate::vm::VerificationType as V;
    if *wire == V::Unknown {
        return true;
    } // Structural conformance remains a front-end proof.
    match (ty, wire) {
        (SymbolType::Generic(index), V::Generic(name)) => {
            generics.entry(*index).or_insert_with(|| name.clone()) == name
        }
        (SymbolType::Primitive(name), wire) => matches!(
            (name.as_str(), wire),
            ("unit", V::Unit)
                | ("bool", V::Bool)
                | ("int", V::Integer)
                | ("float", V::Float)
                | ("byte", V::Byte)
                | ("code_point", V::CodePoint)
                | ("raw_bytes", V::Bytes)
                | ("raw_byte_buffer", V::ByteBuffer)
        ),
        (SymbolType::Reference(_, value), V::Reference(actual)) => {
            wire_matches(value, actual, types, program, generics)
        }
        (SymbolType::Applied(kind, args), V::List(actual))
            if kind == "raw_list" && args.len() == 1 =>
        {
            wire_matches(&args[0], actual, types, program, generics)
        }
        (SymbolType::Applied(kind, args), V::Remote(actual))
            if kind == "remote" && args.len() == 1 =>
        {
            wire_matches(&args[0], actual, types, program, generics)
        }
        (SymbolType::Applied(kind, args), V::Future(actual))
            if kind == "future" && args.len() == 1 =>
        {
            wire_matches(&args[0], actual, types, program, generics)
        }
        (SymbolType::Nominal(name, args), actual) => {
            let Some(binding) = types.get(name) else {
                return false;
            };
            let (variant, id, actual_args) = match actual {
                V::Record { record, arguments } => (false, record.into_raw().into_u32(), arguments),
                V::Variant { variant, arguments } => {
                    (true, variant.into_raw().into_u32(), arguments)
                }
                V::List(element) => {
                    return !binding.variant
                        && program
                            .list_record
                            .is_some_and(|id| id.into_raw().into_u32() == binding.id)
                        && args.len() == 1
                        && wire_matches(&args[0], element, types, program, generics);
                }
                V::Bytes => {
                    return !binding.variant
                        && program
                            .bytes_record
                            .is_some_and(|id| id.into_raw().into_u32() == binding.id)
                        && args.is_empty();
                }
                _ => return false,
            };
            binding.variant == variant
                && binding.id == id
                && args.len() == actual_args.len()
                && args
                    .iter()
                    .zip(actual_args)
                    .all(|(a, b)| wire_matches(a, b, types, program, generics))
        }
        (
            SymbolType::Function(signature),
            V::Function {
                parameters,
                parameter_modes,
                result,
            },
        ) => {
            signature.parameters.len() == parameters.len()
                && signature
                    .parameters
                    .iter()
                    .map(|p| p.mode)
                    .eq(parameter_modes.iter().copied().map(Mode::from))
                && signature
                    .parameters
                    .iter()
                    .zip(parameters)
                    .all(|(a, b)| wire_matches(&a.ty, b, types, program, generics))
                && wire_matches(&signature.result, result, types, program, generics)
        }
        _ => false,
    }
}

pub(super) fn target(instruction: &Instruction) -> Option<FunctionId> {
    match instruction {
        Instruction::Call { function, .. }
        | Instruction::CallMethod { function, .. }
        | Instruction::RemoteCall { function, .. }
        | Instruction::MakeClosure { function, .. }
        | Instruction::CallClosure { function, .. } => Some(*function),
        _ => None,
    }
}
