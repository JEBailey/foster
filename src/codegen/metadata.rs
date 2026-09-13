//! Backend-neutral executable program data.
//!
//! Logical nominal schemas, constants, symbolic identities, dispatch targets, and entry-point
//! conventions are shared by execution backends. This module contains no bytecode bodies,
//! register state, drop-insertion flags, VM values, or target-specific physical layouts.
//! IDs and constant indices are local to a program and are remapped by linking.

use crate::codegen::types::ExecutableType;
use crate::hir::{FunctionId, RecordId, VariantId, VariantTypeId};
use crate::types::{DispatchSlot, NominalTypeId};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq)]
pub enum Constant {
    Unit,
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(String),
    CodePoint(char),
    Symbol(String),
}

/// Shared logical data. Cross-references are checked with the executable bodies at sealing/linking.
/// A sealed program exposes this through an immutable reference.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProgramMetadata {
    pub symbols: crate::symbols::Table,
    pub constants: Vec<Constant>,
    pub main: Option<FunctionId>,
    /// Whether `main` receives one `std.process.Arguments` value.
    pub main_arguments: bool,
    pub string_record: Option<RecordId>,
    pub symbol_record: Option<RecordId>,
    pub list_record: Option<RecordId>,
    pub bytes_record: Option<RecordId>,
    pub byte_buffer_record: Option<RecordId>,
    pub remote_result: Option<VariantTypeId>,
    pub remote_error: Option<VariantTypeId>,
    pub records: HashMap<RecordId, RuntimeRecord>,
    pub dispatch: HashMap<(NominalTypeId, DispatchSlot), FunctionId>,
    pub variants: HashMap<VariantId, RuntimeVariant>,
}

/// Field names and types stay paired; the lookup layout is derived at construction.
///
/// ```compile_fail
/// use foster::codegen::metadata::RuntimeRecord;
/// fn invalidate(record: &mut RuntimeRecord) {
///     record.fields().clear();
/// }
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeRecord {
    pub name: String,
    /// Generic parameters in declaration order.
    pub parameters: Vec<String>,
    layout: Arc<RecordLayout>,
    fields: Vec<RecordField>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecordField {
    pub name: String,
    pub ty: ExecutableType,
}

impl RuntimeRecord {
    /// Preserve declared storage order and reject ambiguous field-name lookup.
    pub fn new(
        name: String,
        parameters: Vec<String>,
        fields: Vec<RecordField>,
    ) -> Result<Self, crate::error::FosterError> {
        let layout = RecordLayout::new(fields.iter().map(|field| field.name.clone()).collect());
        if layout.indices.len() != fields.len() {
            return Err(crate::error::FosterError::runtime(format!(
                "record `{name}` has duplicate field names"
            )));
        }
        Ok(Self {
            name,
            parameters,
            layout: Arc::new(layout),
            fields,
        })
    }

    pub fn fields(&self) -> &[RecordField] {
        &self.fields
    }
    pub fn layout(&self) -> &Arc<RecordLayout> {
        &self.layout
    }

    /// Linking may remap types without changing names or storage positions.
    pub fn map_field_types(&mut self, mut map: impl FnMut(&mut ExecutableType)) {
        for field in &mut self.fields {
            map(&mut field.ty);
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeVariant {
    pub parent: VariantTypeId,
    pub type_name: Arc<str>,
    /// Generic parameters of the parent enum in declaration order.
    pub parameters: Vec<String>,
    pub alternative: Arc<str>,
    /// Enum cases currently have zero or one declared payload value.
    pub payload: Vec<ExecutableType>,
}

/// Logical field order and name lookup, shared by VM storage and native layout selection.
/// This contains no physical offsets, allocation policy, or runtime values.
#[derive(Debug, PartialEq, Eq)]
pub struct RecordLayout {
    names: Vec<String>,
    indices: BTreeMap<String, usize>,
}

impl RecordLayout {
    pub(crate) fn new(names: Vec<String>) -> Self {
        let indices = names
            .iter()
            .enumerate()
            .map(|(index, name)| (name.clone(), index))
            .collect();
        Self { names, indices }
    }

    pub(crate) fn index(&self, name: &str) -> Option<usize> {
        self.indices.get(name).copied()
    }

    pub fn names(&self) -> &[String] {
        &self.names
    }
}

impl ProgramMetadata {
    pub(crate) fn remote_outcome_type(&self, result: ExecutableType) -> ExecutableType {
        ExecutableType::Variant {
            variant: self.remote_result.expect("verified remote Result metadata"),
            arguments: vec![
                result,
                ExecutableType::Variant {
                    variant: self.remote_error.expect("verified RemoteError metadata"),
                    arguments: vec![],
                },
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_fields_preserve_storage_order_and_names_when_types_are_remapped() {
        let mut record = RuntimeRecord::new(
            "Pair".into(),
            vec![],
            vec![
                RecordField {
                    name: "second".into(),
                    ty: ExecutableType::Generic("T".into()),
                },
                RecordField {
                    name: "first".into(),
                    ty: ExecutableType::Bool,
                },
            ],
        )
        .unwrap();
        let layout = record.layout().clone();
        record.map_field_types(|ty| {
            if matches!(ty, ExecutableType::Generic(_)) {
                *ty = ExecutableType::Integer;
            }
        });
        assert!(Arc::ptr_eq(&layout, record.layout()));
        assert_eq!(record.layout().names(), &["second", "first"]);
        for (index, field) in record.fields().iter().enumerate() {
            assert_eq!(record.layout().index(&field.name), Some(index));
        }
        assert_eq!(record.fields()[0].ty, ExecutableType::Integer);
        assert_eq!(record.fields()[1].ty, ExecutableType::Bool);
    }

    #[test]
    fn record_fields_reject_duplicate_names() {
        let field = RecordField {
            name: "value".into(),
            ty: ExecutableType::Integer,
        };
        assert!(
            RuntimeRecord::new("Duplicate".into(), vec![], vec![field.clone(), field])
                .unwrap_err()
                .to_string()
                .contains("duplicate field names")
        );
    }
}
