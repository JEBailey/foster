use super::*;
use crate::types::{DispatchSlot, NominalTypeId};
use vm::{Instruction as I, VerificationType as V};

fn id<T>(raw: u32) -> la_arena::Idx<T> {
    la_arena::Idx::from_raw(la_arena::RawIdx::from_u32(raw))
}
fn raw<T>(id: la_arena::Idx<T>) -> u32 {
    id.into_raw().into_u32()
}

fn method_key(definition: &symbols::Definition) -> Option<MethodKey> {
    if !definition.descriptor.receiver {
        return None;
    }
    fn normalize(ty: &mut symbols::SymbolType, generics: &mut BTreeMap<u32, u32>) {
        use symbols::SymbolType as T;
        match ty {
            T::Generic(index) => {
                let next = generics.len() as u32;
                *index = *generics.entry(*index).or_insert(next);
            }
            T::Nominal(_, args) | T::Applied(_, args) | T::Intersection(args) => {
                for arg in args {
                    normalize(arg, generics);
                }
            }
            T::Reference(_, value) => normalize(value, generics),
            T::Function(signature) => {
                for p in &mut signature.parameters {
                    normalize(&mut p.ty, generics);
                }
                normalize(&mut signature.result, generics);
            }
            _ => {}
        }
    }
    let mut parameters = definition
        .descriptor
        .overload()
        .into_iter()
        .skip(1)
        .collect::<Vec<_>>();
    let mut generics = BTreeMap::new();
    for p in &mut parameters {
        normalize(&mut p.ty, &mut generics);
    }
    Some(MethodKey {
        name: definition.symbol.name.name.rsplit('.').next()?.to_owned(),
        parameters,
    })
}

