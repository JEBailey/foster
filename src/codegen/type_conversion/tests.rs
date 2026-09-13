use super::*;
use crate::ast::{Effect, EffectKind, GroupPath, ParameterMode};
use crate::compiler::Compilation;
use crate::types::{DispatchSlot, FunctionType};

fn fixture() -> Compilation {
    crate::compile(
        r#"
type Empty = {}
type Surface = { pub func read(self) -> Int }
type StoredSurface = { pub value: Int, pub func read(self) -> Int }
type PrivateSurface = { value: Int, pub func read(self) -> Int }
type Ordinary<T> = { pub value: T }
type Implemented = { pub func read(self) -> Int }
impl Implemented { func read(self: Implemented) -> Int { 42 } }
type Alias<T> = List<T>
enum Choice<T> = Some(T) | None
func main() -> Int { 0 }
"#,
    )
    .unwrap()
}

fn record(compilation: &mut Compilation, name: &str, arguments: Vec<TypeId>) -> (RecordId, TypeId) {
    let module = compilation.hir.module_named("main").unwrap();
    let record = compilation.hir.record_named(module, name).unwrap();
    let ty = compilation
        .types
        .types
        .alloc(Type::Record { record, arguments });
    (record, ty)
}

fn bytecode(compilation: &Compilation, ty: TypeId, depth: usize) -> V {
    convert::<Bytecode>(
        &compilation.hir,
        &compilation.types,
        ty,
        &Default::default(),
        depth,
    )
    .unwrap_or_else(|never| match never {})
}

fn native(compilation: &Compilation, ty: TypeId, depth: usize) -> Result<V, NestingLimit> {
    convert::<Native>(
        &compilation.hir,
        &compilation.types,
        ty,
        &Default::default(),
        depth,
    )
}

#[test]
fn scalar_and_nested_callable_conversions_agree() {
    let mut compilation = fixture();
    for (ty, expected) in [
        (Type::Unit, V::Unit),
        (Type::Bool, V::Bool),
        (Type::Int, V::Integer),
        (Type::RawInt, V::Integer),
        (Type::Float, V::Float),
        (Type::CodePoint, V::CodePoint),
        (Type::Byte, V::Byte),
        (Type::RawBytes, V::Bytes),
        (Type::RawByteBuffer, V::ByteBuffer),
        (Type::Module("main".into()), V::Unknown),
    ] {
        let ty = compilation.types.types.alloc(ty);
        assert_eq!(bytecode(&compilation, ty, 0), expected);
        assert_eq!(native(&compilation, ty, 0), Ok(expected));
    }
    let generic = compilation.types.types.alloc(Type::Generic("T".into()));
    let list = compilation.types.types.alloc(Type::RawList(generic));
    let reference = compilation.types.types.alloc(Type::Reference {
        group: "g".into(),
        value: list,
    });
    let future = compilation.types.types.alloc(Type::Future(reference));
    let function = compilation.types.types.alloc(Type::Function(FunctionType {
        parameters: vec![
            crate::types::Parameter {
                ty: future,
                mode: ParameterMode::Borrow,
            },
            crate::types::Parameter {
                ty: generic,
                mode: ParameterMode::Consume,
            },
        ],
        result: reference,
        erased: true,
        effects: vec![Effect {
            kind: EffectKind::Read,
            target: GroupPath::root("g"),
        }],
        suspends: true,
    }));
    let pointee = V::Reference(Box::new(V::List(Box::new(V::Generic("T".into())))));
    let expected = V::Function {
        parameters: crate::types::Parameter::from_parts(
            vec![V::Future(Box::new(pointee.clone())), V::Generic("T".into())],
            vec![ParameterMode::Borrow, ParameterMode::Consume],
        ),
        result: Box::new(pointee),
    };
    assert_eq!(bytecode(&compilation, function, 0), expected);
    assert_eq!(native(&compilation, function, 0), Ok(expected.clone()));
    let substitutions = Specialization::try_new(vec![("T".into(), V::Integer)]).unwrap();
    assert_eq!(
        convert::<Native>(
            &compilation.hir,
            &compilation.types,
            function,
            &substitutions,
            0
        ),
        Ok(expected.specialize(&substitutions))
    );
}

