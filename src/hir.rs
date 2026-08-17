use std::collections::{BTreeMap, HashMap};

use la_arena::{Arena, Idx};

use crate::ast;
use crate::error::FosterError;
use crate::package::Package;

mod loans;
mod lower;
mod ownership;
pub(crate) mod queries;
use loans::check_loan_safety;
use ownership::{
    check_capture_invalidation, check_closure_ownership, infer_capture_modes,
    infer_ref_capture_effects, validate_groups_and_effects,
};

pub type ModuleId = Idx<Module>;
pub type FunctionId = Idx<Function>;
pub type ConstantId = Idx<Constant>;
pub type RecordId = Idx<Record>;
pub type VariantTypeId = Idx<VariantType>;
pub type VariantId = Idx<Variant>;
pub type LocalId = Idx<Local>;
pub type ExprId = Idx<Expr>;

#[derive(Debug)]
pub struct Compilation {
    pub package: Package,
    pub hir: PackageHir,
    pub types: crate::types::TypeInformation,
    pub diagnostics: Vec<crate::diagnostic::Diagnostic>,
    pub ownership: crate::ownership::Program,
}

#[derive(Debug, Default)]
pub struct PackageHir {
    pub modules: Arena<Module>,
    pub functions: Arena<Function>,
    pub constants: Arena<Constant>,
    pub records: Arena<Record>,
    pub variant_types: Arena<VariantType>,
    pub variants: Arena<Variant>,
    pub locals: Arena<Local>,
    pub expressions: Arena<Expr>,
    pub expression_spans: HashMap<ExprId, std::ops::Range<usize>>,
    pub expression_functions: HashMap<ExprId, FunctionId>,
    pub modules_by_name: BTreeMap<String, ModuleId>,
}

#[derive(Debug)]
pub struct Module {
    pub name: String,
    pub source_path: Option<camino::Utf8PathBuf>,
    pub imports_with_spans: Vec<ImportBinding>,
    pub functions: BTreeMap<String, FunctionId>,
    pub constants: BTreeMap<String, ConstantId>,
    pub records: BTreeMap<String, RecordId>,
    pub variant_types: BTreeMap<String, VariantTypeId>,
    pub imports: BTreeMap<String, ModuleId>,
}

