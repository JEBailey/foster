use super::*;
use crate::types::DispatchTypeKey as D;
use symbols::SymbolType as S;

pub(super) fn slots(
    compilation: &Compilation,
    program: &vm::Program,
) -> Result<Vec<Slot>, FosterError> {
    fn ty(value: &D, names: &BTreeMap<(bool, u32), symbols::Name>) -> Result<S, FosterError> {
        let nested = |v| ty(v, names);
        Ok(match value {
            D::Generic(i) => S::Generic(*i),
            D::Never => S::Primitive("never".into()),
            D::Unit => S::Primitive("unit".into()),
            D::Bool => S::Primitive("bool".into()),
            D::Int => S::Primitive("int".into()),
            D::RawInt => S::Primitive("raw_int".into()),
            D::Float => S::Primitive("float".into()),
            D::CodePoint => S::Primitive("code_point".into()),
            D::Byte => S::Primitive("byte".into()),
            D::RawBytes => S::Primitive("raw_bytes".into()),
            D::RawByteBuffer => S::Primitive("raw_byte_buffer".into()),
            D::Record(id, args) => S::Nominal(
                names
                    .get(&(false, id.into_raw().into_u32()))
                    .ok_or_else(|| error("unknown dispatch record"))?
                    .clone(),
                args.iter().map(nested).collect::<Result<_, _>>()?,
            ),
            D::Variant(id, args) => S::Nominal(
                names
                    .get(&(true, id.into_raw().into_u32()))
                    .ok_or_else(|| error("unknown dispatch variant"))?
                    .clone(),
                args.iter().map(nested).collect::<Result<_, _>>()?,
            ),
            D::Reference(v) => S::Reference(String::new(), Box::new(nested(v)?)),
            D::RawList(v) | D::Sequence(v) | D::Remote(v) | D::Future(v) => S::Applied(
                match value {
                    D::RawList(_) => "raw_list",
                    D::Sequence(_) => "sequence",
                    D::Remote(_) => "remote",
                    _ => "future",
                }
                .into(),
                vec![nested(v)?],
            ),
            D::Intersection(v) => S::Intersection(v.iter().map(nested).collect::<Result<_, _>>()?),
            D::Function(p, r) => S::Function(Box::new(symbols::Descriptor {
                generics: 0,
                constraints: vec![],
                receiver: false,
                parameters: p
                    .iter()
                    .map(|(m, t)| {
                        Ok(symbols::Parameter {
                            mode: (*m).into(),
                            ty: nested(t)?,
                        })
                    })
                    .collect::<Result<_, FosterError>>()?,
                result: nested(r)?,
                groups: vec![],
                effects: vec![],
                suspends: false,
                result_dependencies: vec![],
                fresh_result: false,
            })),
            D::Module(_) => return Err(error("module dispatch parameter is unsupported")),
        })
    }
    let names = program
        .metadata
        .symbols
        .modules
        .iter()
        .flat_map(|m| &m.types)
        .map(|t| ((t.variant, t.id), t.name.clone()))
        .collect();
    let mut result = compilation
        .types
        .dispatch_keys
        .iter()
        .enumerate()
        .map(|(i, key)| {
            Ok(Slot {
                id: i as u32,
                key: MethodKey {
                    name: key.name.clone(),
                    parameters: key
                        .parameters
                        .iter()
                        .map(|(m, t)| {
                            Ok(symbols::Parameter {
                                mode: (*m).into(),
                                ty: ty(t, &names)?,
                            })
                        })
                        .collect::<Result<_, FosterError>>()?,
                }
                .canonical(),
            })
        })
        .collect::<Result<Vec<_>, FosterError>>()?;
    for library in &compilation.package.libraries {
        for slot in &library.interface.slots {
            let key = slot.key.clone().canonical();
            if !result.iter().any(|s| s.key == key) {
                result.push(Slot {
                    id: result.len() as u32,
                    key,
                });
            }
        }
    }
    Ok(result)
}

pub(super) fn matches(pattern: &MethodKey, concrete: &MethodKey) -> bool {
    pattern.name == concrete.name
        && pattern
            .parameters
            .iter()
            .map(|p| p.mode)
            .eq(concrete.parameters.iter().map(|p| p.mode))
        && crate::dispatch::matches(
            &pattern
                .parameters
                .iter()
                .map(|p| p.ty.clone())
                .collect::<Vec<_>>(),
            &concrete
                .parameters
                .iter()
                .map(|p| p.ty.clone())
                .collect::<Vec<_>>(),
        )
}

impl MethodKey {
    pub(super) fn canonical(mut self) -> Self {
        let types = self
            .parameters
            .iter()
            .map(|p| p.ty.clone())
            .collect::<Vec<_>>();
        for (parameter, ty) in self
            .parameters
            .iter_mut()
            .zip(crate::dispatch::canonical(&types))
        {
            parameter.ty = ty;
        }
        self
    }
}

impl crate::dispatch::Tree for S {
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
            Self::Reference(_, value) => vec![value],
            Self::Nominal(_, args) | Self::Applied(_, args) | Self::Intersection(args) => {
                args.iter().collect()
            }
            Self::Function(signature) => signature
                .parameters
                .iter()
                .map(|p| &p.ty)
                .chain(std::iter::once(&signature.result))
                .collect(),
            _ => vec![],
        }
    }
    fn with_children(&self, children: Vec<Self>) -> Self {
        let mut children = children.into_iter();
        match self {
            Self::Reference(_, _) => {
                Self::Reference(String::new(), Box::new(children.next().unwrap()))
            }
            Self::Nominal(name, _) => Self::Nominal(name.clone(), children.collect()),
            Self::Applied(name, _) => Self::Applied(name.clone(), children.collect()),
            Self::Intersection(_) => Self::Intersection(children.collect()),
            // Dispatch ignores callable effects and provenance, just as source MethodKey does.
            Self::Function(signature) => Self::Function(Box::new(symbols::Descriptor {
                generics: 0,
                constraints: vec![],
                receiver: false,
                parameters: signature
                    .parameters
                    .iter()
                    .map(|p| symbols::Parameter {
                        mode: p.mode,
                        ty: children.next().unwrap(),
                    })
                    .collect(),
                result: children.next().unwrap(),
                groups: vec![],
                effects: vec![],
                suspends: false,
                result_dependencies: vec![],
                fresh_result: false,
            })),
            _ => self.clone(),
        }
    }
}

pub(super) fn generic_count(key: &MethodKey) -> usize {
    fn count(t: &S) -> usize {
        match t {
            S::Generic(_) => 1,
            S::Reference(_, v) => count(v),
            S::Nominal(_, v) | S::Applied(_, v) | S::Intersection(v) => v.iter().map(count).sum(),
            S::Function(f) => {
                f.parameters.iter().map(|p| count(&p.ty)).sum::<usize>() + count(&f.result)
            }
            _ => 0,
        }
    }
    key.parameters.iter().map(|p| count(&p.ty)).sum()
}
