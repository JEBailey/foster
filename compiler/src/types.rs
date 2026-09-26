use std::collections::{HashMap, HashSet};

use la_arena::{Arena, Idx};

use crate::ast;
use crate::hir::{ConstantId, ExprId, FunctionId, LocalId, RecordId, VariantId, VariantTypeId};

pub type TypeId = Idx<Type>;

mod dispatch;

#[cfg(test)]
mod parameter_tests;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    Never,
    Generic(String),
    Unit,
    Bool,
    Int,
    RawInt,
    Float,
    CodePoint,
    Byte,
    RawBytes,
    RawByteBuffer,
    Reference {
        group: String,
        value: TypeId,
    },
    RawList(TypeId),
    Sequence(TypeId),
    Remote(TypeId),
    Future(TypeId),
    Function(FunctionType),
    Record {
        record: RecordId,
        arguments: Vec<TypeId>,
    },
    Intersection(Vec<TypeId>),
    Variant {
        variant: VariantTypeId,
        arguments: Vec<TypeId>,
    },
    Module(String),
}

pub type Parameter<T = TypeId> = foster_bytecode::types::Parameter<T>;
pub use foster_bytecode::types::ParameterCountMismatch;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FunctionType {
    pub parameters: Vec<Parameter>,
    pub result: TypeId,
    pub erased: bool,
    pub effects: Vec<ast::Effect>,
    pub suspends: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DispatchTypeKey {
    Never,
    Generic(u32),
    Unit,
    Bool,
    Int,
    RawInt,
    Float,
    CodePoint,
    Byte,
    RawBytes,
    RawByteBuffer,
    Reference(Box<Self>),
    RawList(Box<Self>),
    Sequence(Box<Self>),
    Remote(Box<Self>),
    Future(Box<Self>),
    Function(Vec<(ast::ParameterMode, Self)>, Box<Self>),
    Record(RecordId, Vec<Self>),
    Intersection(Vec<Self>),
    Variant(VariantTypeId, Vec<Self>),
    Module(String),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MethodKey {
    pub name: String,
    pub parameters: Vec<(ast::ParameterMode, DispatchTypeKey)>,
}

pub use foster_bytecode::types::{
    CAN_COPY_SLOT, COPY_SLOT, DEINIT_SLOT, DispatchSlot, NominalTypeId,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedCall {
    Function(FunctionId),
    Method {
        function: FunctionId,
        remote: bool,
    },
    ContractMethod {
        slot: DispatchSlot,
        name: String,
        requirement: Option<(RecordId, usize)>,
    },
}

impl ResolvedCall {
    pub fn function(&self) -> Option<FunctionId> {
        match *self {
            Self::Function(function) | Self::Method { function, .. } => Some(function),
            Self::ContractMethod { .. } => None,
        }
    }
}

/// Canonical standard-library record identities, resolved once after lowering.
#[derive(Debug, Default)]
pub struct CoreRecords {
    pub int: Option<RecordId>,
    pub string: Option<RecordId>,
    pub symbol: Option<RecordId>,
    pub bytes: Option<RecordId>,
    pub list: Option<RecordId>,
    pub byte_buffer: Option<RecordId>,
    pub future: Option<RecordId>,
    pub remote_future: Option<RecordId>,
}
impl CoreRecords {
    pub(crate) fn resolve(hir: &crate::hir::PackageHir) -> Self {
        let find = |module, name| {
            hir.module_named(module)
                .and_then(|id| hir.record_named(id, name))
        };
        Self {
            future: find("core.future", "Future"),
            remote_future: find("core.future", "RemoteFuture"),
            int: find("core.int", "Int"),
            string: find("core.string", "String"),
            symbol: find("core.symbol", "Symbol"),
            bytes: find("core.bytes", "Bytes"),
            list: find("core.list", "List"),
            byte_buffer: find("core.bytes.buffer", "ByteBuffer"),
        }
    }
}

#[derive(Debug, Default)]
pub struct TypeInformation {
    /// Runtime inhabitants accepted by the same structural checks as assignment.
    pub type_conformances: HashMap<
        (FunctionId, crate::codegen::types::ExecutableType),
        Vec<crate::codegen::types::ExecutableType>,
    >,
    pub core: CoreRecords,
    pub types: Arena<Type>,
    pub expressions: HashMap<ExprId, TypeId>,
    pub integer_promotions: HashSet<ExprId>,
    /// Resolved semantics for every member expression.
    pub member_kinds: HashMap<ExprId, crate::semantics::MemberKind>,
    pub resolved_calls: HashMap<ExprId, ResolvedCall>,
    pub dispatch: HashMap<(NominalTypeId, DispatchSlot), FunctionId>,
    pub dispatch_keys: Vec<MethodKey>,
    pub locals: HashMap<LocalId, TypeId>,
    pub functions: HashMap<FunctionId, FunctionType>,
    pub constants: HashMap<ConstantId, TypeId>,
    pub record_names: HashMap<RecordId, String>,
    pub record_fields: HashMap<RecordId, HashSet<String>>,
    /// Canonical stored fields and their generic-aware declared types.
    pub record_field_types: HashMap<RecordId, Vec<(String, TypeId)>>,
    pub record_methods: HashMap<RecordId, HashSet<String>>,
    pub variant_names: HashMap<VariantTypeId, String>,
    /// Declared enum-case payload types. `None` denotes a payload-free case.
    pub variant_payloads: HashMap<VariantId, Option<TypeId>>,
    pub variant_field_types: HashMap<VariantTypeId, Vec<TypeId>>,
}

impl TypeInformation {
    /// Awaitable contracts preserve remote request provenance through helper calls.
    pub(crate) fn is_future(&self, ty: TypeId) -> bool {
        match &self.types[ty] {
            Type::Future(_) => true,
            Type::Record { record, .. } => {
                Some(*record) == self.core.future || Some(*record) == self.core.remote_future
            }
            Type::Intersection(members) => members.iter().any(|member| self.is_future(*member)),
            _ => false,
        }
    }
    /// Whether retaining an internal alias could postpone observable resource cleanup.
    pub(crate) fn has_cleanup(&self, ty: TypeId) -> bool {
        fn visit(types: &TypeInformation, ty: TypeId, seen: &mut HashSet<TypeId>) -> bool {
            if !seen.insert(ty) {
                return false;
            }
            match &types.types[ty] {
                Type::Generic(_) | Type::Function(_) | Type::Remote(_) | Type::Future(_) => true,
                Type::Record { record, .. } if Some(*record) == types.core.future => true,
                Type::Record { record, arguments } if Some(*record) == types.core.list => {
                    arguments.iter().any(|ty| visit(types, *ty, seen))
                }
                Type::Record { record, arguments } => {
                    types
                        .dispatch
                        .contains_key(&(NominalTypeId::Record(*record), DEINIT_SLOT))
                        || arguments.iter().any(|ty| visit(types, *ty, seen))
                        || types.record_field_types.get(record).is_some_and(|fields| {
                            fields.iter().any(|(_, ty)| visit(types, *ty, seen))
                        })
                }
                Type::Variant { variant, arguments } => {
                    types
                        .dispatch
                        .contains_key(&(NominalTypeId::Variant(*variant), DEINIT_SLOT))
                        || arguments.iter().any(|ty| visit(types, *ty, seen))
                        || types
                            .variant_field_types
                            .get(variant)
                            .is_some_and(|fields| fields.iter().any(|ty| visit(types, *ty, seen)))
                }
                Type::RawList(element) | Type::Sequence(element) => visit(types, *element, seen),
                Type::Intersection(members) => members.iter().any(|ty| visit(types, *ty, seen)),
                _ => false,
            }
        }
        visit(self, ty, &mut HashSet::new())
    }

    pub fn is_copy(&self, ty: TypeId) -> bool {
        matches!(
            self.types[ty],
            Type::Unit
                | Type::Bool
                | Type::Int
                | Type::RawInt
                | Type::Float
                | Type::CodePoint
                | Type::Byte
        ) || matches!(
            self.types[ty],
            Type::Record { record, .. }
                if Some(record) == self.core.symbol
        )
    }

    pub fn expression_type(&self, expression: ExprId) -> Option<TypeId> {
        self.expressions.get(&expression).copied()
    }

    pub fn local_type(&self, local: LocalId) -> Option<TypeId> {
        self.locals.get(&local).copied()
    }

    pub fn function_type(&self, function: FunctionId) -> Option<&FunctionType> {
        self.functions.get(&function)
    }

    pub fn method_dispatch_key(&self, function: FunctionId, name: &str) -> Option<MethodKey> {
        let signature = self.function_type(function)?;
        let mut generics = HashMap::new();
        Some(
            MethodKey {
                name: name.to_owned(),
                parameters: signature
                    .parameters
                    .iter()
                    .skip(1)
                    .map(|parameter| {
                        (
                            parameter.mode,
                            self.dispatch_type_key_with_generics(parameter.ty, &mut generics),
                        )
                    })
                    .collect(),
            }
            .canonical(),
        )
    }

    pub fn dispatch_type_key(&self, ty: TypeId) -> DispatchTypeKey {
        crate::dispatch::canonical(&[self.dispatch_type_key_with_generics(ty, &mut HashMap::new())])
            .remove(0)
    }

    fn dispatch_type_key_with_generics(
        &self,
        ty: TypeId,
        generics: &mut HashMap<String, u32>,
    ) -> DispatchTypeKey {
        match &self.types[ty] {
            Type::Generic(name) => {
                let next = generics.len() as u32;
                DispatchTypeKey::Generic(*generics.entry(name.clone()).or_insert(next))
            }
            Type::Never => DispatchTypeKey::Never,
            Type::Unit => DispatchTypeKey::Unit,
            Type::Bool => DispatchTypeKey::Bool,
            Type::Int => DispatchTypeKey::Int,
            Type::RawInt => DispatchTypeKey::RawInt,
            Type::Float => DispatchTypeKey::Float,
            Type::CodePoint => DispatchTypeKey::CodePoint,
            Type::Byte => DispatchTypeKey::Byte,
            Type::RawBytes => DispatchTypeKey::RawBytes,
            Type::RawByteBuffer => DispatchTypeKey::RawByteBuffer,
            Type::Reference { value, .. } => DispatchTypeKey::Reference(Box::new(
                self.dispatch_type_key_with_generics(*value, generics),
            )),
            Type::RawList(value) => DispatchTypeKey::RawList(Box::new(
                self.dispatch_type_key_with_generics(*value, generics),
            )),
            Type::Sequence(value) => DispatchTypeKey::Sequence(Box::new(
                self.dispatch_type_key_with_generics(*value, generics),
            )),
            Type::Remote(value) => DispatchTypeKey::Remote(Box::new(
                self.dispatch_type_key_with_generics(*value, generics),
            )),
            Type::Future(value) => DispatchTypeKey::Future(Box::new(
                self.dispatch_type_key_with_generics(*value, generics),
            )),
            Type::Function(function) => DispatchTypeKey::Function(
                function
                    .parameters
                    .iter()
                    .map(|parameter| {
                        (
                            parameter.mode,
                            self.dispatch_type_key_with_generics(parameter.ty, generics),
                        )
                    })
                    .collect(),
                Box::new(self.dispatch_type_key_with_generics(function.result, generics)),
            ),
            Type::Record { record, arguments } => DispatchTypeKey::Record(
                *record,
                arguments
                    .iter()
                    .map(|argument| self.dispatch_type_key_with_generics(*argument, generics))
                    .collect(),
            ),
            Type::Intersection(members) => DispatchTypeKey::Intersection(
                members
                    .iter()
                    .map(|member| self.dispatch_type_key_with_generics(*member, generics))
                    .collect(),
            ),
            Type::Variant { variant, arguments } => DispatchTypeKey::Variant(
                *variant,
                arguments
                    .iter()
                    .map(|argument| self.dispatch_type_key_with_generics(*argument, generics))
                    .collect(),
            ),
            Type::Module(name) => DispatchTypeKey::Module(name.clone()),
        }
    }

    pub fn resolved_call(&self, callee: ExprId) -> Option<&ResolvedCall> {
        self.resolved_calls.get(&callee)
    }

    pub fn resolved_function_for_callee(&self, callee: ExprId) -> Option<FunctionId> {
        self.resolved_call(callee).and_then(ResolvedCall::function)
    }

    pub fn display(&self, ty: TypeId) -> String {
        match &self.types[ty] {
            Type::Generic(name) => name.clone(),
            Type::Never => "Never".into(),
            Type::Unit => "()".into(),
            Type::Bool => "Bool".into(),
            Type::Int => "Int".into(),
            Type::RawInt => "RawInt".into(),
            Type::Float => "Float".into(),
            Type::CodePoint => "CodePoint".into(),
            Type::Byte => "Byte".into(),
            Type::RawBytes => "RawBytes".into(),
            Type::RawByteBuffer => "RawByteBuffer".into(),
            Type::Reference { group, value } => {
                format!("ref[{group}] {}", self.display(*value))
            }
            Type::RawList(element) => format!("RawList<{}>", self.display(*element)),
            Type::Sequence(element) => format!("Sequence<{}>", self.display(*element)),
            Type::Remote(value) => format!("Remote<{}>", self.display(*value)),
            Type::Future(value) => format!("Future<{}>", self.display(*value)),
            Type::Function(function) => {
                let effects = display_effects(&function.effects, function.suspends);
                format!(
                    "func({}) -> {}{effects}",
                    function
                        .parameters
                        .iter()
                        .map(|parameter| match parameter.mode {
                            ast::ParameterMode::Borrow => self.display(parameter.ty),
                            ast::ParameterMode::Consume => {
                                format!("consume {}", self.display(parameter.ty))
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(", "),
                    self.display(function.result),
                )
            }
            Type::Record { record, arguments } => {
                let name = self
                    .record_names
                    .get(record)
                    .cloned()
                    .unwrap_or_else(|| format!("record {record:?}"));
                if arguments.is_empty() {
                    name
                } else {
                    format!(
                        "{name}<{}>",
                        arguments
                            .iter()
                            .map(|argument| self.display(*argument))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            }
            Type::Intersection(members) => members
                .iter()
                .map(|member| self.display(*member))
                .collect::<Vec<_>>()
                .join(" & "),
            Type::Variant { variant, arguments } => {
                let name = self
                    .variant_names
                    .get(variant)
                    .cloned()
                    .unwrap_or_else(|| format!("variant#{variant:?}"));
                if arguments.is_empty() {
                    name
                } else {
                    format!(
                        "{name}<{}>",
                        arguments
                            .iter()
                            .map(|a| self.display(*a))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            }
            Type::Module(name) => format!("module {name}"),
        }
    }
}

fn display_effects(effects: &[ast::Effect], suspends: bool) -> String {
    let mut entries = effects
        .iter()
        .map(|effect| {
            let kind = match effect.kind {
                ast::EffectKind::Read => "read",
                ast::EffectKind::Mut => "mut",
                ast::EffectKind::Reshape => "reshape",
                ast::EffectKind::Consume => "consume",
            };
            format!("{kind} {}", effect.target)
        })
        .collect::<Vec<_>>();
    if suspends {
        entries.push("suspend".into());
    }
    if entries.is_empty() {
        String::new()
    } else {
        format!(" [{}]", entries.join(", "))
    }
}
