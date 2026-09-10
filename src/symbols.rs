//! Symbolic, semantic linkage above the VM's program-local IDs.
//!
//! This is a link-time interface, not a native calling convention. Descriptors retain the
//! checked contract; bytecode instructions continue to use resolved function IDs.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    ast,
    compiler::Compilation,
    error::FosterError,
    hir::{FunctionId, ModuleId},
    types::{Type, TypeId},
    vm::{Instruction, Program},
};

pub const FORMAT_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModuleName {
    pub package: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Name {
    pub module: ModuleName,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum SymbolType {
    Primitive(String),
    Generic(u32),
    Nominal(Name, Vec<SymbolType>),
    Reference(String, Box<SymbolType>),
    Applied(String, Vec<SymbolType>),
    Function(Box<Descriptor>),
    Intersection(Vec<SymbolType>),
    Module(ModuleName),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Borrow,
    Consume,
}

impl From<ast::ParameterMode> for Mode {
    fn from(mode: ast::ParameterMode) -> Self {
        match mode {
            ast::ParameterMode::Borrow => Self::Borrow,
            ast::ParameterMode::Consume => Self::Consume,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Parameter {
    pub ty: SymbolType,
    pub mode: Mode,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Effect {
    pub kind: String,
    /// Parameter roots are positional (`p0`); explicit groups are alpha-renamed (`g0`).
    pub root: String,
    pub path: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Descriptor {
    pub generics: u32,
    pub receiver: bool,
    pub parameters: Vec<Parameter>,
    pub result: SymbolType,
    pub groups: Vec<(String, SymbolType)>,
    pub effects: Vec<Effect>,
    pub suspends: bool,
    /// Positional input dependencies proved by ownership MIR. Receiver, when present, is p0.
    pub result_dependencies: Vec<u32>,
    pub fresh_result: bool,
}

impl Descriptor {
    /// Lookup excludes effects, return type, group names, and result provenance. Those are
    /// checked after resolution, so reducing an effect does not make a symbol disappear.
    pub fn overload(&self) -> Vec<Parameter> {
        self.parameters
            .iter()
            .map(|p| Parameter {
                ty: p.ty.lookup_type(),
                mode: p.mode,
            })
            .collect()
    }

    /// Conservative compatibility: invariant types/constraints and ownership modes, with
    /// effect and result-dependency narrowing. No source-level overload resolution is repeated.
    pub fn accepts(&self, implementation: &Self) -> bool {
        self.generics == implementation.generics
            && self.receiver == implementation.receiver
            && self.parameters == implementation.parameters
            && self.result == implementation.result
            && self.groups == implementation.groups
            && (!implementation.suspends || self.suspends)
            && implementation.effects.iter().all(|effect| {
                self.effects.iter().any(|allowed| {
                    allowed.kind == effect.kind
                        && allowed.root == effect.root
                        && effect.path.starts_with(&allowed.path)
                })
            })
            && implementation
                .result_dependencies
                .iter()
                .all(|p| self.result_dependencies.contains(p))
            && (!self.fresh_result || implementation.fresh_result)
    }
}

impl SymbolType {
    fn lookup_type(&self) -> Self {
        match self {
            Self::Reference(_, value) => Self::Reference("_".into(), Box::new(value.lookup_type())),
            Self::Nominal(name, args) => {
                Self::Nominal(name.clone(), args.iter().map(Self::lookup_type).collect())
            }
            Self::Applied(name, args) => {
                Self::Applied(name.clone(), args.iter().map(Self::lookup_type).collect())
            }
            Self::Intersection(args) => {
                Self::Intersection(args.iter().map(Self::lookup_type).collect())
            }
            Self::Function(signature) => {
                let mut signature = (**signature).clone();
                signature.parameters = signature.overload();
                signature.result = signature.result.lookup_type();
                signature.groups.clear();
                signature.effects.clear();
                signature.suspends = false;
                signature.result_dependencies.clear();
                signature.fresh_result = false;
                Self::Function(Box::new(signature))
            }
            other => other.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Symbol {
    pub name: Name,
    pub receiver: bool,
    pub overload: Vec<Parameter>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub symbol: Symbol,
    pub descriptor: Descriptor,
    /// Implementation binding only; never part of symbolic identity.
    pub function: u32,
    /// Implementation spellings in canonical generic-index order; not part of identity.
    pub generic_names: Vec<String>,
    pub public: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Import {
    pub symbol: Symbol,
    pub required: Descriptor,
    pub function: u32,
    pub generic_names: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypeBinding {
    pub name: Name,
    pub variant: bool,
    /// Executable-local nominal ID; the qualified name is the portable identity.
    pub id: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Module {
    pub name: ModuleName,
    pub types: Vec<TypeBinding>,
    pub definitions: Vec<Definition>,
    pub imports: Vec<Import>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Table {
    pub version: u16,
    pub modules: Vec<Module>,
}

impl Default for Table {
    fn default() -> Self {
        Self {
            version: FORMAT_VERSION,
            modules: Vec::new(),
        }
    }
}

fn error(message: impl Into<String>) -> FosterError {
    FosterError::runtime(format!("symbolic linkage: {}", message.into()))
}

pub(crate) fn raw(id: FunctionId) -> u32 {
    id.into_raw().into_u32()
}
fn function_id(id: u32) -> FunctionId {
    la_arena::Idx::from_raw(la_arena::RawIdx::from_u32(id))
}

#[cfg(test)]
mod tests;

mod linking;
mod lowering;
pub use linking::link;
use linking::target;
