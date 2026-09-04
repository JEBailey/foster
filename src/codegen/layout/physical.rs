//! Target-aware physical layouts derived from the portable logical registry.
//!
//! Logical layout identities remain portable. Sizes, alignments, byte offsets, and object-header
//! details are computed only when a backend selects a target.

use std::fmt;

use super::{LayoutId, LayoutKind, LegalType, Ownership, Registry, Slot};
use crate::vm::VerificationType;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetLayout {
    pointer_size: u8,
    pointer_align: u8,
    i64_align: u8,
    f64_align: u8,
}

impl TargetLayout {
    pub fn new(pointer_size: u8, pointer_align: u8) -> Result<Self, LayoutError> {
        let scalar_align = pointer_align.min(8);
        Self::with_scalar_alignments(pointer_size, pointer_align, scalar_align, scalar_align)
    }

    pub fn with_scalar_alignments(
        pointer_size: u8,
        pointer_align: u8,
        i64_align: u8,
        f64_align: u8,
    ) -> Result<Self, LayoutError> {
        if !matches!(pointer_size, 4 | 8) {
            return Err(LayoutError::new(format!(
                "unsupported target pointer size {pointer_size}"
            )));
        }
        if pointer_align == 0 || !pointer_align.is_power_of_two() || pointer_align > pointer_size {
            return Err(LayoutError::new(format!(
                "invalid target pointer alignment {pointer_align} for {pointer_size}-byte pointers"
            )));
        }
        for (name, align) in [("i64", i64_align), ("f64", f64_align)] {
            if align == 0 || !align.is_power_of_two() || align > 8 {
                return Err(LayoutError::new(format!(
                    "invalid target {name} alignment {align}"
                )));
            }
        }
        Ok(Self {
            pointer_size,
            pointer_align,
            i64_align,
            f64_align,
        })
    }

    pub fn host() -> Self {
        let pointer = std::mem::size_of::<usize>() as u8;
        Self {
            pointer_size: pointer,
            pointer_align: pointer,
            i64_align: std::mem::align_of::<i64>() as u8,
            f64_align: std::mem::align_of::<f64>() as u8,
        }
    }

    pub fn pointer_size(self) -> u8 {
        self.pointer_size
    }

    pub fn pointer_align(self) -> u8 {
        self.pointer_align
    }

    fn integer64_align(self) -> u16 {
        u16::from(self.i64_align)
    }

