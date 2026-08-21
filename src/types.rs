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
    Bytes,
    ByteBuffer,
    Symbol,
    Reference {
        group: String,
        value: TypeId,
    },
    List(TypeId),
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

#[derive(Debug, Default)]
pub struct TypeInformation {
    pub types: Arena<Type>,
    pub expressions: HashMap<ExprId, TypeId>,
    pub locals: HashMap<LocalId, TypeId>,
    pub functions: HashMap<FunctionId, FunctionType>,
    pub constants: HashMap<ConstantId, TypeId>,
    pub record_names: HashMap<RecordId, String>,
    pub record_fields: HashMap<RecordId, HashSet<String>>,
    pub record_properties: HashMap<RecordId, HashSet<String>>,
    pub record_methods: HashMap<RecordId, HashSet<String>>,
    pub variant_names: HashMap<VariantTypeId, String>,
}

impl TypeInformation {
    pub fn is_copy(&self, ty: TypeId) -> bool {
        matches!(
            self.types[ty],
            Type::Unit
                | Type::Bool
                | Type::Int
                | Type::Float
                | Type::CodePoint
                | Type::Byte
                | Type::Symbol
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

    pub fn display(&self, ty: TypeId) -> String {
        match &self.types[ty] {
            Type::Generic(name) => name.clone(),
            Type::Unit => "Unit".into(),
            Type::Bool => "Bool".into(),
            Type::Int => "Int".into(),
            Type::Float => "Float".into(),
            Type::CodePoint => "CodePoint".into(),
            Type::Byte => "Byte".into(),
            Type::Bytes => "Bytes".into(),
            Type::ByteBuffer => "ByteBuffer".into(),
            Type::Symbol => "Symbol".into(),
            Type::Reference { group, value } => {
                format!("ref[{group}] {}", self.display(*value))
            }
            Type::List(element) => format!("List<{}>", self.display(*element)),
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
