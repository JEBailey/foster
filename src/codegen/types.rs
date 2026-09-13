//! Logical executable types shared by SSA, specialization, layout selection, and bytecode verification.
//!
//! Semantic checking determines source-level conformance before conversion into this model.
//! These types retain representation-relevant shape without selecting a target ABI, byte size,
//! alignment, or field offset. Those decisions belong to layout legalization and physical lowering.
//! Nominal IDs identify declarations within the current compilation or relocated program; they are
//! not stable cross-package symbols. Conversion and erasure policies live in `type_conversion`.

use std::collections::HashMap;

use crate::hir::{RecordId, VariantTypeId};

/// Named executable-type substitutions for a call or specialized function instance.
///
/// Construction enforces unique names in ascending order. Compiler constructors canonicalize
/// ordering; bytecode decoding rejects noncanonical input. The read-only entry view and checked
/// rename operation preserve this invariant for binary search and cache keys. Values may contain `Generic` leaves
/// while an enclosing specialization is unresolved; consumers needing materialized layouts must
/// resolve or reject those leaves. Nominal identities are local to the current compilation/program.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Specialization(Vec<(String, ExecutableType)>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidSpecialization;

impl std::fmt::Display for InvalidSpecialization {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("unsorted or duplicate generic substitutions")
    }
}

impl std::error::Error for InvalidSpecialization {}

impl Specialization {
    pub fn new() -> Self {
        Self::default()
    }

    /// Canonicalize compiler-produced entries, rejecting duplicate names even if values agree.
    pub fn try_new(
        mut entries: Vec<(String, ExecutableType)>,
    ) -> Result<Self, InvalidSpecialization> {
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        Self::try_from_sorted(entries)
    }

    /// Validate serialized entries without repairing a noncanonical encoding.
    pub fn try_from_sorted(
        entries: Vec<(String, ExecutableType)>,
    ) -> Result<Self, InvalidSpecialization> {
        if entries.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
            return Err(InvalidSpecialization);
        }
        Ok(Self(entries))
    }

    /// Transform values while preserving the validated names and ordering.
    pub fn map_values(&self, mut map: impl FnMut(&ExecutableType) -> ExecutableType) -> Self {
        Self(
            self.0
                .iter()
                .map(|(name, ty)| (name.clone(), map(ty)))
                .collect(),
        )
    }

    /// Rename atomically; a collision leaves this specialization unchanged.
    pub fn rename(
        &mut self,
        mut name: impl FnMut(&str) -> String,
    ) -> Result<(), InvalidSpecialization> {
        let renamed = Self::try_new(
            self.0
                .iter()
                .map(|(key, ty)| (name(key), ty.clone()))
                .collect(),
        )?;
        *self = renamed;
        Ok(())
    }

    pub(crate) fn values_mut(&mut self) -> impl Iterator<Item = &mut ExecutableType> {
        self.0.iter_mut().map(|(_, ty)| ty)
    }
}

impl From<std::collections::BTreeMap<String, ExecutableType>> for Specialization {
    fn from(entries: std::collections::BTreeMap<String, ExecutableType>) -> Self {
        Self(entries.into_iter().collect())
    }
}

impl std::ops::Deref for Specialization {
    type Target = [(String, ExecutableType)];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'a> IntoIterator for &'a Specialization {
    type Item = &'a (String, ExecutableType);
    type IntoIter = std::slice::Iter<'a, (String, ExecutableType)>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

/// Executable type information shared by bytecode verification and backend layout selection.
///
/// Groups and effects are omitted after semantic checking; callable ownership modes, generic
/// identities, and nominal arguments remain. This is a logical schema, not a source conformance
/// proof or a physical layout: the same schema can have different backend representations.
/// Backend-specific conversion/erasure policies decide which information survives lowering.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExecutableType {
    /// Unavailable or erased shape. This is the bytecode verifier's top type and maps to an
    /// opaque native representation. Availability and ownership checks still apply; this value
    /// does not establish source-level conformance or promise a concrete aggregate layout.
    Unknown,
    /// Named schema placeholder. Allowed in intermediate types, but must be resolved or handled
    /// explicitly before a consumer emits a materialized specialization/layout.
    Generic(String),
    Unit,
    Bool,
    Integer,
    Float,
    CodePoint,
    Byte,
    Bytes,
    ByteBuffer,
    List(Box<ExecutableType>),
    Reference(Box<ExecutableType>),
    Remote(Box<ExecutableType>),
    Future(Box<ExecutableType>),
    /// Callable shape with ownership attached to each parameter.
    Function {
        parameters: Vec<crate::types::Parameter<ExecutableType>>,
        result: Box<ExecutableType>,
    },
    Record {
        record: RecordId,
        arguments: Vec<ExecutableType>,
    },
    Variant {
        variant: VariantTypeId,
        arguments: Vec<ExecutableType>,
    },
    /// Possible types of a value at a control-flow join. Every found alternative must satisfy
    /// an expected type; an expected alternatives set accepts any matching member.
    /// Canonical sets are sorted, unique, flattened, and contain at least two members.
    Alternatives(Vec<ExecutableType>),
    /// Simultaneous structural requirements retained by native conversion. These unordered
    /// members are metadata behind an opaque representation, not possible runtime alternatives.
    /// Bytecode conversion erases this view; it cannot appear in portable bytecode metadata.
    Intersection(Vec<ExecutableType>),
    /// Positional generic arguments retained for an unresolved alias by native conversion.
    /// These are not the alias's target or alternatives. Order and repeated arguments matter.
    /// Bytecode conversion erases this metadata; native layout remains opaque.
    AliasArguments {
        alias: VariantTypeId,
        arguments: Vec<ExecutableType>,
    },
}

impl ExecutableType {
    pub(crate) fn canonical_alternatives(members: &[Self]) -> bool {
        members.len() >= 2
            && members.windows(2).all(|pair| pair[0] < pair[1])
            && members
                .iter()
                .all(|member| !matches!(member, Self::Unknown | Self::Alternatives(_)))
    }