pub(crate) fn link(
    compilation: &Compilation,
    program: &mut vm::Program,
) -> Result<(), FosterError> {
    if compilation.package.libraries.is_empty() {
        return Ok(());
    }
    program.symbols = symbols::Table::from_compilation(compilation, program)?;
    let functions = program
        .symbols
        .modules
        .iter()
        .flat_map(|m| &m.definitions)
        .map(|d| (d.symbol.clone(), d.clone()))
        .collect::<BTreeMap<_, _>>();
    let types = program
        .symbols
        .modules
        .iter()
        .flat_map(|m| &m.types)
        .map(|t| (t.name.clone(), (t.variant, t.id)))
        .collect::<BTreeMap<_, _>>();
    let slot_keys = super::dispatch::slots(compilation, program)?
        .into_iter()
        .map(|s| (s.key, s.id))
        .collect::<BTreeMap<_, _>>();
    for (library_index, library) in compilation.package.libraries.iter().enumerate() {
        let mut mapping = Mapping {
            functions: BTreeMap::new(),
            records: BTreeMap::new(),
            variants: BTreeMap::new(),
            cases: BTreeMap::new(),
            slots: BTreeMap::new(),
            constants: Vec::new(),
            generic_names: BTreeMap::new(),
        };
        for module in &library.code.symbols.modules {
            for definition in &module.definitions {
                let target = functions.get(&definition.symbol).ok_or_else(|| {
                    error(format!(
                        "unresolved library symbol {} in {:?}",
                        definition.symbol.name.name, definition.symbol.name.module
                    ))
                })?;
                if !definition.descriptor.accepts(&target.descriptor) {
                    return Err(error(format!(
                        "incompatible descriptor for {}",
                        definition.symbol.name.name
                    )));
                }
                mapping.generic_names.insert(
                    definition.function,
                    definition
                        .generic_names
                        .iter()
                        .cloned()
                        .zip(target.generic_names.iter().cloned())
                        .collect(),
                );
                mapping
                    .functions
                    .insert(definition.function, target.function);
            }
            for ty in &module.types {
                let (variant, target) = types
                    .get(&ty.name)
                    .ok_or_else(|| error(format!("unresolved library type {:?}", ty.name)))?;
                if *variant != ty.variant {
                    return Err(error("nominal kind mismatch"));
                }
                if *variant {
                    mapping.variants.insert(ty.id, *target);
                } else {
                    mapping.records.insert(ty.id, *target);
                }
            }
        }
        for (case, definition) in &library.code.variants {
            let parent = mapping.variants[&raw(definition.parent)];
            let target = program
                .variants
                .iter()
                .find(|(_, v)| raw(v.parent) == parent && v.alternative == definition.alternative)
                .map(|(id, _)| raw(*id))
                .ok_or_else(|| error("missing enum case"))?;
            mapping.cases.insert(raw(*case), target);
        }
        for slot in &library.interface.slots {
            let target = *slot_keys
                .get(&slot.key)
                .ok_or_else(|| error("unresolved dispatch signature"))?;
            if target >= u32::MAX - 2 {
                return Err(error("dispatch slot limit exceeded"));
            }
            mapping.slots.insert(slot.id, target);
        }
        for reserved in [u32::MAX, u32::MAX - 1, u32::MAX - 2] {
            mapping.slots.insert(reserved, reserved);
        }
        for constant in &library.code.constants {
            let index = program
                .constants
                .iter()
                .position(|c| c == constant)
                .unwrap_or_else(|| {
                    program.constants.push(constant.clone());
                    program.constants.len() - 1
                });
            mapping.constants.push(
                u16::try_from(index).map_err(|_| error("linked constant pool exceeds limit"))?,
            );
        }
        for (source, record) in &library.code.records {
            let mut remapped = record.clone();
            for ty in &mut remapped.field_types {
                mapping.ty(ty);
            }
            if program.records.get(&id(mapping.records[&raw(*source)])) != Some(&remapped) {
                return Err(error(format!("incompatible layout for {}", record.name)));
            }
        }
        for (source, variant) in &library.code.variants {
            let mut remapped = variant.clone();
            remapped.parent = id(mapping.variants[&raw(variant.parent)]);
            for ty in &mut remapped.payload {
                mapping.ty(ty);
            }
            if program.variants.get(&id(mapping.cases[&raw(*source)])) != Some(&remapped) {
                return Err(error(format!(
                    "incompatible layout for {}",
                    variant.type_name
                )));
            }
        }
        for (source, function) in &library.code.functions {
            let target = id(mapping.functions[&raw(*source)]);
            // Embedded runtime/library functions are supplied by the current compiler. Only
            // declarations mounted from this artifact receive its compiled implementation.
            if !compilation
                .hir
                .external_functions
                .get(&target)
                .is_some_and(|e| e.library == library_index)
            {
                continue;
            }
            let mut function = function.clone();
            for ty in function
                .parameter_types
                .iter_mut()
                .chain(&mut function.capture_types)
                .chain(std::iter::once(&mut function.result_type))
            {
                mapping.ty(ty);
            }
            for instruction in &mut function.instructions {
                mapping.instruction(instruction)?;
            }
            program.functions.insert(target, function);
        }
        for ((nominal, slot), function) in &library.code.dispatch {
            let nominal = match nominal {
                NominalTypeId::Record(r) => NominalTypeId::Record(id(mapping.records[&raw(*r)])),
                NominalTypeId::Variant(v) => NominalTypeId::Variant(id(mapping.variants[&raw(*v)])),
            };
            program.dispatch.insert(
                (nominal, DispatchSlot(mapping.slots[&slot.0])),
                id(mapping.functions[&raw(*function)]),
            );
        }
    }
    // Imported structural calls can encounter client-defined types. Populate their matching
    // implementations using the same canonical parameter/name key as the imported slot.
    let nominal_types = program
        .symbols
        .modules
        .iter()
        .flat_map(|m| &m.types)
        .map(|t| (&t.name, t))
        .collect::<BTreeMap<_, _>>();
    let mut candidates = program
        .symbols
        .modules
        .iter()
        .flat_map(|m| &m.definitions)
        .collect::<Vec<_>>();
    candidates.sort_by_key(|d| {
        (
            method_key(d).map_or(usize::MAX, |k| super::dispatch::generic_count(&k)),
            d.function,
        )
    });
    for definition in candidates {
        let Some(key) = method_key(definition) else {
            continue;
        };

        let Some(symbols::Parameter {
            ty: symbols::SymbolType::Nominal(name, _),
            ..
        }) = definition.descriptor.parameters.first()
        else {
            continue;
        };
        let Some(ty) = nominal_types.get(name) else {
            continue;
        };
        let nominal = if ty.variant {
            NominalTypeId::Variant(id(ty.id))
        } else {
            NominalTypeId::Record(id(ty.id))
        };
        for (required, slot) in &slot_keys {
            if super::dispatch::matches(&key, required) {
                program
                    .dispatch
                    .entry((nominal, DispatchSlot(*slot)))
                    .or_insert(id(definition.function));
            }
        }
    }
    program.symbols = symbols::Table::from_compilation(compilation, program)?;
    Ok(())
}