#[test]
fn structural_record_erasure_is_an_explicit_backend_policy() {
    let mut compilation = fixture();
    for (name, vm_erased, native_nested_erased) in [
        ("Empty", true, false),
        ("Surface", true, true),
        ("StoredSurface", false, true),
        ("PrivateSurface", false, false),
        ("Ordinary", false, false),
    ] {
        let (record, ty) = record(&mut compilation, name, vec![]);
        let nominal = V::Record {
            record,
            arguments: vec![],
        };
        assert_eq!(
            bytecode(&compilation, ty, 0),
            if vm_erased {
                V::Unknown
            } else {
                nominal.clone()
            },
            "{name}"
        );
        assert_eq!(native(&compilation, ty, 0), Ok(nominal.clone()), "{name}");
        let list = compilation.types.types.alloc(Type::RawList(ty));
        assert_eq!(
            native(&compilation, list, 0),
            Ok(V::List(Box::new(if native_nested_erased {
                V::Unknown
            } else {
                nominal
            }))),
            "{name}"
        );
    }
}

#[test]
fn capabilities_inherited_methods_and_defaults_keep_their_erasure_rules() {
    let mut compilation = fixture();
    let (record, ty) = record(&mut compilation, "Implemented", vec![]);
    let module = compilation.hir.module_named("main").unwrap();
    let function = compilation
        .hir
        .function_named(module, "Implemented.read")
        .unwrap();
    let nominal = V::Record {
        record,
        arguments: vec![],
    };
    compilation
        .types
        .dispatch
        .insert((NominalTypeId::Record(record), DispatchSlot(17)), function);
    assert_eq!(bytecode(&compilation, ty, 0), V::Unknown);
    assert_eq!(native(&compilation, ty, 1), Ok(nominal.clone()));
    compilation.hir.composition_owners.insert(function, module);
    assert_eq!(native(&compilation, ty, 1), Ok(V::Unknown));
    compilation.hir.composition_owners.remove(&function);
    compilation.hir.composition_dispatch.insert(function);
    assert_eq!(native(&compilation, ty, 1), Ok(V::Unknown));
    compilation.hir.composition_dispatch.remove(&function);
    for slot in [COPY_SLOT, DEINIT_SLOT] {
        compilation
            .types
            .dispatch
            .insert((NominalTypeId::Record(record), slot), function);
        assert_eq!(bytecode(&compilation, ty, 0), nominal);
        compilation
            .types
            .dispatch
            .remove(&(NominalTypeId::Record(record), slot));
    }
}

#[test]
fn views_aliases_and_remote_receivers_preserve_backend_metadata() {
    let mut compilation = fixture();
    let (surface, surface_ty) = record(&mut compilation, "Surface", vec![]);
    let (ordinary, ordinary_ty) = record(&mut compilation, "Ordinary", vec![]);
    let view = compilation
        .types
        .types
        .alloc(Type::Intersection(vec![surface_ty, ordinary_ty]));
    assert_eq!(bytecode(&compilation, view, 0), V::Unknown);
    assert_eq!(
        native(&compilation, view, 0),
        Ok(V::Union(vec![
            V::Unknown,
            V::Record {
                record: ordinary,
                arguments: vec![]
            }
        ]))
    );
    let remote = compilation.types.types.alloc(Type::Remote(surface_ty));
    assert_eq!(
        bytecode(&compilation, remote, 0),
        V::Remote(Box::new(V::Record {
            record: surface,
            arguments: vec![]
        }))
    );
    assert_eq!(
        native(&compilation, remote, 0),
        Ok(V::Remote(Box::new(V::Unknown)))
    );
    let generic = compilation.types.types.alloc(Type::Generic("T".into()));
    let module = compilation.hir.module_named("main").unwrap();
    for (name, alias) in [("Alias", true), ("Choice", false)] {
        let variant = compilation.hir.variant_type_named(module, name).unwrap();
        let ty = compilation.types.types.alloc(Type::Variant {
            variant,
            arguments: vec![generic],
        });
        let nominal = V::Variant {
            variant,
            arguments: vec![V::Generic("T".into())],
        };
        assert_eq!(
            bytecode(&compilation, ty, 0),
            if alias { V::Unknown } else { nominal.clone() }
        );
        assert_eq!(
            native(&compilation, ty, 0),
            Ok(if alias {
                V::Union(vec![V::Generic("T".into())])
            } else {
                nominal
            })
        );
    }
    let sequence = compilation.types.types.alloc(Type::Sequence(generic));
    assert_eq!(bytecode(&compilation, sequence, 0), V::Unknown);
    assert_eq!(native(&compilation, sequence, 0), Ok(V::Unknown));
}

