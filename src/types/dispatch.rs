use super::{DispatchTypeKey as D, MethodKey};
use crate::dispatch::Tree;

impl MethodKey {
    pub(crate) fn canonical(mut self) -> Self {
        let types = self
            .parameters
            .iter()
            .map(|(_, ty)| ty.clone())
            .collect::<Vec<_>>();
        for ((_, ty), canonical) in self
            .parameters
            .iter_mut()
            .zip(crate::dispatch::canonical(&types))
        {
            *ty = canonical;
        }
        self
    }

    pub(crate) fn matches(&self, actual: &Self) -> bool {
        self.name == actual.name
            && self
                .parameters
                .iter()
                .map(|(mode, _)| mode)
                .eq(actual.parameters.iter().map(|(mode, _)| mode))
            && crate::dispatch::matches(
                &self
                    .parameters
                    .iter()
                    .map(|(_, ty)| ty.clone())
                    .collect::<Vec<_>>(),
                &actual
                    .parameters
                    .iter()
                    .map(|(_, ty)| ty.clone())
                    .collect::<Vec<_>>(),
            )
    }
}

impl Tree for D {
    fn generic(&self) -> Option<u32> {
        if let Self::Generic(index) = self {
            Some(*index)
        } else {
            None
        }
    }
    fn renamed_generic(index: u32) -> Self {
        Self::Generic(index)
    }
    fn unordered(&self) -> bool {
        matches!(self, Self::Intersection(_))
    }
    fn children(&self) -> Vec<&Self> {
        match self {
            Self::Reference(value)
            | Self::RawList(value)
            | Self::Sequence(value)
            | Self::Remote(value)
            | Self::Future(value) => vec![value],
            Self::Record(_, args) | Self::Variant(_, args) | Self::Intersection(args) => {
                args.iter().collect()
            }
            Self::Function(params, result) => params
                .iter()
                .map(|(_, ty)| ty)
                .chain(std::iter::once(result.as_ref()))
                .collect(),
            _ => vec![],
        }
    }
    fn with_children(&self, children: Vec<Self>) -> Self {
        let mut children = children.into_iter();
        match self {
            Self::Reference(_) => Self::Reference(Box::new(children.next().unwrap())),
            Self::RawList(_) => Self::RawList(Box::new(children.next().unwrap())),
            Self::Sequence(_) => Self::Sequence(Box::new(children.next().unwrap())),
            Self::Remote(_) => Self::Remote(Box::new(children.next().unwrap())),
            Self::Future(_) => Self::Future(Box::new(children.next().unwrap())),
            Self::Record(id, _) => Self::Record(*id, children.collect()),
            Self::Variant(id, _) => Self::Variant(*id, children.collect()),
            Self::Intersection(_) => Self::Intersection(children.collect()),
            Self::Function(params, _) => Self::Function(
                params
                    .iter()
                    .map(|(mode, _)| (*mode, children.next().unwrap()))
                    .collect(),
                Box::new(children.next().unwrap()),
            ),
            _ => self.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::ParameterMode::{Borrow, Consume};

    fn record(id: u32, args: Vec<D>) -> D {
        D::Record(
            la_arena::Idx::from_raw(la_arena::RawIdx::from_u32(id)),
            args,
        )
    }
    fn key(types: Vec<D>) -> MethodKey {
        MethodKey {
            name: "apply".into(),
            parameters: types.into_iter().map(|ty| (Borrow, ty)).collect(),
        }
    }

    #[test]
    fn dispatch_identity_preserves_generic_relationships_across_parameters() {
        let left = key(vec![
            D::Intersection(vec![
                record(1, vec![D::Generic(7)]),
                record(2, vec![D::Generic(8)]),
            ]),
            D::Generic(7),
        ])
        .canonical();
        let right = key(vec![
            D::Intersection(vec![
                record(2, vec![D::Generic(4)]),
                record(1, vec![D::Generic(5)]),
            ]),
            D::Generic(5),
        ])
        .canonical();
        assert_eq!(left, right);
        assert_eq!(left, left.clone().canonical());
        let different = key(vec![
            D::Intersection(vec![
                record(2, vec![D::Generic(4)]),
                record(1, vec![D::Generic(5)]),
            ]),
            D::Generic(4),
        ])
        .canonical();
        assert_ne!(left, different);
        let shared = key(vec![
            D::Intersection(vec![
                record(1, vec![D::Generic(7)]),
                record(2, vec![D::Generic(7)]),
            ]),
            D::Generic(7),
        ])
        .canonical();
        assert_ne!(left, shared);
        let mut consumed = left.clone();
        consumed.parameters[0].0 = Consume;
        assert!(!left.matches(&consumed));
    }

    #[test]
    fn dispatch_identity_keeps_ties_until_nested_callable_results() {
        let signature = |members, result| {
            key(vec![D::Function(
                vec![(Borrow, D::Intersection(members))],
                Box::new(result),
            )])
            .canonical()
        };
        let left = signature(
            vec![
                record(1, vec![D::Generic(1)]),
                record(1, vec![D::Generic(2)]),
            ],
            D::Generic(1),
        );
        let right = signature(
            vec![
                record(1, vec![D::Generic(20)]),
                record(1, vec![D::Generic(10)]),
            ],
            D::Generic(10),
        );
        assert_eq!(left, right);
        assert_ne!(
            left,
            signature(
                vec![
                    record(1, vec![D::Generic(1)]),
                    record(1, vec![D::Generic(2)])
                ],
                D::Generic(3)
            )
        );
    }

    #[test]
    fn dispatch_matching_backtracks_across_later_parameters() {
        let pattern = key(vec![
            D::Intersection(vec![
                record(1, vec![D::Generic(0)]),
                record(1, vec![D::Generic(1)]),
            ]),
            D::Generic(0),
        ])
        .canonical();
        let actual = |last| {
            key(vec![
                D::Intersection(vec![record(1, vec![D::Int]), record(1, vec![D::Bool])]),
                last,
            ])
            .canonical()
        };
        assert!(pattern.matches(&actual(D::Int)));
        assert!(pattern.matches(&actual(D::Bool)));
        assert!(!pattern.matches(&actual(D::Byte)));
    }

    #[test]
    fn dispatch_identity_preserves_multiplicity_and_actual_generic_identity() {
        let a = record(1, vec![]);
        let b = record(2, vec![]);
        assert_ne!(
            key(vec![D::Intersection(vec![a.clone(), b.clone()])]).canonical(),
            key(vec![D::Intersection(vec![a.clone(), b, a])]).canonical()
        );
        let pattern = key(vec![D::Generic(0), D::Generic(0)]);
        assert!(!pattern.matches(&key(vec![D::Generic(1), D::Generic(2)])));
    }

    #[test]
    fn dispatch_identity_prunes_distinguishable_intersection_permutations() {
        let members = (0..16)
            .map(|i| record(i, vec![D::Generic(i)]))
            .collect::<Vec<_>>();
        let reversed = members.iter().rev().cloned().collect();
        assert_eq!(
            key(vec![D::Intersection(members)]).canonical(),
            key(vec![D::Intersection(reversed)]).canonical()
        );
    }
}