struct Mapping {
    generic_names: BTreeMap<u32, BTreeMap<String, String>>,
    functions: BTreeMap<u32, u32>,
    records: BTreeMap<u32, u32>,
    variants: BTreeMap<u32, u32>,
    cases: BTreeMap<u32, u32>,
    slots: BTreeMap<u32, u32>,
    constants: Vec<u16>,
}
impl Mapping {
    fn ty(&self, ty: &mut V) {
        match ty {
            V::Record { record, arguments } => {
                *record = id(self.records[&raw(*record)]);
                for t in arguments {
                    self.ty(t);
                }
            }
            V::Variant { variant, arguments } => {
                *variant = id(self.variants[&raw(*variant)]);
                for t in arguments {
                    self.ty(t);
                }
            }
            V::Reference(t) | V::List(t) | V::Remote(t) | V::Future(t) => self.ty(t),
            V::Function {
                parameters, result, ..
            } => {
                for t in parameters {
                    self.ty(t);
                }
                self.ty(result);
            }
            V::Union(types) => {
                for t in types {
                    self.ty(t);
                }
            }
            _ => {}
        }
    }
    fn pattern(&self, pattern: &mut hir::Pattern) {
        match pattern {
            hir::Pattern::Variant { variant, fields } => {
                *variant = id(self.cases[&raw(*variant)]);
                for p in fields {
                    self.pattern(p);
                }
            }
            hir::Pattern::Spanned { pattern, .. } => self.pattern(pattern),
            _ => {}
        }
    }
    fn instruction(&self, instruction: &mut I) -> Result<(), FosterError> {
        match instruction {
            I::LoadConstant { constant, .. } => *constant = self.constants[*constant as usize],
            I::MakeRecord {
                record,
                type_arguments,
                ..
            } => {
                *record = id(self.records[&raw(*record)]);
                for t in type_arguments {
                    self.ty(t);
                }
            }
            I::MakeVariant {
                variant,
                type_arguments,
                ..
            } => {
                *variant = id(self.cases[&raw(*variant)]);
                for t in type_arguments {
                    self.ty(t);
                }
            }
            I::MakeList { element_type, .. } => self.ty(element_type),
            I::MakeReference { pointee_type, .. }
            | I::MakeWholeReference { pointee_type, .. }
            | I::MakeFieldReference { pointee_type, .. } => self.ty(pointee_type),
            I::MatchPattern { pattern, .. } => self.pattern(pattern),
            I::Call {
                function,
                specialization,
                ..
            }
            | I::CallMethod {
                function,
                specialization,
                ..
            }
            | I::MakeClosure {
                function,
                specialization,
                ..
            }
            | I::CallClosure {
                function,
                specialization,
                ..
            } => {
                let names = &self.generic_names[&raw(*function)];
                *function = id(self.functions[&raw(*function)]);
                for (name, t) in specialization {
                    if let Some(target) = names.get(name) {
                        *name = target.clone();
                    }
                    self.ty(t);
                }
            }
            I::RemoteCall { function, .. } => *function = id(self.functions[&raw(*function)]),
            I::CallContractMethod {
                slot, result_type, ..
            } => {
                *slot = DispatchSlot(
                    *self
                        .slots
                        .get(&slot.0)
                        .ok_or_else(|| error("unresolved contract slot"))?,
                );
                self.ty(result_type);
            }
            _ => {}
        }
        Ok(())
    }
}
