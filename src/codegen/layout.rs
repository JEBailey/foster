//! Target-independent aggregate layout legalization.
//!
//! The executable IR names logical fields, alternatives, captures, and places.  This module
//! turns those names into deterministic slots before a backend sees them.  VM values remain
//! boxed Rust values, while native backends can use the same descriptions to choose an ABI.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::error::FosterError;
use crate::hir::{FunctionId, RecordId, VariantId, VariantTypeId};
use crate::vm::{Instruction, Program, VerificationType};

pub mod physical;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LayoutId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Ownership {
    Owned,
    Borrowed,
    Shared,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    pub index: u32,
    pub name: String,
    pub ty: VerificationType,
    pub ownership: Ownership,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alternative {
    pub variant: VariantId,
    pub tag: u32,
    pub name: String,
    pub payload: Vec<VerificationType>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutKind {
    Record {
        record: RecordId,
        name: String,
        arguments: Vec<VerificationType>,
        fields: Vec<Slot>,
    },
    Variant {
        variant_type: VariantTypeId,
        name: String,
        arguments: Vec<VerificationType>,
        alternatives: Vec<Alternative>,
    },
    Closure {
        function: FunctionId,
        specialization: crate::vm::Specialization,
        captures: Vec<Slot>,
    },
    /// A place handle is two scalar components: an owning slot pointer and a projection path.
    Pointer {
        pointee: VerificationType,
        ownership: Ownership,
    },
    /// A runtime-backed structural value such as a list, byte buffer, future, or callable.
    Builtin { ty: VerificationType },
    /// Box used when an explicitly dynamic value erases its concrete representation.
    Opaque,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub id: LayoutId,
    /// Aggregate values cross both current backend boundaries as one pointer-sized scalar.
    pub boxed: bool,
    /// False for a generic schema that must be instantiated before code generation.
    pub materialized: bool,
    pub kind: LayoutKind,
}

/// The only values a backend receives after representation legalization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegalType {
    /// An explicitly dynamic or heterogeneous value uses the erased runtime representation.
    Opaque,
    /// A generic schema has no executable representation until monomorphized.
    UnresolvedGeneric,
    I8,
    I32,
    I64,
    F64,
    Pointer {
        layout: Option<LayoutId>,
        ownership: Ownership,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Registry {
    layouts: Vec<Layout>,
    records: HashMap<RecordId, LayoutId>,
    variants: HashMap<VariantTypeId, LayoutId>,
    record_parameters: HashMap<RecordId, Vec<String>>,
    variant_parameters: HashMap<VariantTypeId, Vec<String>>,
    record_instances: BTreeMap<(RecordId, Vec<VerificationType>), LayoutId>,
    variant_instances: BTreeMap<(VariantTypeId, Vec<VerificationType>), LayoutId>,
    closures: HashMap<FunctionId, LayoutId>,
    closure_instances: BTreeMap<(FunctionId, crate::vm::Specialization), LayoutId>,
    pointers: BTreeMap<(VerificationType, Ownership), LayoutId>,
    builtins: BTreeMap<VerificationType, LayoutId>,
    opaque: Option<LayoutId>,
}

impl Registry {
    pub fn layouts(&self) -> &[Layout] {
        &self.layouts
    }

    pub fn get(&self, id: LayoutId) -> &Layout {
        &self.layouts[id.0 as usize]
    }

    pub fn record(&self, id: RecordId) -> Option<LayoutId> {
        self.records.get(&id).copied()
    }

    pub fn record_instance(
        &self,
        id: RecordId,
        arguments: &[VerificationType],
    ) -> Option<LayoutId> {
        self.record_instances
            .get(&(id, arguments.to_vec()))
            .copied()
            .or_else(|| arguments.is_empty().then(|| self.record(id)).flatten())
    }

    pub fn variant(&self, id: VariantTypeId) -> Option<LayoutId> {
        self.variants.get(&id).copied()
    }

    pub fn variant_instance(
        &self,
        id: VariantTypeId,
        arguments: &[VerificationType],
    ) -> Option<LayoutId> {
        self.variant_instances
            .get(&(id, arguments.to_vec()))
            .copied()
            .or_else(|| arguments.is_empty().then(|| self.variant(id)).flatten())
    }

    /// Materialize concrete nominal layouts reachable by one native specialization.
    pub fn instantiate_type(&mut self, ty: &VerificationType) -> Result<(), FosterError> {
        match ty {
            VerificationType::Record { record, arguments } => {
                for argument in arguments {
                    self.instantiate_type(argument)?;
                }
                self.instantiate_record(*record, arguments)?;
            }
            VerificationType::Variant { variant, arguments } => {
                for argument in arguments {
                    self.instantiate_type(argument)?;
                }
                self.instantiate_variant(*variant, arguments)?;
            }
            VerificationType::Reference(value) => {
                self.instantiate_type(value)?;
                let key = ((**value).clone(), Ownership::Borrowed);
                if !self.pointers.contains_key(&key) {
                    let layout = self.push_runtime_kind(LayoutKind::Pointer {
                        pointee: (**value).clone(),
                        ownership: Ownership::Borrowed,
                    });
                    self.pointers.insert(key, layout);
                }
            }
            VerificationType::List(value)
            | VerificationType::Remote(value)
            | VerificationType::Future(value) => {
                self.instantiate_type(value)?;
                self.instantiate_builtin(ty);
            }
            VerificationType::Function {
                parameters, result, ..
            } => {
                for parameter in parameters {
                    self.instantiate_type(parameter)?;
                }
                self.instantiate_type(result)?;
                self.instantiate_builtin(ty);
            }
            VerificationType::Union(members) => {
                for member in members {
                    self.instantiate_type(member)?;
                }
            }
            VerificationType::Bytes | VerificationType::ByteBuffer => {
                self.instantiate_builtin(ty);
            }
            _ => {}
        }
        Ok(())
    }

    fn instantiate_builtin(&mut self, ty: &VerificationType) -> LayoutId {
        if let Some(layout) = self.builtin(ty) {
            return layout;
        }
        let layout = self.push_runtime_kind(LayoutKind::Builtin { ty: ty.clone() });
        self.builtins.insert(ty.clone(), layout);
        layout
    }

    fn instantiate_record(
        &mut self,
        record: RecordId,
        arguments: &[VerificationType],
    ) -> Result<LayoutId, FosterError> {
        let key = (record, arguments.to_vec());
        if let Some(layout) = self.record_instances.get(&key) {
            return Ok(*layout);
        }
        let parameters = self
            .record_parameters
            .get(&record)
            .cloned()
            .unwrap_or_default();
        if parameters.len() != arguments.len() {
            return self.record(record).ok_or_else(|| {
                FosterError::runtime("record specialization has no logical layout")
            });
        }
        let base = self
            .record(record)
            .ok_or_else(|| FosterError::runtime("record specialization has no logical layout"))?;
        if parameters.is_empty() {
            return Ok(base);
        }
        let LayoutKind::Record { name, fields, .. } = self.get(base).kind.clone() else {
            unreachable!()
        };
        let substitutions = parameters
            .into_iter()
            .zip(arguments.iter().cloned())
            .collect::<HashMap<_, _>>();
        let fields = fields
            .into_iter()
            .map(|mut field| {
                field.ty = field.ty.substitute(&substitutions);
                field
            })
            .collect::<Vec<_>>();
        let layout = self.push(LayoutKind::Record {
            record,
            name: name.clone(),
            arguments: arguments.to_vec(),
            fields: Vec::new(),
        });
        self.record_instances.insert(key, layout);
        for field in &fields {
            self.instantiate_type(&field.ty)?;
        }
        self.layouts[layout.0 as usize].kind = LayoutKind::Record {
            record,
            name,
            arguments: arguments.to_vec(),
            fields,
        };
        Ok(layout)
    }

    fn instantiate_variant(
        &mut self,
        variant_type: VariantTypeId,
        arguments: &[VerificationType],
    ) -> Result<LayoutId, FosterError> {
        let key = (variant_type, arguments.to_vec());
        if let Some(layout) = self.variant_instances.get(&key) {
            return Ok(*layout);
        }
        let parameters = self
            .variant_parameters
            .get(&variant_type)
            .cloned()
            .unwrap_or_default();
        if parameters.len() != arguments.len() {
            return self.variant(variant_type).ok_or_else(|| {
                FosterError::runtime("variant specialization has no logical layout")
            });
        }
        let base = self
            .variant(variant_type)
            .ok_or_else(|| FosterError::runtime("variant specialization has no logical layout"))?;
        if parameters.is_empty() {
            return Ok(base);
        }
        let LayoutKind::Variant {
            name, alternatives, ..
        } = self.get(base).kind.clone()
        else {
            unreachable!()
        };
        let substitutions = parameters
            .into_iter()
            .zip(arguments.iter().cloned())
            .collect::<HashMap<_, _>>();
        let alternatives = alternatives
            .into_iter()
            .map(|mut alternative| {
                alternative.payload = alternative
                    .payload
                    .iter()
                    .map(|ty| ty.substitute(&substitutions))
                    .collect();
                alternative
            })
            .collect::<Vec<_>>();
        let layout = self.push(LayoutKind::Variant {
            variant_type,
            name: name.clone(),
            arguments: arguments.to_vec(),
            alternatives: Vec::new(),
        });
        self.variant_instances.insert(key, layout);
        for alternative in &alternatives {
            for ty in &alternative.payload {
                self.instantiate_type(ty)?;
            }
        }
        self.layouts[layout.0 as usize].kind = LayoutKind::Variant {
            variant_type,
            name,
            arguments: arguments.to_vec(),
            alternatives,
        };
        Ok(layout)
    }

    pub fn closure(&self, id: FunctionId) -> Option<LayoutId> {
        self.closures.get(&id).copied()
    }

    pub fn closure_instance(
        &self,
        id: FunctionId,
        specialization: &crate::vm::Specialization,
    ) -> Option<LayoutId> {
        self.closure_instances
            .get(&(id, specialization.clone()))
            .copied()
            .or_else(|| {
                specialization
                    .is_empty()
                    .then(|| self.closure(id))
                    .flatten()
            })
    }

    /// Materialize the capture environment for one concrete closure body instance.
    pub fn instantiate_closure(
        &mut self,
        function: FunctionId,
        specialization: &crate::vm::Specialization,
    ) -> Result<LayoutId, FosterError> {
        let key = (function, specialization.clone());
        if let Some(layout) = self.closure_instances.get(&key) {
            return Ok(*layout);
        }
        let base = self
            .closure(function)
            .ok_or_else(|| FosterError::runtime("closure specialization has no logical layout"))?;
        if specialization.is_empty() {
            self.closure_instances.insert(key, base);
            return Ok(base);
        }
        let LayoutKind::Closure { captures, .. } = self.get(base).kind.clone() else {
            unreachable!()
        };
        let substitutions = specialization.iter().cloned().collect::<HashMap<_, _>>();
        let captures = captures
            .into_iter()
            .map(|mut capture| {
                capture.ty = capture.ty.substitute(&substitutions);
                capture
            })
            .collect::<Vec<_>>();
        let layout = self.push(LayoutKind::Closure {
            function,
            specialization: specialization.clone(),
            captures: Vec::new(),
        });
        self.closure_instances.insert(key, layout);
        for capture in &captures {
            self.instantiate_type(&capture.ty)?;
        }
        self.layouts[layout.0 as usize].kind = LayoutKind::Closure {
            function,
            specialization: specialization.clone(),
            captures,
        };
        Ok(layout)
    }

    pub fn pointer(&self, pointee: &VerificationType, ownership: Ownership) -> Option<LayoutId> {
        self.pointers.get(&(pointee.clone(), ownership)).copied()
    }

    pub fn builtin(&self, ty: &VerificationType) -> Option<LayoutId> {
        self.builtins.get(ty).copied()
    }

    pub fn opaque(&self) -> LayoutId {
        self.opaque.expect("every registry has an opaque layout")
    }

    /// Reduce a verifier type to the scalar-or-pointer contract shared by the VM and Cranelift.
    pub fn legal_type(&self, ty: &VerificationType) -> LegalType {
        match ty {
            VerificationType::Unit | VerificationType::Bool | VerificationType::Byte => {
                LegalType::I8
            }
            VerificationType::CodePoint => LegalType::I32,
            VerificationType::Integer => LegalType::I64,
            VerificationType::Float => LegalType::F64,
            VerificationType::Record { record, .. } => LegalType::Pointer {
                layout: match ty {
                    VerificationType::Record { arguments, .. } => {
                        self.record_instance(*record, arguments)
                    }
                    _ => unreachable!(),
                },
                ownership: Ownership::Owned,
            },
            VerificationType::Variant { variant, .. } => LegalType::Pointer {
                layout: match ty {
                    VerificationType::Variant { arguments, .. } => {
                        self.variant_instance(*variant, arguments)
                    }
                    _ => unreachable!(),
                },
                ownership: Ownership::Owned,
            },
            VerificationType::Reference(pointee) => LegalType::Pointer {
                layout: self.pointer(pointee, Ownership::Borrowed),
                ownership: Ownership::Borrowed,
            },
            VerificationType::Unknown | VerificationType::Union(_) => LegalType::Opaque,
            VerificationType::Generic(_) => LegalType::UnresolvedGeneric,
            VerificationType::Bytes
            | VerificationType::ByteBuffer
            | VerificationType::List(_)
            | VerificationType::Remote(_)
            | VerificationType::Future(_)
            | VerificationType::Function { .. } => LegalType::Pointer {
                layout: self.builtin(ty),
                ownership: Ownership::Owned,
            },
        }
    }

    fn push(&mut self, kind: LayoutKind) -> LayoutId {
        self.push_with_materialization(kind, true)
    }

    fn push_schema(&mut self, kind: LayoutKind) -> LayoutId {
        self.push_with_materialization(kind, false)
    }

    fn push_runtime_kind(&mut self, kind: LayoutKind) -> LayoutId {
        if layout_kind_has_generic(&kind) {
            self.push_schema(kind)
        } else {
            self.push(kind)
        }
    }

    fn push_with_materialization(&mut self, kind: LayoutKind, materialized: bool) -> LayoutId {
        let id = LayoutId(self.layouts.len() as u32);
        self.layouts.push(Layout {
            id,
            boxed: true,
            materialized,
            kind,
        });
        id
    }
}

fn layout_kind_has_generic(kind: &LayoutKind) -> bool {
    match kind {
        LayoutKind::Record { fields, .. } => fields.iter().any(|field| field.ty.contains_generic()),
        LayoutKind::Variant { alternatives, .. } => alternatives
            .iter()
            .flat_map(|alternative| &alternative.payload)
            .any(VerificationType::contains_generic),
        LayoutKind::Closure { captures, .. } => {
            captures.iter().any(|capture| capture.ty.contains_generic())
        }
        LayoutKind::Pointer { pointee, .. } => pointee.contains_generic(),
        LayoutKind::Builtin { ty } => ty.contains_generic(),
        LayoutKind::Opaque => false,
    }
}

/// Canonicalize aggregate operands and construct the complete logical layout table.
///
/// This is deliberately run before optimization.  Consequently field order and variant tags are
/// stable even when an optimizer later removes a construction site.
pub fn legalize(program: &mut Program) -> Result<Registry, FosterError> {
    let mut registry = Registry::default();
    registry.opaque = Some(registry.push(LayoutKind::Opaque));

    let mut records = program.records.iter().collect::<Vec<_>>();
    records.sort_unstable_by_key(|(id, _)| id.into_raw().into_u32());
    for (record, runtime) in records {
        registry
            .record_parameters
            .insert(*record, runtime.parameters.clone());
        if runtime.layout.names().len() != runtime.field_types.len() {
            return Err(FosterError::runtime(format!(
                "record `{}` has inconsistent typed layout metadata",
                runtime.name
            )));
        }
        let fields = runtime
            .layout
            .names()
            .iter()
            .zip(&runtime.field_types)
            .enumerate()
            .map(|(index, (name, ty))| Slot {
                index: index as u32,
                name: name.clone(),
                ty: ty.clone(),
                ownership: Ownership::Owned,
            })
            .collect();
        let kind = LayoutKind::Record {
            record: *record,
            name: runtime.name.clone(),
            arguments: Vec::new(),
            fields,
        };
        let layout = if runtime.parameters.is_empty() {
            registry.push(kind)
        } else {
            registry.push_schema(kind)
        };
        registry.records.insert(*record, layout);
    }

    let mut by_parent = BTreeMap::<u32, (VariantTypeId, Vec<(VariantId, String)>)>::new();
    for (variant, runtime) in &program.variants {
        by_parent
            .entry(runtime.parent.into_raw().into_u32())
            .or_insert_with(|| (runtime.parent, Vec::new()))
            .1
            .push((*variant, runtime.alternative.to_string()));
    }
    for (_, (variant_type, mut entries)) in by_parent {
        if let Some((variant, _)) = entries.first() {
            registry
                .variant_parameters
                .insert(variant_type, program.variants[variant].parameters.clone());
        }
        entries.sort_unstable_by_key(|(id, _)| id.into_raw().into_u32());
        let name = entries
            .first()
            .map(|(variant, _)| program.variants[variant].type_name.to_string())
            .unwrap_or_default();
        let alternatives = entries
            .into_iter()
            .enumerate()
            .map(|(tag, (variant, name))| Alternative {
                variant,
                tag: tag as u32,
                name,
                payload: program.variants[&variant].payload.clone(),
            })
            .collect();
        let kind = LayoutKind::Variant {
            variant_type,
            name,
            arguments: Vec::new(),
            alternatives,
        };
        let layout = if registry.variant_parameters[&variant_type].is_empty() {
            registry.push(kind)
        } else {
            registry.push_schema(kind)
        };
        registry.variants.insert(variant_type, layout);
    }

    let closure_targets = program
        .functions
        .values()
        .flat_map(|function| &function.instructions)
        .filter_map(|instruction| match instruction {
            Instruction::MakeClosure { function, .. }
            | Instruction::CallClosure { function, .. } => Some(*function),
            _ => None,
        })
        .collect::<std::collections::HashSet<_>>();
    let mut closure_modes = HashMap::new();
    for instruction in program
        .functions
        .values()
        .flat_map(|function| &function.instructions)
    {
        let (target, captures) = match instruction {
            Instruction::MakeClosure {
                function, captures, ..
            }
            | Instruction::CallClosure {
                function, captures, ..
            } => (*function, captures),
            _ => continue,
        };
        let modes = captures.iter().map(|(mode, _)| *mode).collect::<Vec<_>>();
        match closure_modes.get(&target) {
            Some(previous) if previous != &modes => {
                return Err(FosterError::runtime(format!(
                    "closure target `{}` is constructed with inconsistent capture modes",
                    program.functions[&target].name
                )));
            }
            _ => {
                closure_modes.insert(target, modes);
            }
        }
    }
    let mut functions = program.functions.iter().collect::<Vec<_>>();
    functions.sort_unstable_by_key(|(id, _)| id.into_raw().into_u32());
    for (function, body) in functions {
        if body.captures == 0 && !closure_targets.contains(function) {
            continue;
        }
        let captures =
            body.capture_types
                .iter()
                .enumerate()
                .map(|(index, ty)| Slot {
                    index: index as u32,
                    name: format!("capture{index}"),
                    ty: ty.clone(),
                    ownership: closure_modes
                        .get(function)
                        .and_then(|modes| modes.get(index))
                        .map_or_else(
                            || {
                                if matches!(ty, VerificationType::Reference(_)) {
                                    Ownership::Borrowed
                                } else {
                                    Ownership::Owned
                                }
                            },
                            |mode| match mode {
                                crate::hir::CaptureMode::Ref => Ownership::Borrowed,
                                crate::hir::CaptureMode::Copy => Ownership::Shared,
                                crate::hir::CaptureMode::Move
                                | crate::hir::CaptureMode::Pending => Ownership::Owned,
                            },
                        ),
                })
                .collect();
        let kind = LayoutKind::Closure {
            function: *function,
            specialization: Vec::new(),
            captures,
        };
        let layout = if layout_kind_has_generic(&kind) {
            registry.push_schema(kind)
        } else {
            registry.push(kind)
        };
        registry.closures.insert(*function, layout);
        registry
            .closure_instances
            .insert((*function, Vec::new()), layout);
    }

    collect_runtime_layouts(program, &mut registry);
    canonicalize_and_verify(program, &registry)?;
    Ok(registry)
}

fn collect_runtime_layouts(program: &Program, registry: &mut Registry) {
    let mut types = BTreeSet::new();
    for function in program.functions.values() {
        types.extend(function.parameter_types.iter().cloned());
        types.extend(function.capture_types.iter().cloned());
        types.insert(function.result_type.clone());
        for instruction in &function.instructions {
            match instruction {
                Instruction::MakeList { element_type, .. } => {
                    types.insert(VerificationType::List(Box::new(element_type.clone())));
                }
                Instruction::MakeReference { pointee_type, .. }
                | Instruction::MakeWholeReference { pointee_type, .. }
                | Instruction::MakeFieldReference { pointee_type, .. } => {
                    types.insert(VerificationType::Reference(Box::new(pointee_type.clone())));
                }
                _ => {}
            }
        }
    }
    for record in program.records.values() {
        types.extend(record.field_types.iter().cloned());
    }
    for variant in program.variants.values() {
        types.extend(variant.payload.iter().cloned());
    }
    for ty in types {
        visit_runtime_types(&ty, registry);
    }
}

fn visit_runtime_types(ty: &VerificationType, registry: &mut Registry) {
    match ty {
        VerificationType::Reference(pointee) => {
            let key = ((**pointee).clone(), Ownership::Borrowed);
            if !registry.pointers.contains_key(&key) {
                let id = registry.push_runtime_kind(LayoutKind::Pointer {
                    pointee: (**pointee).clone(),
                    ownership: Ownership::Borrowed,
                });
                registry.pointers.insert(key, id);
            }
            visit_runtime_types(pointee, registry);
        }
        VerificationType::List(element)
        | VerificationType::Remote(element)
        | VerificationType::Future(element) => {
            if !registry.builtins.contains_key(ty) {
                let id = registry.push_runtime_kind(LayoutKind::Builtin { ty: ty.clone() });
                registry.builtins.insert(ty.clone(), id);
            }
            visit_runtime_types(element, registry);
        }
        VerificationType::Function {
            parameters, result, ..
        } => {
            if !registry.builtins.contains_key(ty) {
                let id = registry.push_runtime_kind(LayoutKind::Builtin { ty: ty.clone() });
                registry.builtins.insert(ty.clone(), id);
            }
            parameters
                .iter()
                .for_each(|ty| visit_runtime_types(ty, registry));
            visit_runtime_types(result, registry);
        }
        VerificationType::Union(types) => {
            types
                .iter()
                .for_each(|ty| visit_runtime_types(ty, registry));
        }
        VerificationType::Record { arguments, .. }
        | VerificationType::Variant { arguments, .. } => {
            arguments
                .iter()
                .for_each(|ty| visit_runtime_types(ty, registry));
        }
        VerificationType::Bytes | VerificationType::ByteBuffer
            if !registry.builtins.contains_key(ty) =>
        {
            let id = registry.push_runtime_kind(LayoutKind::Builtin { ty: ty.clone() });
            registry.builtins.insert(ty.clone(), id);
        }
        _ => {}
    }
}

fn canonicalize_and_verify(program: &mut Program, registry: &Registry) -> Result<(), FosterError> {
    let closure_metadata = program
        .functions
        .iter()
        .map(|(id, function)| (*id, (function.name.clone(), function.captures)))
        .collect::<HashMap<_, _>>();
    for function in program.functions.values_mut() {
        for instruction in &mut function.instructions {
            match instruction {
                Instruction::MakeRecord { record, fields, .. } => {
                    let Some(layout) = registry.record(*record) else {
                        return Err(FosterError::runtime("record construction has no layout"));
                    };
                    let LayoutKind::Record { fields: slots, .. } = &registry.get(layout).kind
                    else {
                        unreachable!();
                    };
                    let mut supplied = fields.drain(..).collect::<HashMap<_, _>>();
                    *fields = slots
                        .iter()
                        .map(|slot| {
                            supplied
                                .remove(&slot.name)
                                .map(|value| (slot.name.clone(), value))
                                .ok_or_else(|| {
                                    FosterError::runtime(format!(
                                        "record construction is missing field `{}`",
                                        slot.name
                                    ))
                                })
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    if let Some((name, _)) = supplied.into_iter().next() {
                        return Err(FosterError::runtime(format!(
                            "record construction has unknown field `{name}`"
                        )));
                    }
                }
                Instruction::MakeVariant {
                    variant, payload, ..
                } => {
                    let Some(runtime) = program.variants.get(variant) else {
                        return Err(FosterError::runtime("variant construction has no layout"));
                    };
                    let layout = registry.variant(runtime.parent).expect("registered above");
                    let LayoutKind::Variant { alternatives, .. } = &registry.get(layout).kind
                    else {
                        unreachable!();
                    };
                    let alternative = alternatives
                        .iter()
                        .find(|alternative| alternative.variant == *variant)
                        .expect("registered above");
                    if payload.len() != alternative.payload.len() {
                        return Err(FosterError::runtime(format!(
                            "variant `{}` expects {} payload values but has {}",
                            alternative.name,
                            alternative.payload.len(),
                            payload.len()
                        )));
                    }
                }
                Instruction::MakeClosure {
                    function: target,
                    captures,
                    ..
                }
                | Instruction::CallClosure {
                    function: target,
                    captures,
                    ..
                } => {
                    let (target_name, target_captures) = &closure_metadata[target];
                    let expected = usize::from(*target_captures);
                    if captures.len() != expected {
                        return Err(FosterError::runtime(format!(
                            "closure for `{}` expects {expected} captures but has {}",
                            target_name,
                            captures.len()
                        )));
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hir::{Function, FunctionId, Record, RecordId};
    use crate::vm::{BytecodeFunction, Register, RuntimeRecord};
    use la_arena::{Idx, RawIdx};
    use std::sync::Arc;

    #[test]
    fn record_fields_are_legalized_to_layout_order() {
        let record: RecordId = Idx::<Record>::from_raw(RawIdx::from_u32(0));
        let function: FunctionId = Idx::<Function>::from_raw(RawIdx::from_u32(0));
        let mut program = Program::default();
        program.records.insert(
            record,
            RuntimeRecord {
                name: "Pair".into(),
                parameters: Vec::new(),
                layout: Arc::new(crate::vm::RecordLayout::new(vec!["a".into(), "b".into()])),
                field_types: vec![VerificationType::Integer, VerificationType::Bool],
            },
        );
        program.functions.insert(
            function,
            BytecodeFunction {
                name: "f".into(),
                intrinsic_stub: false,
                parameters: 0,
                parameter_types: vec![],
                parameter_modes: vec![],
                mutable_parameters: vec![],
                returns_reference: false,
                captures: 0,
                capture_types: vec![],
                result_type: VerificationType::Record {
                    record,
                    arguments: Vec::new(),
                },
                registers: 3,
                instructions: vec![Instruction::MakeRecord {
                    destination: Register(2),
                    record,
                    type_arguments: Vec::new(),
                    fields: vec![("b".into(), Register(1)), ("a".into(), Register(0))],
                }],
                instruction_spans: std::iter::once(0..0).collect(),
            },
        );
        let registry = legalize(&mut program).unwrap();
        let layout = registry.record(record).unwrap();
        assert_eq!(
            registry.legal_type(&VerificationType::Record {
                record,
                arguments: Vec::new(),
            }),
            LegalType::Pointer {
                layout: Some(layout),
                ownership: Ownership::Owned,
            }
        );
        assert_eq!(
            registry.legal_type(&VerificationType::Unknown),
            LegalType::Opaque
        );
        let Instruction::MakeRecord { fields, .. } = &program.functions[&function].instructions[0]
        else {
            panic!();
        };
        assert_eq!(fields[0].0, "a");
        assert_eq!(fields[1].0, "b");
    }

    #[test]
    fn logical_layout_ids_do_not_depend_on_hash_map_insertion_order() {
        let first: RecordId = Idx::<Record>::from_raw(RawIdx::from_u32(0));
        let second: RecordId = Idx::<Record>::from_raw(RawIdx::from_u32(1));
        let runtime = |name: &str| RuntimeRecord {
            name: name.into(),
            parameters: Vec::new(),
            layout: Arc::new(crate::vm::RecordLayout::new(vec!["value".into()])),
            field_types: vec![VerificationType::Integer],
        };
        let mut left = Program::default();
        left.records.insert(second, runtime("Second"));
        left.records.insert(first, runtime("First"));
        let mut right = Program::default();
        right.records.insert(first, runtime("First"));
        right.records.insert(second, runtime("Second"));

        assert_eq!(legalize(&mut left).unwrap(), legalize(&mut right).unwrap());
    }

    #[test]
    fn generic_schemas_are_not_executable_layouts() {
        let compilation = crate::compile(
            r#"
type Boxed<T> = { value: T }
func main() -> Int { Boxed { value: 42 }.value }
"#,
        )
        .unwrap();
        let mut program = crate::vm::compile_with_options(
            &compilation,
            crate::vm::CompileOptions { optimize: false },
        )
        .unwrap();
        let mut registry = legalize(&mut program).unwrap();
        let record = program
            .records
            .iter()
            .find_map(|(id, record)| (record.name == "Boxed").then_some(*id))
            .unwrap();
        let schema = registry.record(record).unwrap();
        assert!(!registry.get(schema).materialized);
        assert_eq!(
            registry.legal_type(&VerificationType::Generic("T".into())),
            LegalType::UnresolvedGeneric
        );

        let concrete = VerificationType::Record {
            record,
            arguments: vec![VerificationType::Integer],
        };
        registry.instantiate_type(&concrete).unwrap();
        let instance = registry
            .record_instance(record, &[VerificationType::Integer])
            .unwrap();
        assert_ne!(schema, instance);
        assert!(registry.get(instance).materialized);
        let physical =
            physical::PhysicalRegistry::build(&registry, physical::TargetLayout::host()).unwrap();
        assert!(!physical.get(schema).materialized);
        assert!(physical.get(instance).materialized);
    }

    #[test]
    fn runtime_backed_types_receive_concrete_physical_layouts() {
        let mut registry = Registry::default();
        registry.opaque = Some(registry.push(LayoutKind::Opaque));
        let types = [
            VerificationType::Bytes,
            VerificationType::ByteBuffer,
            VerificationType::List(Box::new(VerificationType::Integer)),
            VerificationType::Remote(Box::new(VerificationType::Integer)),
            VerificationType::Future(Box::new(VerificationType::Integer)),
            VerificationType::Function {
                parameters: vec![VerificationType::Integer],
                parameter_modes: vec![crate::ast::ParameterMode::Borrow],
                result: Box::new(VerificationType::Bool),
            },
        ];
        for ty in &types {
            registry.instantiate_type(ty).unwrap();
            assert!(registry.get(registry.builtin(ty).unwrap()).materialized);
        }
        let reference = VerificationType::Reference(Box::new(VerificationType::Integer));
        registry.instantiate_type(&reference).unwrap();
        let place = registry
            .pointer(&VerificationType::Integer, Ownership::Borrowed)
            .unwrap();
        assert!(registry.get(place).materialized);

        let schema = VerificationType::List(Box::new(VerificationType::Generic("T".into())));
        registry.instantiate_type(&schema).unwrap();
        assert!(
            !registry
                .get(registry.builtin(&schema).unwrap())
                .materialized
        );

        let physical =
            physical::PhysicalRegistry::build(&registry, physical::TargetLayout::host()).unwrap();
        assert!(matches!(
            physical
                .get(registry.builtin(&VerificationType::Bytes).unwrap())
                .kind,
            physical::PhysicalKind::Bytes { .. }
        ));
        assert!(matches!(
            physical
                .get(registry.builtin(&VerificationType::ByteBuffer).unwrap())
                .kind,
            physical::PhysicalKind::Buffer { .. }
        ));
        assert!(matches!(
            physical.get(place).kind,
            physical::PhysicalKind::Place { .. }
        ));
        let callable = registry.builtin(&types[5]).unwrap();
        let physical::PhysicalKind::Callable {
            code_offset,
            environment_offset,
            release_offset,
        } = physical.get(callable).kind
        else {
            panic!("function type did not receive a callable layout");
        };
        assert!(code_offset < environment_offset);
        assert!(environment_offset < release_offset);
        assert!(release_offset < physical.get(callable).size);

        let physical::PhysicalKind::Opaque {
            value_offset,
            release_offset,
            ..
        } = physical.get(registry.opaque()).kind
        else {
            panic!("erased values did not receive an opaque layout");
        };
        assert!(value_offset < release_offset);
        assert!(release_offset < physical.get(registry.opaque()).size);
    }
}
