use std::collections::{HashMap, HashSet};

use la_arena::{Arena, Idx};

use crate::ast;
use crate::hir::{ConstantId, ExprId, FunctionId, LocalId, RecordId, VariantTypeId};

pub type TypeId = Idx<Type>;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    Generic(String),
    Unit,
    Bool,
    Int,
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FunctionType {
    pub parameters: Vec<TypeId>,
    pub parameter_modes: Vec<ast::ParameterMode>,
    pub result: TypeId,
    pub erased: bool,
    pub effects: Vec<ast::Effect>,
    pub suspends: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DispatchTypeKey {
    Generic(u32),
    Unit,
    Bool,
    Int,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DispatchSlot(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NominalTypeId {
    Record(RecordId),
    Variant(VariantTypeId),
}

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

#[derive(Debug, Default)]
pub struct TypeInformation {
    pub types: Arena<Type>,
    pub expressions: HashMap<ExprId, TypeId>,
    pub integer_promotions: HashSet<ExprId>,
    pub resolved_calls: HashMap<ExprId, ResolvedCall>,
    pub dispatch: HashMap<(NominalTypeId, DispatchSlot), FunctionId>,
    pub locals: HashMap<LocalId, TypeId>,
    pub functions: HashMap<FunctionId, FunctionType>,
    pub constants: HashMap<ConstantId, TypeId>,
    pub record_names: HashMap<RecordId, String>,
    pub record_fields: HashMap<RecordId, HashSet<String>>,
    pub record_methods: HashMap<RecordId, HashSet<String>>,
    pub variant_names: HashMap<VariantTypeId, String>,
}

impl TypeInformation {
    pub fn is_copy(&self, ty: TypeId) -> bool {
        matches!(
            self.types[ty],
            Type::Unit | Type::Bool | Type::Int | Type::Float | Type::CodePoint | Type::Byte
        ) || matches!(
            self.types[ty],
            Type::Record { record, .. }
                if self.record_names.get(&record).is_some_and(|name| name == "Symbol")
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
        Some(MethodKey {
            name: name.to_owned(),
            parameters: signature
                .parameters
                .iter()
                .skip(1)
                .zip(signature.parameter_modes.iter().skip(1))
                .map(|(parameter, mode)| {
                    (
                        *mode,
                        self.dispatch_type_key_with_generics(*parameter, &mut generics),
                    )
                })
                .collect(),
        })
    }

    pub fn dispatch_type_key(&self, ty: TypeId) -> DispatchTypeKey {
        self.dispatch_type_key_with_generics(ty, &mut HashMap::new())
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
            Type::Unit => DispatchTypeKey::Unit,
            Type::Bool => DispatchTypeKey::Bool,
            Type::Int => DispatchTypeKey::Int,
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
                    .parameter_modes
                    .iter()
                    .copied()
                    .zip(function.parameters.iter().map(|parameter| {
                        self.dispatch_type_key_with_generics(*parameter, generics)
                    }))
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
            Type::Unit => "()".into(),
            Type::Bool => "Bool".into(),
            Type::Int => "Int".into(),
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
                        .zip(&function.parameter_modes)
                        .map(|(parameter, mode)| match mode {
                            ast::ParameterMode::Borrow => self.display(*parameter),
                            ast::ParameterMode::Consume => {
                                format!("consume {}", self.display(*parameter))
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
