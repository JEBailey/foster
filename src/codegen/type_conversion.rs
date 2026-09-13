//! Semantic types to executable verification/layout types.
//!
//! The recursive walk is shared; bytecode and native lowering deliberately differ in how
//! much structural identity they retain. These policies preserve existing backend behavior.

use std::convert::Infallible;

use crate::ast::VariantKind;
use crate::hir::{PackageHir, RecordId};
use crate::types::{COPY_SLOT, DEINIT_SLOT, NominalTypeId, Type, TypeId, TypeInformation};
use crate::vm::{Specialization, VerificationType as V};

pub(crate) const MAX_TYPE_DEPTH: usize = 64;

pub(crate) enum StructuralViews {
    Erase,
    /// Keep member metadata behind the native opaque representation. This does not turn
    /// source intersections into union contracts or promise a concrete aggregate layout.
    RetainMembers,
}

pub(crate) enum RecordErasure {
    StatelessWithoutCapabilities,
    NestedDynamicContracts,
}

pub(crate) enum RemoteReceivers {
    PreserveEmptyNominal,
    ConvertNormally,
}

pub(crate) enum ListArguments {
    /// Bytecode historically accepts the first argument and erases a missing element.
    FirstOrUnknown,
    /// Native lowering recognizes the builtin only for a complete unary application.
    ExactlyOne,
}

pub(crate) trait Policy {
    type Error;
    const VIEWS: StructuralViews;
    const RECORDS: RecordErasure;
    const REMOTE_RECEIVERS: RemoteReceivers;
    const LIST_ARGUMENTS: ListArguments;
    fn nesting_limit() -> Result<V, Self::Error>;
}

pub(crate) struct Bytecode;

impl Policy for Bytecode {
    type Error = Infallible;
    const VIEWS: StructuralViews = StructuralViews::Erase;
    const RECORDS: RecordErasure = RecordErasure::StatelessWithoutCapabilities;
    const REMOTE_RECEIVERS: RemoteReceivers = RemoteReceivers::PreserveEmptyNominal;
    const LIST_ARGUMENTS: ListArguments = ListArguments::FirstOrUnknown;

    fn nesting_limit() -> Result<V, Self::Error> {
        Ok(V::Unknown)
    }
}

pub(crate) struct Native;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NestingLimit;

impl Policy for Native {
    type Error = NestingLimit;
    const VIEWS: StructuralViews = StructuralViews::RetainMembers;
    const RECORDS: RecordErasure = RecordErasure::NestedDynamicContracts;
    const REMOTE_RECEIVERS: RemoteReceivers = RemoteReceivers::ConvertNormally;
    const LIST_ARGUMENTS: ListArguments = ListArguments::ExactlyOne;

    fn nesting_limit() -> Result<V, Self::Error> {
        Err(NestingLimit)
    }
}