    fn float64_align(self) -> u16 {
        u16::from(self.f64_align)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutError {
    message: String,
}

impl LayoutError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for LayoutError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for LayoutError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectHeader {
    pub descriptor_offset: u32,
    pub strong_count_offset: u32,
    pub flags_offset: u32,
    pub size: u32,
    pub align: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarKind {
    I8,
    I32,
    I64,
    F64,
    Pointer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValueLayout {
    pub size: u32,
    pub align: u16,
    pub kind: ScalarKind,
    /// The object descriptor expected by a pointer value, when statically known.
    pub pointee: Option<LayoutId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldLayout {
    pub index: u32,
    pub name: String,
    pub offset: u32,
    pub value: ValueLayout,
    pub ownership: Ownership,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlternativeLayout {
    pub tag: u32,
    pub name: String,
    pub fields: Vec<FieldLayout>,
    pub payload_size: u32,
    pub payload_align: u16,
}

/// One legalized field/index step in a place-handle projection path.
///
/// `operand` is a field slot or collection index. Index entries snapshot both the root generation
/// and the generation of the preceding projection prefix; field entries leave those words zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectionEntryLayout {
    pub kind_offset: u32,
    pub operand_offset: u32,
    pub root_generation_offset: u32,
    pub projected_generation_offset: u32,
    pub size: u32,
    pub align: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DropPlan {
    Trivial,
    Fields(Vec<DropField>),
    Variant {
        tag_offset: u32,
        alternatives: Vec<VariantDropPlan>,
    },
    Buffer {
        element: ValueLayout,
        mutable: bool,
    },
    Runtime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariantDropPlan {
    pub tag: u32,
    pub fields: Vec<DropField>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DropField {
    pub offset: u32,
    pub pointee: LayoutId,
    pub ownership: Ownership,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhysicalKind {
    Record {
        fields: Vec<FieldLayout>,
    },
    Variant {
        tag_offset: u32,
        payload_offset: u32,
        payload_size: u32,
        payload_align: u16,
        alternatives: Vec<AlternativeLayout>,
    },
    Closure {
        code_offset: u32,
        signature_offset: u32,
        captures: Vec<FieldLayout>,
    },
    Place {
        origin_offset: u32,
        path_offset: u32,
        path_len_offset: u32,
        projection: ProjectionEntryLayout,
    },
    Bytes {
        data_offset: u32,
        length_offset: u32,
    },
    Buffer {
        data_offset: u32,
        length_offset: u32,
        capacity_offset: u32,
        element: ValueLayout,
    },
    Handle {
        handle_offset: u32,
        value_descriptor_offset: u32,
    },
    Callable {
        code_offset: u32,
        environment_offset: u32,
        release_offset: u32,
    },
    Opaque {
        value_offset: u32,
        release_offset: u32,
        value_size: u32,
        value_align: u16,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhysicalLayout {
    pub id: LayoutId,
    /// Generic schemas are measured for stable IDs but never emitted or allocated.
    pub materialized: bool,
    pub size: u32,
    pub align: u16,
    pub header: ObjectHeader,
    pub kind: PhysicalKind,
    pub drop_plan: DropPlan,
}

impl PhysicalLayout {
    /// Encode a versioned, address-free descriptor for emission into native object files.
    pub fn descriptor_bytes(&self) -> Vec<u8> {
        let kind = match self.kind {
            PhysicalKind::Record { .. } => 0_u16,
            PhysicalKind::Variant { .. } => 1,
            PhysicalKind::Closure { .. } => 2,
            PhysicalKind::Place { .. } => 3,
            PhysicalKind::Bytes { .. } => 4,
            PhysicalKind::Buffer { .. } => 5,
            PhysicalKind::Handle { .. } => 6,
            PhysicalKind::Callable { .. } => 7,
            PhysicalKind::Opaque { .. } => 8,
        };
        let drop_kind = match self.drop_plan {
            DropPlan::Trivial => 0_u16,
            DropPlan::Fields(_) => 1,
            DropPlan::Variant { .. } => 2,
            DropPlan::Buffer { .. } => 3,
            DropPlan::Runtime => 4,
        };
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"FLYT");
        push_u16(&mut bytes, 1);
        push_u16(&mut bytes, kind);
        push_u32(&mut bytes, self.id.0);
        push_u32(&mut bytes, self.size);
        push_u16(&mut bytes, self.align);
        push_u16(&mut bytes, drop_kind);
        push_u32(&mut bytes, self.header.size);
        push_u32(&mut bytes, self.header.descriptor_offset);
        push_u32(&mut bytes, self.header.strong_count_offset);
        push_u32(&mut bytes, self.header.flags_offset);
        match &self.kind {
            PhysicalKind::Record { fields } => push_fields(&mut bytes, fields),
            PhysicalKind::Variant {
                tag_offset,
                payload_offset,
                payload_size,
                payload_align,
                alternatives,
            } => {
                push_u32(&mut bytes, *tag_offset);
                push_u32(&mut bytes, *payload_offset);
                push_u32(&mut bytes, *payload_size);
                push_u16(&mut bytes, *payload_align);
                push_u16(&mut bytes, 0);
                push_u32(&mut bytes, alternatives.len() as u32);
                for alternative in alternatives {
                    push_u32(&mut bytes, alternative.tag);
                    push_u32(&mut bytes, alternative.payload_size);
                    push_u16(&mut bytes, alternative.payload_align);
                    push_u16(&mut bytes, 0);
                    push_fields(&mut bytes, &alternative.fields);
                }
            }
            PhysicalKind::Closure {
                code_offset,
                signature_offset,
                captures,
            } => {
                push_u32(&mut bytes, *code_offset);
                push_u32(&mut bytes, *signature_offset);
                push_fields(&mut bytes, captures);
            }
            PhysicalKind::Place {
                origin_offset,
                path_offset,
                path_len_offset,
                projection,
            } => {
                for offset in [origin_offset, path_offset, path_len_offset] {
                    push_u32(&mut bytes, *offset);
                }
                push_u32(&mut bytes, projection.kind_offset);
                push_u32(&mut bytes, projection.operand_offset);
                push_u32(&mut bytes, projection.root_generation_offset);
                push_u32(&mut bytes, projection.projected_generation_offset);
                push_u32(&mut bytes, projection.size);
                push_u16(&mut bytes, projection.align);
                push_u16(&mut bytes, 0);
            }
            PhysicalKind::Bytes {
                data_offset,
                length_offset,
            } => {
                push_u32(&mut bytes, *data_offset);
                push_u32(&mut bytes, *length_offset);
            }
            PhysicalKind::Buffer {
                data_offset,
                length_offset,
                capacity_offset,
                element,
            } => {
                push_u32(&mut bytes, *data_offset);
                push_u32(&mut bytes, *length_offset);
                push_u32(&mut bytes, *capacity_offset);
                push_value(&mut bytes, *element);
            }
            PhysicalKind::Handle {
                handle_offset,
                value_descriptor_offset,
            } => {
                push_u32(&mut bytes, *handle_offset);
                push_u32(&mut bytes, *value_descriptor_offset);
            }
            PhysicalKind::Callable {
                code_offset,
                environment_offset,
                release_offset,
            } => {
                push_u32(&mut bytes, *code_offset);
                push_u32(&mut bytes, *environment_offset);
                push_u32(&mut bytes, *release_offset);
            }
            PhysicalKind::Opaque {
                value_offset,
                release_offset,
                value_size,
                value_align,
            } => {
                push_u32(&mut bytes, *value_offset);
                push_u32(&mut bytes, *release_offset);
                push_u32(&mut bytes, *value_size);
                push_u16(&mut bytes, *value_align);
                push_u16(&mut bytes, 0);
            }
        }
        match &self.drop_plan {
            DropPlan::Fields(fields) => {
                push_u32(&mut bytes, fields.len() as u32);
                for field in fields {
                    push_u32(&mut bytes, field.offset);
                    push_u32(&mut bytes, field.pointee.0);
                    bytes.push(ownership_tag(field.ownership));
                    bytes.extend_from_slice(&[0; 3]);
                }
            }
            DropPlan::Variant {
                tag_offset,
                alternatives,
            } => {
                push_u32(&mut bytes, alternatives.len() as u32);
                push_u32(&mut bytes, *tag_offset);
                for alternative in alternatives {
                    push_u32(&mut bytes, alternative.tag);
                    push_u32(&mut bytes, alternative.fields.len() as u32);
                    for field in &alternative.fields {
                        push_u32(&mut bytes, field.offset);
                        push_u32(&mut bytes, field.pointee.0);
                        bytes.push(ownership_tag(field.ownership));
                        bytes.extend_from_slice(&[0; 3]);
                    }
                }
            }
            DropPlan::Buffer { element, mutable } => {
                push_u32(&mut bytes, 1);
                push_value(&mut bytes, *element);
                bytes.push(u8::from(*mutable));
                bytes.extend_from_slice(&[0; 3]);
            }
            DropPlan::Trivial | DropPlan::Runtime => push_u32(&mut bytes, 0),
        }
        bytes
    }
}

fn push_fields(bytes: &mut Vec<u8>, fields: &[FieldLayout]) {
    push_u32(bytes, fields.len() as u32);
    for field in fields {
        push_u32(bytes, field.index);
        push_u32(bytes, field.offset);
        push_value(bytes, field.value);
        bytes.push(ownership_tag(field.ownership));
        bytes.extend_from_slice(&[0; 3]);
    }
}

fn push_value(bytes: &mut Vec<u8>, value: ValueLayout) {
    push_u32(bytes, value.size);
    push_u16(bytes, value.align);
    bytes.push(match value.kind {
        ScalarKind::I8 => 0,
        ScalarKind::I32 => 1,
        ScalarKind::I64 => 2,
        ScalarKind::F64 => 3,
        ScalarKind::Pointer => 4,
    });
    bytes.push(0);
    push_u32(bytes, value.pointee.map_or(u32::MAX, |layout| layout.0));
}

fn ownership_tag(ownership: Ownership) -> u8 {
    match ownership {
        Ownership::Owned => 0,
        Ownership::Borrowed => 1,
        Ownership::Shared => 2,
    }
}

fn push_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhysicalRegistry {
    target: TargetLayout,
    header: ObjectHeader,
    layouts: Vec<PhysicalLayout>,
}

impl PhysicalRegistry {
    pub fn build(logical: &Registry, target: TargetLayout) -> Result<Self, LayoutError> {
        let header = object_header(target)?;
        let mut layouts = Vec::with_capacity(logical.layouts().len());
        for layout in logical.layouts() {
            let mut physical = calculate_layout(logical, target, header, layout.id, &layout.kind)?;
            physical.materialized = layout.materialized;
            if physical.id.0 as usize != layouts.len() {
                return Err(LayoutError::new("logical layout IDs are not dense"));
            }
            layouts.push(physical);
        }
        let result = Self {
            target,
            header,
            layouts,
        };
        result.validate()?;
        Ok(result)
    }

    pub fn target(&self) -> TargetLayout {
        self.target
    }

    pub fn header(&self) -> ObjectHeader {
        self.header
    }

    pub fn layouts(&self) -> &[PhysicalLayout] {
        &self.layouts
    }

    pub fn get(&self, id: LayoutId) -> &PhysicalLayout {
        &self.layouts[id.0 as usize]
    }

    pub fn record_field(&self, id: LayoutId, slot: u32) -> Option<&FieldLayout> {
        let PhysicalKind::Record { fields } = &self.get(id).kind else {
            return None;
        };
        fields.iter().find(|field| field.index == slot)
    }

    pub fn variant_alternative(&self, id: LayoutId, tag: u32) -> Option<&AlternativeLayout> {
        let PhysicalKind::Variant { alternatives, .. } = &self.get(id).kind else {
            return None;
        };
        alternatives
            .iter()
            .find(|alternative| alternative.tag == tag)
    }

    pub fn closure_capture(&self, id: LayoutId, slot: u32) -> Option<&FieldLayout> {
        let PhysicalKind::Closure { captures, .. } = &self.get(id).kind else {
            return None;
        };
        captures.iter().find(|capture| capture.index == slot)
    }

    pub fn validate(&self) -> Result<(), LayoutError> {
        for layout in &self.layouts {
            if layout.align == 0 || !layout.align.is_power_of_two() {
                return Err(LayoutError::new(format!(
                    "layout l{} has invalid alignment {}",
                    layout.id.0, layout.align
                )));
            }
            if layout.size < layout.header.size
                || !layout.size.is_multiple_of(u32::from(layout.align))
            {
                return Err(LayoutError::new(format!(
                    "layout l{} has invalid size {} for alignment {}",
                    layout.id.0, layout.size, layout.align
                )));
            }
            validate_slot(
                layout,
                layout.header.descriptor_offset,
                u32::from(self.target.pointer_size),
                u16::from(self.target.pointer_align),
                "header descriptor",
            )?;
            validate_slot(
                layout,
                layout.header.strong_count_offset,
                u32::from(self.target.pointer_size),
                u16::from(self.target.pointer_align),
                "header reference count",
            )?;
            validate_slot(layout, layout.header.flags_offset, 4, 4, "header flags")?;
            for field in fields_of(&layout.kind) {
                validate_field(layout, field, self.layouts.len())?;
            }
            validate_kind(layout, self.target)?;
            if let DropPlan::Fields(fields) = &layout.drop_plan {
                for field in fields {
                    if field.pointee.0 as usize >= self.layouts.len() || field.offset >= layout.size
                    {
                        return Err(LayoutError::new(format!(
                            "layout l{} has an invalid drop field",
                            layout.id.0
                        )));
                    }
                }
            }
            if let DropPlan::Variant {
                tag_offset,
                alternatives,
            } = &layout.drop_plan
            {
                if *tag_offset >= layout.size {
                    return Err(LayoutError::new(format!(
                        "layout l{} has an invalid variant drop tag",
                        layout.id.0
                    )));
                }
                for alternative in alternatives {
                    for field in &alternative.fields {
                        if field.pointee.0 as usize >= self.layouts.len()
                            || field.offset >= layout.size
                        {
                            return Err(LayoutError::new(format!(
                                "layout l{} has an invalid variant drop field",
                                layout.id.0
                            )));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

fn object_header(target: TargetLayout) -> Result<ObjectHeader, LayoutError> {
    let pointer_size = u32::from(target.pointer_size);
    let pointer_align = u16::from(target.pointer_align);
    let descriptor_offset = 0;
    let strong_count_offset = pointer_size;
    let flags_offset = pointer_size
        .checked_mul(2)
        .ok_or_else(|| LayoutError::new("object header overflow"))?;
    let size = align_up(
        flags_offset
            .checked_add(4)
            .ok_or_else(|| LayoutError::new("object header overflow"))?,
        pointer_align,
    )?;
    Ok(ObjectHeader {
        descriptor_offset,
        strong_count_offset,
        flags_offset,
        size,
        align: pointer_align,
    })
}

fn calculate_layout(
    registry: &Registry,
    target: TargetLayout,
    header: ObjectHeader,
    id: LayoutId,
    kind: &LayoutKind,
) -> Result<PhysicalLayout, LayoutError> {
    match kind {
        LayoutKind::Record { fields, .. } => {
            let (fields, end, align) = place_fields(registry, target, header.size, fields)?;
            finish(
                id,
                header,
                end,
                align,
                PhysicalKind::Record {
                    fields: fields.clone(),
                },
                drop_plan(&fields),
            )
        }
        LayoutKind::Variant { alternatives, .. } => {
            let tag_offset = align_up(header.size, 4)?;
            let after_tag = checked_add(tag_offset, 4)?;
            let mut lowered = Vec::with_capacity(alternatives.len());
            let mut payload_size = 0;
            let mut payload_align = 1;
            for alternative in alternatives {
                let slots = alternative
                    .payload
                    .iter()
                    .enumerate()
                    .map(|(index, ty)| Slot {
                        index: index as u32,
                        name: format!("payload{index}"),
                        ty: ty.clone(),
                        ownership: Ownership::Owned,
                    })
                    .collect::<Vec<_>>();
                let (fields, end, align) = place_fields(registry, target, 0, &slots)?;
                payload_size = payload_size.max(end);
                payload_align = payload_align.max(align);
                lowered.push(AlternativeLayout {
                    tag: alternative.tag,
                    name: alternative.name.clone(),
                    fields,
                    payload_size: end,
                    payload_align: align,
                });
            }
            let payload_offset = align_up(after_tag, payload_align)?;
            for alternative in &mut lowered {
                for field in &mut alternative.fields {
                    field.offset = checked_add(field.offset, payload_offset)?;
                }
            }
            let end = checked_add(payload_offset, payload_size)?;
            let drop_alternatives = lowered
                .iter()
                .map(|alternative| VariantDropPlan {
                    tag: alternative.tag,
                    fields: drop_fields(&alternative.fields),
                })
                .collect::<Vec<_>>();
            let drop_plan = if drop_alternatives
                .iter()
                .all(|alternative| alternative.fields.is_empty())
            {
                DropPlan::Trivial
            } else {
                DropPlan::Variant {
                    tag_offset,
                    alternatives: drop_alternatives,
                }
            };
            finish(
                id,
                header,
                end,
                header.align.max(payload_align),
                PhysicalKind::Variant {
                    tag_offset,
                    payload_offset,
                    payload_size,
                    payload_align,
                    alternatives: lowered,
                },
                drop_plan,
            )
        }
        LayoutKind::Closure { captures, .. } => {
            let pointer = pointer_value(target, None);
            let code_offset = align_up(header.size, pointer.align)?;
            let signature_offset = checked_add(code_offset, pointer.size)?;
            let captures_start = checked_add(signature_offset, pointer.size)?;
            let (captures, end, align) = place_fields(registry, target, captures_start, captures)?;
            finish(
                id,
                header,
                end,
                header.align.max(align),
                PhysicalKind::Closure {
                    code_offset,
                    signature_offset,
                    captures: captures.clone(),
                },
                drop_plan(&captures),
            )
        }
        LayoutKind::Pointer { .. } => place_layout(id, target, header),
        LayoutKind::Builtin { ty } => builtin_layout(registry, id, target, header, ty),
        LayoutKind::Opaque => opaque_layout(id, target, header),
    }
}

fn place_layout(
    id: LayoutId,
    target: TargetLayout,
    header: ObjectHeader,
) -> Result<PhysicalLayout, LayoutError> {
    let pointer = pointer_value(target, None);
    let word = pointer;
    let origin_offset = align_up(header.size, pointer.align)?;
    let path_offset = checked_add(origin_offset, pointer.size)?;
    let path_len_offset = checked_add(path_offset, pointer.size)?;
    let end = checked_add(path_len_offset, word.size)?;

    let generation_align = target.integer64_align();
    let operand_offset = align_up(1, word.align)?;
    let root_generation_offset =
        align_up(checked_add(operand_offset, word.size)?, generation_align)?;
    let projected_generation_offset = checked_add(root_generation_offset, 8)?;
    let projection_align = word.align.max(generation_align);
    let projection = ProjectionEntryLayout {
        kind_offset: 0,
        operand_offset,
        root_generation_offset,
        projected_generation_offset,
        size: align_up(
            checked_add(projected_generation_offset, 8)?,
            projection_align,
        )?,
        align: projection_align,
    };
    finish(
        id,
        header,
        end,
        header.align,
        PhysicalKind::Place {
            origin_offset,
            path_offset,
            path_len_offset,
            projection,
        },
        DropPlan::Runtime,
    )
}

fn builtin_layout(
    registry: &Registry,
    id: LayoutId,
    target: TargetLayout,
    header: ObjectHeader,
    ty: &VerificationType,
) -> Result<PhysicalLayout, LayoutError> {
    let pointer = pointer_value(target, None);
    let word = pointer;
    match ty {
        VerificationType::Bytes => {
            let data_offset = align_up(header.size, pointer.align)?;
            let length_offset = checked_add(data_offset, pointer.size)?;
            let end = checked_add(length_offset, word.size)?;
            finish(
                id,
                header,
                end,
                header.align,
                PhysicalKind::Bytes {
                    data_offset,
                    length_offset,
                },
                DropPlan::Runtime,
            )
        }
        VerificationType::ByteBuffer | VerificationType::List(_) => {
            let data_offset = align_up(header.size, pointer.align)?;
            let length_offset = checked_add(data_offset, pointer.size)?;
            let capacity_offset = checked_add(length_offset, word.size)?;
            let end = checked_add(capacity_offset, word.size)?;
            let element_ty = match ty {
                VerificationType::ByteBuffer => &VerificationType::Byte,
                VerificationType::List(element) => element,
                _ => unreachable!(),
            };
            let element = value_layout(registry, target, element_ty);
            finish(
                id,
                header,
                end,
                header.align,
                PhysicalKind::Buffer {
                    data_offset,
                    length_offset,
                    capacity_offset,
                    element,
                },
                DropPlan::Buffer {
                    element,
                    mutable: matches!(ty, VerificationType::ByteBuffer),
                },
            )
        }
        VerificationType::Remote(_) | VerificationType::Future(_) => {
            let handle_offset = align_up(header.size, pointer.align)?;
            let value_descriptor_offset = checked_add(handle_offset, pointer.size)?;
            let end = checked_add(value_descriptor_offset, pointer.size)?;
            finish(
                id,
                header,
                end,
                header.align,
                PhysicalKind::Handle {
                    handle_offset,
                    value_descriptor_offset,
                },
                DropPlan::Runtime,
            )
        }
        VerificationType::Function { .. } => {
            let code_offset = align_up(header.size, pointer.align)?;
            let environment_offset = checked_add(code_offset, pointer.size)?;
            let release_offset = checked_add(environment_offset, pointer.size)?;
            let end = checked_add(release_offset, pointer.size)?;
            finish(
                id,
                header,
                end,
                header.align,
                PhysicalKind::Callable {
                    code_offset,
                    environment_offset,
                    release_offset,
                },
                DropPlan::Runtime,
            )
        }
        _ => Err(LayoutError::new(format!(
            "l{} has unsupported builtin type {ty:?}",
            id.0
        ))),
    }
}

fn opaque_layout(
    id: LayoutId,
    target: TargetLayout,
    header: ObjectHeader,
) -> Result<PhysicalLayout, LayoutError> {
    let value_size = 8;
    let value_align = target
        .integer64_align()
        .max(target.float64_align())
        .max(u16::from(target.pointer_align));
    let value_offset = align_up(header.size, value_align)?;
    let release_offset = align_up(
        checked_add(value_offset, value_size)?,
        u16::from(target.pointer_align),
    )?;
    finish(
        id,
        header,
        checked_add(release_offset, u32::from(target.pointer_size))?,
        header.align.max(value_align),
        PhysicalKind::Opaque {
            value_offset,
            release_offset,
            value_size,
            value_align,
        },
        DropPlan::Runtime,
    )
}

fn finish(
    id: LayoutId,
    header: ObjectHeader,
    end: u32,
    align: u16,
    kind: PhysicalKind,
    drop_plan: DropPlan,
) -> Result<PhysicalLayout, LayoutError> {
    let align = align.max(header.align);
    Ok(PhysicalLayout {
        id,
        materialized: true,
        size: align_up(end, align)?,
        align,
        header,
        kind,
        drop_plan,
    })
}

fn place_fields(
    registry: &Registry,
    target: TargetLayout,
    start: u32,
    slots: &[Slot],
) -> Result<(Vec<FieldLayout>, u32, u16), LayoutError> {
    let mut offset = start;
    let mut aggregate_align = 1;
    let mut fields = Vec::with_capacity(slots.len());
    for slot in slots {
        let value = value_layout(registry, target, &slot.ty);
        aggregate_align = aggregate_align.max(value.align);
        offset = align_up(offset, value.align)?;
        fields.push(FieldLayout {
            index: slot.index,
            name: slot.name.clone(),
            offset,
            value,
            ownership: slot.ownership,
        });
        offset = checked_add(offset, value.size)?;
    }
    Ok((fields, offset, aggregate_align))
}

fn value_layout(registry: &Registry, target: TargetLayout, ty: &VerificationType) -> ValueLayout {
    match registry.legal_type(ty) {
        LegalType::I8 => ValueLayout {
            size: 1,
            align: 1,
            kind: ScalarKind::I8,
            pointee: None,
        },
        LegalType::I32 => ValueLayout {
            size: 4,
            align: 4,
            kind: ScalarKind::I32,
            pointee: None,
        },
        LegalType::I64 => ValueLayout {
            size: 8,
            align: target.integer64_align(),
            kind: ScalarKind::I64,
            pointee: None,
        },
        LegalType::F64 => ValueLayout {
            size: 8,
            align: target.float64_align(),
            kind: ScalarKind::F64,
            pointee: None,
        },
        LegalType::Pointer { layout, .. } => pointer_value(target, layout),
        LegalType::Opaque => pointer_value(target, Some(registry.opaque())),
        LegalType::UnresolvedGeneric => pointer_value(target, None),
    }
}

fn pointer_value(target: TargetLayout, pointee: Option<LayoutId>) -> ValueLayout {
    ValueLayout {
        size: u32::from(target.pointer_size),
        align: u16::from(target.pointer_align),
        kind: ScalarKind::Pointer,
        pointee,
    }
}

fn drop_plan(fields: &[FieldLayout]) -> DropPlan {
    let fields = drop_fields(fields);
    if fields.is_empty() {
        DropPlan::Trivial
    } else {
        DropPlan::Fields(fields)
    }
}

fn drop_fields(fields: &[FieldLayout]) -> Vec<DropField> {
    fields
        .iter()
        .filter_map(|field| {
            field.value.pointee.map(|pointee| DropField {
                offset: field.offset,
                pointee,
                ownership: field.ownership,
            })
        })
        .filter(|field| field.ownership != Ownership::Borrowed)
        .collect()
}

fn fields_of(kind: &PhysicalKind) -> Vec<&FieldLayout> {
    match kind {
        PhysicalKind::Record { fields } => fields.iter().collect(),
        PhysicalKind::Variant { alternatives, .. } => alternatives
            .iter()
            .flat_map(|alternative| &alternative.fields)
            .collect(),
        PhysicalKind::Closure { captures, .. } => captures.iter().collect(),
        _ => Vec::new(),
    }
}

fn validate_field(
    owner: &PhysicalLayout,
    field: &FieldLayout,
    layout_count: usize,
) -> Result<(), LayoutError> {
    if !field.offset.is_multiple_of(u32::from(field.value.align)) {
        return Err(LayoutError::new(format!(
            "layout l{} field `{}` is misaligned",
            owner.id.0, field.name
        )));
    }
    if checked_add(field.offset, field.value.size)? > owner.size {
        return Err(LayoutError::new(format!(
            "layout l{} field `{}` is out of bounds",
            owner.id.0, field.name
        )));
    }
    if field
        .value
        .pointee
        .is_some_and(|id| id.0 as usize >= layout_count)
    {
        return Err(LayoutError::new(format!(
            "layout l{} field `{}` references a missing layout",
            owner.id.0, field.name
        )));
    }
    Ok(())
}

fn validate_kind(layout: &PhysicalLayout, target: TargetLayout) -> Result<(), LayoutError> {
    let pointer_size = u32::from(target.pointer_size);
    let pointer_align = u16::from(target.pointer_align);
    let word = |offset, label| validate_slot(layout, offset, pointer_size, pointer_align, label);
    match &layout.kind {
        PhysicalKind::Record { .. } => Ok(()),
        PhysicalKind::Variant {
            tag_offset,
            payload_offset,
            payload_size,
            payload_align,
            ..
        } => {
            validate_slot(layout, *tag_offset, 4, 4, "variant tag")?;
            validate_slot(
                layout,
                *payload_offset,
                *payload_size,
                *payload_align,
                "variant payload",
            )
        }
        PhysicalKind::Closure {
            code_offset,
            signature_offset,
            ..
        } => {
            word(*code_offset, "closure code")?;
            word(*signature_offset, "closure signature")
        }
        PhysicalKind::Place {
            origin_offset,
            path_offset,
            path_len_offset,
            projection,
        } => {
            word(*origin_offset, "place origin")?;
            word(*path_offset, "place path")?;
            word(*path_len_offset, "place path length")?;
            if projection.align == 0
                || !projection.align.is_power_of_two()
                || !projection
                    .operand_offset
                    .is_multiple_of(u32::from(pointer_align))
                || !projection
                    .root_generation_offset
                    .is_multiple_of(u32::from(target.integer64_align()))
                || !projection
                    .projected_generation_offset
                    .is_multiple_of(u32::from(target.integer64_align()))
                || checked_add(projection.kind_offset, 1)? > projection.size
                || checked_add(projection.operand_offset, pointer_size)? > projection.size
                || checked_add(projection.root_generation_offset, 8)? > projection.size
                || checked_add(projection.projected_generation_offset, 8)? > projection.size
            {
                return Err(LayoutError::new(format!(
                    "layout l{} has an invalid projection entry layout",
                    layout.id.0
                )));
            }
            Ok(())
        }
        PhysicalKind::Bytes {
            data_offset,
            length_offset,
        } => {
            word(*data_offset, "bytes data")?;
            word(*length_offset, "bytes length")
        }
        PhysicalKind::Buffer {
            data_offset,
            length_offset,
            capacity_offset,
            ..
        } => {
            word(*data_offset, "buffer data")?;
            word(*length_offset, "buffer length")?;
            word(*capacity_offset, "buffer capacity")
        }
        PhysicalKind::Handle {
            handle_offset,
            value_descriptor_offset,
        } => {
            word(*handle_offset, "runtime handle")?;
            word(*value_descriptor_offset, "runtime value descriptor")
        }
        PhysicalKind::Callable {
            code_offset,
            environment_offset,
            release_offset,
        } => {
            word(*code_offset, "callable code")?;
            word(*environment_offset, "callable environment")?;
            word(*release_offset, "callable environment release")
        }
        PhysicalKind::Opaque {
            value_offset,
            release_offset,
            value_size,
            value_align,
        } => {
            validate_slot(
                layout,
                *value_offset,
                *value_size,
                *value_align,
                "opaque payload",
            )?;
            word(*release_offset, "opaque payload release")
        }
    }
}

fn validate_slot(
    owner: &PhysicalLayout,
    offset: u32,
    size: u32,
    align: u16,
    label: &str,
) -> Result<(), LayoutError> {
    if align == 0
        || !align.is_power_of_two()
        || !offset.is_multiple_of(u32::from(align))
        || checked_add(offset, size)? > owner.size
    {
        return Err(LayoutError::new(format!(
            "layout l{} has an invalid {label} slot",
            owner.id.0
        )));
    }
    Ok(())
}

fn checked_add(left: u32, right: u32) -> Result<u32, LayoutError> {
    left.checked_add(right)
        .ok_or_else(|| LayoutError::new("physical layout size overflow"))
}

fn align_up(value: u32, align: u16) -> Result<u32, LayoutError> {
    let align = u32::from(align);
    let mask = align
        .checked_sub(1)
        .ok_or_else(|| LayoutError::new("zero physical alignment"))?;
    value
        .checked_add(mask)
        .map(|value| value & !mask)
        .ok_or_else(|| LayoutError::new("physical layout alignment overflow"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::layout::{Alternative, Layout, LayoutKind};
    use crate::hir::{Record, Variant, VariantType};
    use la_arena::{Idx, RawIdx};

    fn id<T>(raw: u32) -> Idx<T> {
        Idx::from_raw(RawIdx::from_u32(raw))
    }

    fn registry(kinds: Vec<LayoutKind>) -> Registry {
        let mut registry = Registry::default();
        for kind in kinds {
            let id = LayoutId(registry.layouts.len() as u32);
            registry.layouts.push(Layout {
                id,
                boxed: true,
                materialized: true,
                kind,
            });
        }
        registry.opaque = Some(LayoutId(0));
        registry
    }

    #[test]
    fn record_offsets_are_target_aware_and_aligned() {
        let registry = registry(vec![
            LayoutKind::Opaque,
            LayoutKind::Record {
                record: id::<Record>(0),
                fields: vec![
                    Slot {
                        index: 0,
                        name: "flag".into(),
                        ty: VerificationType::Bool,
                        ownership: Ownership::Owned,
                    },
                    Slot {
                        index: 1,
                        name: "count".into(),
                        ty: VerificationType::Integer,
                        ownership: Ownership::Owned,
                    },
                    Slot {
                        index: 2,
                        name: "erased".into(),
                        ty: VerificationType::Unknown,
                        ownership: Ownership::Shared,
                    },
                ],
            },
        ]);
        let physical =
            PhysicalRegistry::build(&registry, TargetLayout::new(8, 8).unwrap()).unwrap();
        let record = physical.get(LayoutId(1));
        let PhysicalKind::Record { fields } = &record.kind else {
            panic!();
        };
        assert_eq!(record.header.size, 24);
        assert_eq!(fields[0].offset, 24);
        assert_eq!(fields[1].offset, 32);
        assert_eq!(fields[2].offset, 40);
        assert_eq!(record.size, 48);
        assert!(matches!(record.drop_plan, DropPlan::Fields(_)));
    }

    #[test]
    fn thirty_two_bit_layout_uses_four_byte_pointer_slots() {
        let registry = registry(vec![
            LayoutKind::Opaque,
            LayoutKind::Pointer {
                pointee: VerificationType::Integer,
                ownership: Ownership::Borrowed,
            },
        ]);
        let physical =
            PhysicalRegistry::build(&registry, TargetLayout::new(4, 4).unwrap()).unwrap();
        let place = physical.get(LayoutId(1));
        assert_eq!(place.header.size, 12);
        assert_eq!(place.align, 4);
        assert_eq!(place.size, 24);
        let PhysicalKind::Place { projection, .. } = &place.kind else {
            panic!();
        };
        assert_eq!(projection.size, 24);
    }

    #[test]
    fn scalar_alignment_is_independent_of_pointer_alignment() {
        let registry = registry(vec![
            LayoutKind::Opaque,
            LayoutKind::Record {
                record: id::<Record>(0),
                fields: vec![
                    Slot {
                        index: 0,
                        name: "flag".into(),
                        ty: VerificationType::Bool,
                        ownership: Ownership::Owned,
                    },
                    Slot {
                        index: 1,
                        name: "wide".into(),
                        ty: VerificationType::Integer,
                        ownership: Ownership::Owned,
                    },
                ],
            },
        ]);
        let target = TargetLayout::with_scalar_alignments(4, 4, 8, 4).unwrap();
        let physical = PhysicalRegistry::build(&registry, target).unwrap();
        let PhysicalKind::Record { fields } = &physical.get(LayoutId(1)).kind else {
            panic!();
        };
        assert_eq!(fields[0].offset, 12);
        assert_eq!(fields[1].offset, 16);
        assert_eq!(physical.get(LayoutId(1)).align, 8);
    }

    #[test]
    fn variant_payload_starts_after_a_fixed_tag() {
        let registry = registry(vec![
            LayoutKind::Opaque,
            LayoutKind::Variant {
                variant_type: id::<VariantType>(0),
                alternatives: vec![
                    Alternative {
                        variant: id::<Variant>(0),
                        tag: 0,
                        name: "None".into(),
                        payload: vec![],
                    },
                    Alternative {
                        variant: id::<Variant>(1),
                        tag: 1,
                        name: "Some".into(),
                        payload: vec![VerificationType::Integer],
                    },
                ],
            },
        ]);
        let physical =
            PhysicalRegistry::build(&registry, TargetLayout::new(8, 8).unwrap()).unwrap();
        let PhysicalKind::Variant {
            tag_offset,
            payload_offset,
            alternatives,
            ..
        } = &physical.get(LayoutId(1)).kind
        else {
            panic!();
        };
        assert_eq!(*tag_offset, 24);
        assert_eq!(*payload_offset, 32);
        assert_eq!(alternatives[1].fields[0].offset, 32);
    }

    #[test]
    fn descriptors_are_deterministic_and_include_field_offsets() {
        let registry = registry(vec![
            LayoutKind::Opaque,
            LayoutKind::Record {
                record: id::<Record>(0),
                fields: vec![Slot {
                    index: 0,
                    name: "value".into(),
                    ty: VerificationType::Integer,
                    ownership: Ownership::Owned,
                }],
            },
        ]);
        let physical =
            PhysicalRegistry::build(&registry, TargetLayout::new(8, 8).unwrap()).unwrap();
        let layout = physical.get(LayoutId(1));
        let first = layout.descriptor_bytes();
        assert_eq!(first, layout.descriptor_bytes());
        assert_eq!(&first[..4], b"FLYT");
        assert!(first.windows(4).any(|bytes| bytes == 24_u32.to_le_bytes()));
    }
}
