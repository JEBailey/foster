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
                },
            })
        })
        .collect::<Result<Vec<_>, FosterError>>()?;
    for library in &compilation.package.libraries {
        for slot in &library.interface.slots {
            if !result.iter().any(|s| s.key == slot.key) {
                result.push(Slot {
                    id: result.len() as u32,
                    key: slot.key.clone(),
                });
            }
        }
    }
    Ok(result)
}

pub(super) fn matches(pattern: &MethodKey, concrete: &MethodKey) -> bool {
    fn ty(a: &S, b: &S, g: &mut BTreeMap<u32, S>) -> bool {
        if let S::Generic(i) = a {
            return g.entry(*i).or_insert_with(|| b.clone()) == b;
        }
        match (a, b) {
            (S::Nominal(n, a), S::Nominal(m, b)) => n == m && list(a, b, g),
            (S::Applied(n, a), S::Applied(m, b)) => n == m && list(a, b, g),
            (S::Reference(_, a), S::Reference(_, b)) => ty(a, b, g),
            (S::Intersection(a), S::Intersection(b)) => list(a, b, g),
            (S::Function(a), S::Function(b)) => {
                params(&a.parameters, &b.parameters, g) && ty(&a.result, &b.result, g)
            }
            _ => a == b,
        }
    }
    fn list(a: &[S], b: &[S], g: &mut BTreeMap<u32, S>) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(a, b)| ty(a, b, g))
    }
    fn params(
        a: &[symbols::Parameter],
        b: &[symbols::Parameter],
        g: &mut BTreeMap<u32, S>,
    ) -> bool {
        a.len() == b.len()
            && a.iter()
                .zip(b)
                .all(|(a, b)| a.mode == b.mode && ty(&a.ty, &b.ty, g))
    }
    pattern.name == concrete.name
        && params(
            &pattern.parameters,
            &concrete.parameters,
            &mut BTreeMap::new(),
        )
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
