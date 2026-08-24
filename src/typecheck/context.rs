use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Ty {
    Variable(u32),
    Generic(String),
    Unit,
    Bool,
    Int,
    Float,
    CodePoint,
    Byte,
    RawBytes,
    RawByteBuffer,
    RawList(Box<Ty>),
    Sequence(Box<Ty>),
    Remote(Box<Ty>),
    Future(Box<Ty>),
    Function(Vec<Ty>, Box<Ty>),
    Callable {
        parameters: Vec<Ty>,
        parameter_modes: Vec<crate::ast::ParameterMode>,
        result: Box<Ty>,
        erased: bool,
        effects: Vec<crate::ast::Effect>,
        suspends: bool,
    },
    Reference(String, Box<Ty>),
    Record(RecordId, Vec<Ty>),
    Intersection(Vec<Ty>),
    Variant(VariantTypeId, Vec<Ty>),
    Module(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NominalType {
    Record(RecordId),
    Variant(VariantTypeId),
}

#[derive(Debug, Clone)]
pub(super) struct Signature {
    pub(super) parameters: Vec<Ty>,
    pub(super) parameter_modes: Vec<crate::ast::ParameterMode>,
    pub(super) result: Ty,
}

#[derive(Debug, Clone)]
pub(super) struct MemberConstraint {
    pub(super) function: FunctionId,
    pub(super) receiver: Ty,
    pub(super) name: String,
    pub(super) result: Ty,
}

pub(super) struct Checker<'a> {
    pub(super) hir: &'a hir::PackageHir,
    pub(super) next_variable: u32,
    pub(super) substitutions: HashMap<u32, Ty>,
    pub(super) functions: HashMap<FunctionId, Signature>,
    pub(super) constants: HashMap<ConstantId, Ty>,
    pub(super) locals: HashMap<LocalId, Ty>,
    pub(super) local_groups: HashMap<LocalId, String>,
    pub(super) expressions: HashMap<ExprId, Ty>,
    pub(super) variant_injections: HashMap<ExprId, hir::VariantId>,
    pub(super) extension_methods: HashMap<ExprId, FunctionId>,
    pub(super) member_constraints: Vec<MemberConstraint>,
    pub(super) diagnostics: Vec<crate::diagnostic::Diagnostic>,
    pub(super) inferred_effects: InferredEffects,
}