    /// Canonicalize control-flow alternatives, with Unknown as the top type.
    pub fn alternatives(members: Vec<Self>) -> Self {
        let mut flattened = Vec::new();
        for member in members {
            match member {
                Self::Unknown => return Self::Unknown,
                Self::Alternatives(nested) => match Self::alternatives(nested) {
                    Self::Unknown => return Self::Unknown,
                    Self::Alternatives(nested) => flattened.extend(nested),
                    other => flattened.push(other),
                },
                other => flattened.push(other),
            }
        }
        flattened.sort();
        flattened.dedup();
        match flattened.len() {
            0 => Self::Unknown,
            1 => flattened.pop().unwrap(),
            _ => Self::Alternatives(flattened),
        }
    }

    /// Canonicalize simultaneous requirements without collapsing them into a concrete type.
    pub fn intersection(members: Vec<Self>) -> Self {
        let mut flattened = Vec::new();
        for member in members {
            match member {
                Self::Intersection(nested) => {
                    let Self::Intersection(nested) = Self::intersection(nested) else {
                        unreachable!()
                    };
                    flattened.extend(nested);
                }
                other => flattened.push(other),
            }
        }
        flattened.sort();
        flattened.dedup();
        Self::Intersection(flattened)
    }
    /// Collect generic bindings from an operand's retained shape. This does not check
    /// conformance: callers establish compatible shapes separately, and the first binding wins.
    pub(crate) fn infer_specialization(
        &self,
        actual: &Self,
        substitutions: &mut std::collections::BTreeMap<String, Self>,
    ) {
        match (self, actual) {
            (Self::Generic(name), actual) => {
                substitutions
                    .entry(name.clone())
                    .or_insert_with(|| actual.clone());
            }
            (Self::List(schema), Self::List(actual))
            | (Self::Reference(schema), Self::Reference(actual))
            | (Self::Remote(schema), Self::Remote(actual))
            | (Self::Future(schema), Self::Future(actual)) => {
                schema.infer_specialization(actual, substitutions)
            }
            (
                Self::Record {
                    record: left,
                    arguments: schema,
                },
                Self::Record {
                    record: right,
                    arguments: actual,
                },
            ) if left == right => {
                for (schema, actual) in schema.iter().zip(actual) {
                    schema.infer_specialization(actual, substitutions);
                }
            }
            (
                Self::Variant {
                    variant: left,
                    arguments: schema,
                },
                Self::Variant {
                    variant: right,
                    arguments: actual,
                },
            ) if left == right => {
                for (schema, actual) in schema.iter().zip(actual) {
                    schema.infer_specialization(actual, substitutions);
                }
            }
            (
                Self::Function {
                    parameters: schema,
                    result: schema_result,
                    ..
                },
                Self::Function {
                    parameters: actual,
                    result: actual_result,
                    ..
                },
            ) => {
                for (schema, actual) in schema.iter().zip(actual) {
                    schema.ty.infer_specialization(&actual.ty, substitutions);
                }
                schema_result.infer_specialization(actual_result, substitutions);
            }
            _ => {}
        }
    }

    pub(crate) fn indexed_element(&self) -> Option<Self> {
        match self {
            Self::Reference(pointee) => pointee.indexed_element(),
            Self::List(element) => Some((**element).clone()),
            Self::ByteBuffer => Some(Self::Byte),
            Self::Unknown | Self::Generic(_) => Some(Self::Unknown),
            _ => None,
        }
    }