/// `depth` includes enclosing semantic types; native callers sometimes start at one for
/// aggregate arguments. Substitutions are already executable types and are copied as leaves.
/// Passing no substitutions retains named generics, as required by bytecode schemas.
pub(crate) fn convert<P: Policy>(
    hir: &PackageHir,
    information: &TypeInformation,
    ty: TypeId,
    substitutions: &Specialization,
    depth: usize,
) -> Result<V, P::Error> {
    if depth >= MAX_TYPE_DEPTH {
        return P::nesting_limit();
    }
    let nested = |ty| convert::<P>(hir, information, ty, substitutions, depth + 1);
    let arguments = |values: &[TypeId]| {
        values
            .iter()
            .copied()
            .map(nested)
            .collect::<Result<Vec<_>, _>>()
    };
    let view = |members: &[TypeId]| match P::VIEWS {
        StructuralViews::Erase => Ok(V::Unknown),
        StructuralViews::RetainMembers => Ok(V::Union(arguments(members)?)),
    };
    Ok(match &information.types[ty] {
        Type::Generic(name) => substitutions
            .iter()
            .find_map(|(candidate, ty)| (candidate == name).then(|| ty.clone()))
            .unwrap_or_else(|| V::Generic(name.clone())),
        Type::Unit => V::Unit,
        Type::Bool => V::Bool,
        Type::Int | Type::RawInt => V::Integer,
        Type::Float => V::Float,
        Type::CodePoint => V::CodePoint,
        Type::Byte => V::Byte,
        Type::RawBytes => V::Bytes,
        Type::RawByteBuffer => V::ByteBuffer,
        Type::Reference { value, .. } => V::Reference(Box::new(nested(*value)?)),
        Type::RawList(value) => V::List(Box::new(nested(*value)?)),
        // Behavioral views have no single promised runtime representation.
        Type::Sequence(_) | Type::Module(_) => V::Unknown,
        Type::Remote(value) => {
            let receiver = match (&information.types[*value], P::REMOTE_RECEIVERS) {
                (
                    Type::Record {
                        record,
                        arguments: values,
                    },
                    RemoteReceivers::PreserveEmptyNominal,
                ) if hir.records[*record].fields.is_empty() => {
                    // Preserve the receiver schema without adding a traversal level:
                    // this matches the bytecode remote-dispatch metadata contract.
                    V::Record {
                        record: *record,
                        arguments: arguments(values)?,
                    }
                }
                _ => nested(*value)?,
            };
            V::Remote(Box::new(receiver))
        }
        Type::Future(value) => V::Future(Box::new(nested(*value)?)),
        Type::Function(function) => V::Function {
            parameters: function
                .parameters
                .iter()
                .map(|p| nested(p.ty))
                .collect::<Result<_, _>>()?,
            parameter_modes: function.parameters.iter().map(|p| p.mode).collect(),
            result: Box::new(nested(function.result)?),
        },
        Type::Intersection(members) => view(members)?,
        Type::Record {
            record,
            arguments: values,
        } => {
            let builtin_list = Some(*record) == information.core.list
                && match P::LIST_ARGUMENTS {
                    ListArguments::FirstOrUnknown => true,
                    ListArguments::ExactlyOne => values.len() == 1,
                };
            if builtin_list {
                V::List(Box::new(
                    values
                        .first()
                        .copied()
                        .map(nested)
                        .transpose()?
                        .unwrap_or(V::Unknown),
                ))
            } else if Some(*record) == information.core.bytes {
                V::Bytes
            } else {
                let erase = match P::RECORDS {
                    RecordErasure::StatelessWithoutCapabilities => {
                        hir.records[*record].fields.is_empty()
                            && ![COPY_SLOT, DEINIT_SLOT].iter().any(|slot| {
                                information
                                    .dispatch
                                    .contains_key(&(NominalTypeId::Record(*record), *slot))
                            })
                    }
                    RecordErasure::NestedDynamicContracts => {
                        depth > 0 && record_uses_dynamic_dispatch(hir, information, *record)
                    }
                };
                if erase {
                    V::Unknown
                } else {
                    V::Record {
                        record: *record,
                        arguments: arguments(values)?,
                    }
                }
            }
        }
        Type::Variant {
            variant,
            arguments: values,
        } => {
            if hir.variant_types[*variant].kind == VariantKind::Alias {
                // Preserve the previous alias-argument metadata policy. Resolving aliases
                // to their targets is a separate semantic change, not part of this walk.
                view(values)?
            } else {
                V::Variant {
                    variant: *variant,
                    arguments: arguments(values)?,
                }
            }
        }
    })
}

/// Native structural-dispatch classification, also used for top-level representation choice.
pub(crate) fn record_uses_dynamic_dispatch(
    hir: &PackageHir,
    information: &TypeInformation,
    record: RecordId,
) -> bool {
    let declaration = &hir.records[record];
    // Private storage always keeps its concrete descriptor.
    if declaration.fields.iter().any(|field| !field.public) {
        return false;
    }
    let has_contract_surface =
        !declaration.methods.is_empty() || !declaration.compositions.is_empty();
    // Inherited defaults add dispatch entries without creating implementation storage.
    let has_implementation = information.dispatch.iter().any(|((nominal, _), function)| {
        *nominal == NominalTypeId::Record(record) && !hir.composition_owners.contains_key(function)
    });
    let provides_defaults = hir.composition_dispatch.iter().any(|function| {
        let method = &hir.functions[*function];
        !hir.composition_owners.contains_key(function)
            && method.module == declaration.module
            && method.owner.as_deref() == Some(declaration.name.as_str())
    });
    (has_contract_surface && !has_implementation) || provides_defaults
}

#[cfg(test)]
mod tests;
