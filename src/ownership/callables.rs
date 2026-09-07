//! Type-level fallback for callable results whose concrete target is unavailable.
use crate::{
    hir::PackageHir,
    types::{FunctionType, Type, TypeId, TypeInformation},
};
use std::collections::{HashMap, HashSet};

/// Prove that the complete result representation contains no borrowers. A function
/// value can hide references in its environment even when its signature has none.
pub(super) fn may_borrow(hir: &PackageHir, types: &TypeInformation, ty: TypeId) -> bool {
    fn visit(
        hir: &PackageHir,
        types: &TypeInformation,
        ty: TypeId,
        substitutions: &HashMap<String, TypeId>,
        active: &mut HashSet<TypeId>,
        depth: usize,
    ) -> bool {
        if depth >= 32 || !active.insert(ty) {
            return true;
        }
        let result = match &types.types[ty] {
            Type::Reference { .. } | Type::Function(_) | Type::Intersection(_) => true,
            Type::Generic(name) => substitutions
                .get(name)
                .is_none_or(|actual| visit(hir, types, *actual, substitutions, active, depth + 1)),
            Type::RawList(element) | Type::Future(element) => {
                visit(hir, types, *element, substitutions, active, depth + 1)
            }
            // Structural/remote representations may hide storage dependencies.
            Type::Sequence(_) | Type::Remote(_) => true,
            Type::Record { record, arguments } => {
                let declaration = &hir.records[*record];
                // A public structural surface can conceal extra implementation fields.
                if declaration.fields.iter().all(|field| field.public)
                    && (!declaration.methods.is_empty() || !declaration.compositions.is_empty())
                {
                    active.remove(&ty);
                    return true;
                }
                let mut nested = substitutions.clone();
                for (name, argument) in hir.records[*record].parameters.iter().zip(arguments) {
                    let actual = match &types.types[*argument] {
                        Type::Generic(name) => {
                            substitutions.get(name).copied().unwrap_or(*argument)
                        }
                        _ => *argument,
                    };
                    nested.insert(name.clone(), actual);
                }
                types.record_field_types.get(record).is_none_or(|fields| {
                    fields
                        .iter()
                        .any(|(_, field)| visit(hir, types, *field, &nested, active, depth + 1))
                })
            }
            Type::Variant { variant, arguments } => {
                let mut nested = substitutions.clone();
                for (name, argument) in hir.variant_types[*variant].parameters.iter().zip(arguments)
                {
                    let actual = match &types.types[*argument] {
                        Type::Generic(name) => {
                            substitutions.get(name).copied().unwrap_or(*argument)
                        }
                        _ => *argument,
                    };
                    nested.insert(name.clone(), actual);
                }
                types.variant_field_types.get(variant).is_none_or(|fields| {
                    fields
                        .iter()
                        .any(|field| visit(hir, types, *field, &nested, active, depth + 1))
                })
            }
            _ => false,
        };
        active.remove(&ty);
        result
    }
    visit(hir, types, ty, &HashMap::new(), &mut HashSet::new(), 0)
}

/// Explicit reference groups already form part of callable compatibility checking.
/// Unknown groups and aggregate/environment dependencies keep the conservative union.
pub(super) fn parameter_origins(types: &TypeInformation, signature: &FunctionType) -> Vec<usize> {
    signature
        .parameters
        .iter()
        .enumerate()
        .filter_map(|(index, parameter)| {
            let excluded = matches!((&types.types[signature.result], &types.types[*parameter]),
            (Type::Reference { group: result, .. }, Type::Reference { group: input, .. })
                if result != "_" && input != "_" && result != "<frame>" && input != "<frame>" && result != input);
            (!excluded).then_some(index)
        })
        .collect()
}