    pub(crate) fn depth(&self) -> usize {
        match self {
            Self::List(value)
            | Self::Reference(value)
            | Self::Remote(value)
            | Self::Future(value) => 1 + value.depth(),
            Self::Function {
                parameters, result, ..
            } => {
                1 + parameters
                    .iter()
                    .map(|p| &p.ty)
                    .chain(std::iter::once(result.as_ref()))
                    .map(Self::depth)
                    .max()
                    .unwrap_or(0)
            }
            Self::Record { arguments, .. }
            | Self::Variant { arguments, .. }
            | Self::Alternatives(arguments)
            | Self::Intersection(arguments)
            | Self::AliasArguments { arguments, .. } => {
                1 + arguments.iter().map(Self::depth).max().unwrap_or(0)
            }
            _ => 1,
        }
    }

    pub(crate) fn contains_generic(&self) -> bool {
        match self {
            Self::Generic(_) => true,
            Self::List(value)
            | Self::Reference(value)
            | Self::Remote(value)
            | Self::Future(value) => value.contains_generic(),
            Self::Function {
                parameters, result, ..
            } => parameters.iter().any(|p| p.ty.contains_generic()) || result.contains_generic(),
            Self::Record { arguments, .. }
            | Self::Variant { arguments, .. }
            | Self::Alternatives(arguments)
            | Self::Intersection(arguments)
            | Self::AliasArguments { arguments, .. } => {
                arguments.iter().any(Self::contains_generic)
            }
            _ => false,
        }
    }

    /// Replace generic leaves using a named substitution map.
    pub(crate) fn substitute(&self, substitutions: &HashMap<String, ExecutableType>) -> Self {
        self.substitute_with(&|name| substitutions.get(name).cloned())
    }

    /// Replace generic leaves using a sorted specialization shared by both backends.
    /// Unbound names remain generic, and replacement values are copied without recursively
    /// applying the same substitutions to them.
    pub(crate) fn specialize(&self, substitutions: &Specialization) -> Self {
        self.substitute_with(&|name| {
            substitutions
                .binary_search_by(|(candidate, _)| candidate.as_str().cmp(name))
                .ok()
                .map(|index| substitutions[index].1.clone())
        })
    }