#[derive(Debug, Clone)]
pub struct Constant {
    pub span: std::ops::Range<usize>,
    pub documentation: Option<String>,
    pub module: ModuleId,
    pub name: String,
    pub public: bool,
    pub value: ConstantValue,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConstantValue {
    Unit,
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(String),
    CodePoint(char),
    Symbol(String),
    List(Vec<ConstantValue>),
    Constant(ConstantId),
}

#[derive(Debug, Clone)]
pub struct ImportBinding {
    pub name: String,
    pub target: ModuleId,
    pub span: std::ops::Range<usize>,
}

#[derive(Debug, Clone)]
pub struct VariantType {
    pub span: std::ops::Range<usize>,
    pub documentation: Option<String>,
    pub module: ModuleId,
    pub name: String,
    pub public: bool,
    pub parameters: Vec<String>,
    pub alternatives: Vec<VariantId>,
}

#[derive(Debug, Clone)]
pub struct Variant {
    pub parent: VariantTypeId,
    pub name: String,
    pub payload: Vec<ast::TypeExpr>,
}

#[derive(Debug, Clone)]
pub struct Record {
    pub span: std::ops::Range<usize>,
    pub documentation: Option<String>,
    pub module: ModuleId,
    pub name: String,
    pub public: bool,
    pub parameters: Vec<String>,
    pub fields: Vec<RecordField>,
}

#[derive(Debug, Clone)]
pub struct RecordField {
    pub name: String,
    pub public: bool,
    pub ty: ast::TypeExpr,
}

#[derive(Debug)]
pub struct Function {
    pub span: std::ops::Range<usize>,
    pub documentation: Option<String>,
    pub module: ModuleId,
    pub name: String,
    pub public: bool,
    pub type_parameters: Vec<String>,
    pub groups: Vec<ast::GroupParameter>,
    pub parameters: Vec<LocalId>,
    pub parameter_types: Vec<Option<ast::TypeExpr>>,
    pub return_type: Option<ast::TypeExpr>,
    pub effects_explicit: bool,
    pub effects: Vec<ast::Effect>,
    pub effect_spans: Vec<std::ops::Range<usize>>,
    pub suspends: bool,
    pub suspend_span: Option<std::ops::Range<usize>>,
    pub body: Vec<Stmt>,
    pub statement_spans: Vec<std::ops::Range<usize>>,
}

#[derive(Debug)]
pub struct Local {
    pub span: std::ops::Range<usize>,
    pub function: FunctionId,
    pub name: String,
    pub kind: LocalKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalKind {
    Parameter,
    Binding,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Return {
        value: ExprId,
        guard: Option<ExprId>,
    },
    Bind {
        local: LocalId,
        value: ExprId,
    },
    Assign {
        local: LocalId,
        value: ExprId,
    },
    Expr(ExprId),
    Set {
        place: ExprId,
        value: ExprId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Place {
    pub root: LocalId,
    pub projections: Vec<Projection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Projection {
    Field(String),
    Index(ExprId),
    Dereference,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Unit,
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(String),
    CodePoint(String),
    Symbol(String),
    List(Vec<ExprId>),
    Name(ResolvedName),
    Call {
        callee: ExprId,
        arguments: Vec<ExprId>,
    },
    Member {
        object: ExprId,
        name: String,
    },
    Index {
        object: ExprId,
        index: ExprId,
    },
    Reference(ExprId),
    MoveOut(ExprId),
    Remote(ExprId),
    Await(ExprId),
    Record {
        record: RecordId,
        fields: Vec<(String, ExprId)>,
    },
    Unary {
        operator: ast::UnaryOp,
        operand: ExprId,
    },
    Binary {
        left: ExprId,
        operator: ast::BinaryOp,
        right: ExprId,
    },
    Branch {
        subject: Option<ExprId>,
        arms: Vec<BranchArm>,
    },
    Closure {
        function: FunctionId,
        captures: Vec<Capture>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capture {
    pub local: LocalId,
    pub mode: CaptureMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureMode {
    Pending,
    Copy,
    Move,
    Ref,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedName {
    Local(LocalId),
    Constant(ConstantId),
    Function(FunctionId),
    Module(ModuleId),
    Builtin(Builtin),
    Record(RecordId),
    Variant(VariantId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builtin {
    Print,
    Println,
    CodePoint,
    FromCodePoint,
    ParseFloat,
    IoReadText,
    IoWriteText,
    IoListDirectory,
    IoExists,
    IoIsFile,
    IoIsDirectory,
    IoJoin,
    IoParent,
    IoFileName,
    IoExtension,
    IoCanonicalize,
    IoCurrentDirectory,
    TcpListen,
    TcpConnect,
    TcpAccept,
    TcpRead,
    TcpWrite,
    TcpSetTimeout,
    TcpCloseListener,
    TcpCloseConnection,
}

#[derive(Debug, Clone)]
pub struct BranchArm {
    pub test: BranchTest,
    pub value: ExprId,
}

#[derive(Debug, Clone)]
pub enum BranchTest {
    Condition(ExprId),
    Wildcard,
    Pattern(Pattern),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
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

impl Compilation {
    pub fn new(package: Package) -> Result<Self, FosterError> {
        let mut hir = PackageHir::lower(&package)?;
        infer_ref_capture_effects(&mut hir);
        validate_groups_and_effects(&hir)?;
        let (initial_types, _) = crate::typecheck::check(&mut hir)?;
        infer_capture_modes(&mut hir, &initial_types)?;
        let (types, diagnostics) = crate::typecheck::check(&mut hir)?;
        validate_groups_and_effects(&hir)?;
        check_closure_ownership(&hir)?;
        check_loan_safety(&hir, &types)?;
        check_capture_invalidation(&hir)?;
        let ownership = crate::ownership::build_and_check(&hir, &types)?;
        Ok(Self {
            package,
            hir,
            types,
            diagnostics,
            ownership,
        })
    }
}
