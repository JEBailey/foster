//! Target-independent aggregate layout legalization.
//!
//! The executable IR names logical fields, alternatives, captures, and places.  This module
//! turns those names into deterministic slots before a backend sees them.  VM values remain
//! boxed Rust values, while native backends can use the same descriptions to choose an ABI.

use std::collections::{BTreeMap, HashMap};

use crate::error::FosterError;
use crate::hir::{FunctionId, RecordId, VariantId, VariantTypeId};
use crate::vm::{Instruction, Program, VerificationType};

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
        fields: Vec<Slot>,
    },
    Variant {
        variant_type: VariantTypeId,
        alternatives: Vec<Alternative>,
    },
    Closure {
        function: FunctionId,
        captures: Vec<Slot>,
    },
    /// A place handle is two scalar components: an owning slot pointer and a projection path.
    Pointer {
        pointee: VerificationType,
        ownership: Ownership,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub id: LayoutId,
    /// Aggregate values cross both current backend boundaries as one pointer-sized scalar.
    pub boxed: bool,
    pub kind: LayoutKind,
}

/// The only values a backend receives after representation legalization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegalType {
    /// Erased generics and heterogeneous joins cannot be assigned a physical ABI yet.
    Opaque,
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
    closures: HashMap<FunctionId, LayoutId>,
    pointers: BTreeMap<(VerificationType, Ownership), LayoutId>,
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

    pub fn variant(&self, id: VariantTypeId) -> Option<LayoutId> {
        self.variants.get(&id).copied()
    }

    pub fn closure(&self, id: FunctionId) -> Option<LayoutId> {
        self.closures.get(&id).copied()
    }

    pub fn pointer(&self, pointee: &VerificationType, ownership: Ownership) -> Option<LayoutId> {
        self.pointers.get(&(pointee.clone(), ownership)).copied()
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
            VerificationType::Record(record) => LegalType::Pointer {
                layout: self.record(*record),
                ownership: Ownership::Owned,
            },
            VerificationType::Variant(variant) => LegalType::Pointer {
                layout: self.variant(*variant),
                ownership: Ownership::Owned,
            },
            VerificationType::Reference(pointee) => LegalType::Pointer {
                layout: self.pointer(pointee, Ownership::Borrowed),
                ownership: Ownership::Borrowed,
            },
            VerificationType::Unknown | VerificationType::Union(_) => LegalType::Opaque,
            VerificationType::Bytes
            | VerificationType::ByteBuffer
            | VerificationType::List(_)
            | VerificationType::Remote(_)
            | VerificationType::Future(_)
            | VerificationType::Function { .. } => LegalType::Pointer {
                layout: None,
                ownership: Ownership::Owned,
            },
        }
    }

    fn push(&mut self, kind: LayoutKind) -> LayoutId {
        let id = LayoutId(self.layouts.len() as u32);
        self.layouts.push(Layout {
            id,
            boxed: true,
            kind,
        });
        id
    }
}