#[test]
fn builtin_nominals_and_generic_arguments_use_the_shared_walk() {
    let mut compilation = fixture();
    let generic = compilation.types.types.alloc(Type::Generic("T".into()));
    let (ordinary, ordinary_ty) = record(&mut compilation, "Ordinary", vec![generic]);
    let record = compilation.types.core.list.unwrap();
    let list = compilation.types.types.alloc(Type::Record {
        record,
        arguments: vec![ordinary_ty],
    });
    let expected = V::List(Box::new(V::Record {
        record: ordinary,
        arguments: vec![V::Generic("T".into())],
    }));
    assert_eq!(bytecode(&compilation, list, 0), expected);
    assert_eq!(native(&compilation, list, 0), Ok(expected));
    let malformed = compilation.types.types.alloc(Type::Record {
        record,
        arguments: vec![],
    });
    assert_eq!(
        bytecode(&compilation, malformed, 0),
        V::List(Box::new(V::Unknown))
    );
    assert_eq!(
        native(&compilation, malformed, 0),
        Ok(V::Record {
            record,
            arguments: vec![]
        })
    );
    let bytes = compilation.types.types.alloc(Type::Record {
        record: compilation.types.core.bytes.unwrap(),
        arguments: vec![],
    });
    assert_eq!(bytecode(&compilation, bytes, 0), V::Bytes);
    assert_eq!(native(&compilation, bytes, 0), Ok(V::Bytes));
}

#[test]
fn nesting_limit_preserves_bytecode_erasure_and_native_diagnostics() {
    let mut compilation = fixture();
    let scalar = compilation.types.types.alloc(Type::Int);
    let list = compilation.types.types.alloc(Type::RawList(scalar));
    assert_eq!(
        bytecode(&compilation, scalar, MAX_TYPE_DEPTH - 1),
        V::Integer
    );
    assert_eq!(
        native(&compilation, scalar, MAX_TYPE_DEPTH - 1),
        Ok(V::Integer)
    );
    assert_eq!(
        bytecode(&compilation, list, MAX_TYPE_DEPTH - 1),
        V::List(Box::new(V::Unknown))
    );
    assert_eq!(
        native(&compilation, list, MAX_TYPE_DEPTH - 1),
        Err(NestingLimit)
    );
    assert_eq!(bytecode(&compilation, scalar, MAX_TYPE_DEPTH), V::Unknown);
    assert_eq!(
        native(&compilation, scalar, MAX_TYPE_DEPTH),
        Err(NestingLimit)
    );
    // Erased views do not walk their members, even at the final permitted level.
    let view = compilation
        .types
        .types
        .alloc(Type::Intersection(vec![scalar, list]));
    assert_eq!(bytecode(&compilation, view, MAX_TYPE_DEPTH - 1), V::Unknown);
    assert_eq!(
        native(&compilation, view, MAX_TYPE_DEPTH - 1),
        Err(NestingLimit)
    );
}
