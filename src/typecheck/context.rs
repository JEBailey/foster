use super::transactions::{JournalMap, JournalSet};
use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[cfg_attr(test, derive(Clone))]
pub(super) struct Checker<'a> {
    pub(super) body_cache: Option<incremental::SharedBodyCache>,
    pub(super) body_cacheable: bool,
    pub(super) record_fields_cache:
        JournalMap<(RecordId, Vec<Ty>), Vec<composition::EffectiveField>>,
    pub(super) record_methods_cache:
        JournalMap<(RecordId, Vec<Ty>), Vec<composition::EffectiveMethod>>,
    pub(super) checked_requirements: JournalSet<(RecordId, Vec<Ty>, RecordId, usize)>,
    pub(super) hir: &'a hir::PackageHir,
    pub(super) next_variable: u32,
    pub(super) substitutions: substitutions::Substitutions,
    pub(super) functions: JournalMap<FunctionId, Signature>,
    pub(super) constants: JournalMap<ConstantId, Ty>,
    pub(super) locals: JournalMap<LocalId, Ty>,
    pub(super) local_groups: JournalMap<LocalId, String>,
    pub(super) expressions: JournalMap<ExprId, Ty>,
    pub(super) integer_promotions: JournalSet<ExprId>,
    pub(super) member_kinds: JournalMap<ExprId, crate::semantics::MemberKind>,
    pub(super) bare_method_members: JournalSet<ExprId>,
    pub(super) resolved_calls: JournalMap<ExprId, crate::types::ResolvedCall>,
    pub(super) dispatch_slots: JournalMap<MethodKey, DispatchSlot>,
    pub(super) dispatch_keys: Vec<MethodKey>,
    pub(super) member_constraints: Vec<MemberConstraint>,
    pub(super) diagnostics: Vec<crate::diagnostic::Diagnostic>,
    pub(super) derived_effects: DerivedEffects,
    pub(super) effect_seeds: HashMap<FunctionId, effect_worklist::EffectSummary>,
    pub(super) effect_dependencies: HashMap<FunctionId, HashSet<FunctionId>>,
    pub(super) resolving_aliases: Vec<hir::VariantTypeId>,
}