/// Canonicalize aggregate operands and construct the complete logical layout table.
///
/// This is deliberately run before optimization.  Consequently field order and variant tags are
/// stable even when an optimizer later removes a construction site.
pub fn legalize(program: &mut Program) -> Result<Registry, FosterError> {
    let mut registry = Registry::default();

    let mut records = program.records.iter().collect::<Vec<_>>();
    records.sort_unstable_by_key(|(id, _)| id.into_raw().into_u32());
    for (record, runtime) in records {
        let fields = runtime
            .layout
            .names()
            .iter()
            .enumerate()
            .map(|(index, name)| Slot {
                index: index as u32,
                name: name.clone(),
                ty: VerificationType::Unknown,
            })
            .collect();
        let layout = registry.push(LayoutKind::Record {
            record: *record,
            fields,
        });
        registry.records.insert(*record, layout);
    }

    let mut payloads = HashMap::<VariantId, Vec<VerificationType>>::new();
    for function in program.functions.values() {
        for instruction in &function.instructions {
            if let Instruction::MakeVariant {
                variant, payload, ..
            } = instruction
            {
                let shape = vec![VerificationType::Unknown; payload.len()];
                match payloads.get(variant) {
                    Some(previous) if previous.len() != shape.len() => {
                        return Err(FosterError::runtime(format!(
                            "variant is constructed with both {} and {} payload values",
                            previous.len(),
                            shape.len()
                        )));
                    }
                    _ => {
                        payloads.insert(*variant, shape);
                    }
                }
            }
        }
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
        entries.sort_unstable_by_key(|(id, _)| id.into_raw().into_u32());
        let alternatives = entries
            .into_iter()
            .enumerate()
            .map(|(tag, (variant, name))| Alternative {
                variant,
                tag: tag as u32,
                name,
                payload: payloads.remove(&variant).unwrap_or_default(),
            })
            .collect();
        let layout = registry.push(LayoutKind::Variant {
            variant_type,
            alternatives,
        });
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
    let mut functions = program.functions.iter().collect::<Vec<_>>();
    functions.sort_unstable_by_key(|(id, _)| id.into_raw().into_u32());
    for (function, body) in functions {
        if body.captures == 0 && !closure_targets.contains(function) {
            continue;
        }
        let captures = body
            .capture_types
            .iter()
            .enumerate()
            .map(|(index, ty)| Slot {
                index: index as u32,
                name: format!("capture{index}"),
                ty: ty.clone(),
            })
            .collect();
        let layout = registry.push(LayoutKind::Closure {
            function: *function,
            captures,
        });
        registry.closures.insert(*function, layout);
    }

    collect_pointer_layouts(program, &mut registry);
    canonicalize_and_verify(program, &registry)?;
    Ok(registry)
}

fn collect_pointer_layouts(program: &Program, registry: &mut Registry) {
    let mut types = Vec::new();
    for function in program.functions.values() {
        types.extend(function.parameter_types.iter().cloned());
        types.extend(function.capture_types.iter().cloned());
        types.push(function.result_type.clone());
    }
    for ty in types {
        visit_references(&ty, &mut |pointee| {
            let key = (pointee.clone(), Ownership::Borrowed);
            if !registry.pointers.contains_key(&key) {
                let id = registry.push(LayoutKind::Pointer {
                    pointee: pointee.clone(),
                    ownership: Ownership::Borrowed,
                });
                registry.pointers.insert(key, id);
            }
        });
    }
    let constructs_erased_reference = program.functions.values().any(|function| {
        function.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::MakeReference { .. }
                    | Instruction::MakeWholeReference { .. }
                    | Instruction::MakeFieldReference { .. }
            )
        })
    });
    if constructs_erased_reference {
        let key = (VerificationType::Unknown, Ownership::Borrowed);
        if !registry.pointers.contains_key(&key) {
            let id = registry.push(LayoutKind::Pointer {
                pointee: VerificationType::Unknown,
                ownership: Ownership::Borrowed,
            });
            registry.pointers.insert(key, id);
        }
    }
}

fn visit_references(ty: &VerificationType, visit: &mut impl FnMut(&VerificationType)) {
    match ty {
        VerificationType::Reference(pointee) => {
            visit(pointee);
            visit_references(pointee, visit);
        }
        VerificationType::List(element)
        | VerificationType::Remote(element)
        | VerificationType::Future(element) => visit_references(element, visit),
        VerificationType::Function {
            parameters, result, ..
        } => {
            parameters.iter().for_each(|ty| visit_references(ty, visit));
            visit_references(result, visit);
        }
        VerificationType::Union(types) => {
            types.iter().for_each(|ty| visit_references(ty, visit));
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
                layout: Arc::new(crate::vm::RecordLayout::new(vec!["a".into(), "b".into()])),
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
                result_type: VerificationType::Record(record),
                registers: 3,
                instructions: vec![Instruction::MakeRecord {
                    destination: Register(2),
                    record,
                    fields: vec![("b".into(), Register(1)), ("a".into(), Register(0))],
                }],
                instruction_spans: std::iter::once(0..0).collect(),
            },
        );
        let registry = legalize(&mut program).unwrap();
        let layout = registry.record(record).unwrap();
        assert_eq!(
            registry.legal_type(&VerificationType::Record(record)),
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
}
