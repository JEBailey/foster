//! Program-local identities and executable pattern contracts.
use la_arena::Idx;
#[derive(Debug)]
pub enum Function {}
pub type FunctionId = Idx<Function>;
#[derive(Debug)]
pub enum Record {}
pub type RecordId = Idx<Record>;
#[derive(Debug)]
pub enum VariantType {}
pub type VariantTypeId = Idx<VariantType>;
#[derive(Debug)]
pub enum Variant {}
pub type VariantId = Idx<Variant>;
#[derive(Debug)]
pub enum Local {}
pub type LocalId = Idx<Local>;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureMode {
    Pending,
    Copy,
    Move,
    Ref,
}
#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    Record {
        fields: Vec<(String, Pattern)>,
    },
    IsType {
        target: crate::codegen::types::ExecutableType,
        source: crate::codegen::types::ExecutableType,
        conforming: Vec<crate::codegen::types::ExecutableType>,
        binding: Option<LocalId>,
    },
    Spanned {
        pattern: Box<Pattern>,
        span: std::ops::Range<usize>,
    },
    Wildcard,
    Binding(LocalId),
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(String),
    CodePoint(String),
    Symbol(String),
    Variant {
        variant: VariantId,
        fields: Vec<Pattern>,
    },
}

impl Pattern {
    pub fn binding_locals(&self, result: &mut Vec<LocalId>) {
        match self.unspanned() {
            Self::Binding(local)
            | Self::IsType {
                binding: Some(local),
                ..
            } => result.push(*local),
            Self::Record { fields } => {
                for (_, pattern) in fields {
                    pattern.binding_locals(result);
                }
            }
            Self::Variant { fields, .. } => {
                for pattern in fields {
                    pattern.binding_locals(result);
                }
            }
            _ => {}
        }
    }

    pub fn unspanned(&self) -> &Self {
        match self {
            Self::Spanned { pattern, .. } => pattern.unspanned(),
            pattern => pattern,
        }
    }

    pub fn span(&self) -> Option<std::ops::Range<usize>> {
        match self {
            Self::Spanned { span, .. } => Some(span.clone()),
            _ => None,
        }
    }
}