    fn substitute_with(&self, lookup: &impl Fn(&str) -> Option<Self>) -> Self {
        match self {
            Self::Generic(name) => lookup(name).unwrap_or_else(|| self.clone()),
            Self::List(value) => Self::List(Box::new(value.substitute_with(lookup))),
            Self::Reference(value) => Self::Reference(Box::new(value.substitute_with(lookup))),
            Self::Remote(value) => Self::Remote(Box::new(value.substitute_with(lookup))),
            Self::Future(value) => Self::Future(Box::new(value.substitute_with(lookup))),
            Self::Function { parameters, result } => Self::Function {
                parameters: parameters
                    .iter()
                    .map(|p| crate::types::Parameter {
                        ty: p.ty.substitute_with(lookup),
                        mode: p.mode,
                    })
                    .collect(),
                result: Box::new(result.substitute_with(lookup)),
            },
            Self::Record { record, arguments } => Self::Record {
                record: *record,
                arguments: arguments
                    .iter()
                    .map(|ty| ty.substitute_with(lookup))
                    .collect(),
            },
            Self::Variant { variant, arguments } => Self::Variant {
                variant: *variant,
                arguments: arguments
                    .iter()
                    .map(|ty| ty.substitute_with(lookup))
                    .collect(),
            },
            Self::Alternatives(members) => Self::alternatives(
                members
                    .iter()
                    .map(|ty| ty.substitute_with(lookup))
                    .collect(),
            ),
            Self::Intersection(members) => Self::intersection(
                members
                    .iter()
                    .map(|ty| ty.substitute_with(lookup))
                    .collect(),
            ),
            Self::AliasArguments { alias, arguments } => Self::AliasArguments {
                alias: *alias,
                arguments: arguments
                    .iter()
                    .map(|ty| ty.substitute_with(lookup))
                    .collect(),
            },
            _ => self.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alternatives_and_intersections_have_distinct_identity_and_normalization() {
        use ExecutableType as T;
        let members = vec![T::Integer, T::Bool, T::Integer];
        let alternatives = T::alternatives(members.clone());
        let intersection = T::intersection(members);
        assert_ne!(alternatives, intersection);
        assert_eq!(alternatives, T::Alternatives(vec![T::Bool, T::Integer]));
        assert_eq!(intersection, T::Intersection(vec![T::Bool, T::Integer]));
        assert_eq!(
            T::intersection(vec![T::Integer, intersection.clone()]),
            intersection
        );
        assert_eq!(
            T::alternatives(vec![T::Integer, alternatives.clone()]),
            alternatives
        );
        assert_eq!(T::alternatives(vec![T::Integer, T::Unknown]), T::Unknown);
        assert_eq!(
            T::intersection(vec![T::Unknown]),
            T::Intersection(vec![T::Unknown])
        );
        assert_eq!(
            T::intersection(vec![T::Integer]),
            T::Intersection(vec![T::Integer])
        );
    }

    #[test]
    fn specialization_recanonicalizes_sets_but_preserves_alias_positions() {
        use ExecutableType as T;
        let alias = VariantTypeId::from_raw(la_arena::RawIdx::from_u32(0));
        let arguments = vec![
            T::Generic("A".into()),
            T::Generic("Z".into()),
            T::Generic("A".into()),
        ];
        let substitutions = HashMap::from([("A".into(), T::Integer), ("Z".into(), T::Bool)]);
        assert_eq!(
            T::alternatives(arguments.clone()).substitute(&substitutions),
            T::Alternatives(vec![T::Bool, T::Integer])
        );
        assert_eq!(
            T::intersection(arguments.clone()).substitute(&substitutions),
            T::Intersection(vec![T::Bool, T::Integer])
        );
        let metadata = T::AliasArguments { alias, arguments };
        assert!(metadata.contains_generic());
        assert_eq!(metadata.depth(), 2);
        assert_eq!(
            metadata.substitute(&substitutions),
            T::AliasArguments {
                alias,
                arguments: vec![T::Integer, T::Bool, T::Integer]
            }
        );
        let same = HashMap::from([("A".into(), T::Bool), ("Z".into(), T::Bool)]);
        assert_eq!(
            T::alternatives(vec![T::Generic("A".into()), T::Generic("Z".into())]).substitute(&same),
            T::Bool
        );
    }

    #[test]
    fn specialization_canonicalizes_identity_and_rejects_duplicate_names() {
        let entries = vec![
            ("Z".into(), ExecutableType::Bool),
            ("A".into(), ExecutableType::Integer),
        ];
        let specialization = Specialization::try_new(entries.clone()).unwrap();
        let canonical =
            Specialization::try_from_sorted(entries.into_iter().rev().collect()).unwrap();
        assert_eq!(specialization, canonical);
        assert_eq!(
            std::collections::HashSet::from([specialization.clone(), canonical]).len(),
            1
        );
        assert_eq!(
            ExecutableType::Generic("A".into()).specialize(&specialization),
            ExecutableType::Integer
        );
        for value in [ExecutableType::Integer, ExecutableType::Bool] {
            assert!(
                Specialization::try_new(vec![
                    ("T".into(), ExecutableType::Integer),
                    ("T".into(), value)
                ])
                .is_err()
            );
        }
    }

    #[test]
    fn specialization_rename_is_atomic_and_restores_order() {
        let mut specialization = Specialization::try_new(vec![
            ("A".into(), ExecutableType::Integer),
            ("B".into(), ExecutableType::Bool),
        ])
        .unwrap();
        let original = specialization.clone();
        assert!(specialization.rename(|_| "T".into()).is_err());
        assert_eq!(specialization, original);
        specialization
            .rename(|name| if name == "A" { "Z".into() } else { "A".into() })
            .unwrap();
        assert_eq!(specialization[0], ("A".into(), ExecutableType::Bool));
        assert_eq!(specialization[1], ("Z".into(), ExecutableType::Integer));
        assert_eq!(
            specialization
                .map_values(|_| ExecutableType::Unit)
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            vec!["A", "Z"]
        );
    }

    #[test]
    fn substitutions_walk_nested_types_consistently() {
        let generic = ExecutableType::Function {
            parameters: crate::types::Parameter::from_parts(
                vec![ExecutableType::List(Box::new(ExecutableType::Generic(
                    "T".into(),
                )))],
                vec![crate::ast::ParameterMode::Borrow],
            ),
            result: Box::new(ExecutableType::Reference(Box::new(
                ExecutableType::Generic("T".into()),
            ))),
        };
        let expected = ExecutableType::Function {
            parameters: crate::types::Parameter::from_parts(
                vec![ExecutableType::List(Box::new(ExecutableType::Integer))],
                vec![crate::ast::ParameterMode::Borrow],
            ),
            result: Box::new(ExecutableType::Reference(Box::new(ExecutableType::Integer))),
        };
        let map = HashMap::from([("T".into(), ExecutableType::Integer)]);
        let specialization =
            Specialization::try_new(vec![("T".into(), ExecutableType::Integer)]).unwrap();

        assert_eq!(generic.substitute(&map), expected);
        assert_eq!(generic.specialize(&specialization), expected);
        assert_eq!(generic.depth(), 3);
        assert!(generic.contains_generic());
        assert!(!expected.contains_generic());
    }
}
