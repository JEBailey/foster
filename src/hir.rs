use std::collections::{BTreeMap, HashMap};

use la_arena::{Arena, Idx};

use crate::ast;
use crate::error::FosterError;
use crate::package::Package;

mod lower;
mod ownership;
pub(crate) mod queries;
pub(crate) mod visit;
use ownership::{
    check_closure_ownership, infer_capture_modes, infer_ref_capture_effects,
    validate_groups_and_effects,
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
    pub tests: Vec<FunctionId>,
    pub expression_spans: HashMap<ExprId, std::ops::Range<usize>>,
    pub expression_functions: HashMap<ExprId, FunctionId>,
    pub modules_by_name: BTreeMap<String, ModuleId>,
}

#[derive(Debug)]
pub struct Module {
    pub name: String,
    pub documentation: Option<String>,
    pub source_path: Option<camino::Utf8PathBuf>,
    pub imports_with_spans: Vec<ImportBinding>,
    pub functions: BTreeMap<String, FunctionId>,
    pub function_overloads: BTreeMap<String, Vec<FunctionId>>,
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
    pub kind: ast::VariantKind,
    pub parameters: Vec<String>,
    pub alternatives: Vec<VariantId>,
    pub compositions: Vec<ast::TypeExpr>,
    pub methods: Vec<ast::MethodRequirement>,
}

#[derive(Debug, Clone)]
pub struct Variant {
    pub span: std::ops::Range<usize>,
    pub parent: VariantTypeId,
    pub member: Option<ast::TypeExpr>,
    pub name: String,
    pub payload: Option<ast::TypeExpr>,
}

#[derive(Debug, Clone)]
pub struct Record {
    pub span: std::ops::Range<usize>,
    pub documentation: Option<String>,
    pub module: ModuleId,
    pub name: String,
    pub public: bool,
    pub intrinsic: bool,
    pub parameters: Vec<String>,
    pub compositions: Vec<ast::TypeExpr>,
    pub fields: Vec<RecordField>,
    pub methods: Vec<ast::MethodRequirement>,
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
    pub owner: Option<String>,
    pub receiver: Option<LocalId>,
    pub test_description: Option<String>,
    pub public: bool,
    pub intrinsic: Option<String>,
    pub type_parameters: Vec<String>,
    pub groups: Vec<ast::GroupParameter>,
    pub parameters: Vec<LocalId>,
    pub parameter_types: Vec<Option<ast::TypeExpr>>,
    pub parameter_type_spans: Vec<Option<std::ops::Range<usize>>>,
    pub return_type: Option<ast::TypeExpr>,
    pub effects_explicit: bool,
    pub effects: Vec<ast::Effect>,
    pub effect_spans: Vec<std::ops::Range<usize>>,
    pub suspends: bool,
    pub suspend_span: Option<std::ops::Range<usize>>,
    pub body: crate::block::Block<Stmt>,
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
    Assert {
        condition: ExprId,
        message: Option<ExprId>,
    },
    Loop {
        body: crate::block::Block<Stmt>,
    },
    Break {
        guard: Option<ExprId>,
    },
    Continue {
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
    Index {
        expression: ExprId,
        constant: Option<i64>,
    },
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
    Try {
        value: ExprId,
        binding: LocalId,
    },
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
    FormatFloat,
    ByteValid,
    ByteUnchecked,
    BytesEmpty,
    BytesFromList,
    BytesConcat,
    BytesSlice,
    BytesToList,
    BytesHex,
    BytesFromHex,
    StringUtf8,
    BytesUtf8Valid,
    BytesDecodeUtf8,
    ByteBufferEmpty,
    ByteBufferWithCapacity,
    ByteBufferPush,
    ByteBufferExtend,
    ByteBufferClear,
    ByteBufferTruncate,
    ByteBufferReserve,
    ByteBufferFreeze,
    ByteBufferSnapshot,
    IoReadText,
    IoWriteText,
    IoReadBytes,
    IoWriteBytes,
    IoListDirectory,
    IoExists,
    IoIsFile,
    IoIsDirectory,
    IoCreateDirectory,
    IoCreateDirectoryAll,
    IoRemoveFile,
    IoRemoveDirectory,
    IoRename,
    IoCopyFile,
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
    TcpReadBytes,
    TcpWriteBytes,
    TcpSetTimeout,
    TcpCloseListener,
    TcpCloseConnection,
}

#[derive(Debug, Clone)]
pub struct BranchArm {
    pub test: BranchTest,
    pub body: crate::block::Block<Stmt>,
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
        let diagnostic_package = package.clone();
        Self::build(package)
            .map_err(|error| diagnostic_package.locate_compiler_error(FosterError::from(error)))
    }

    fn build(package: Package) -> Result<Self, crate::error::CompileError> {
        use crate::error::CompileError;

        let mut hir = PackageHir::lower(&package).map_err(CompileError::lowering)?;
        infer_ref_capture_effects(&mut hir);
        validate_groups_and_effects(&hir).map_err(CompileError::effects)?;
        let (initial_types, _) = crate::typecheck::check(&mut hir).map_err(CompileError::types)?;
        infer_capture_modes(&mut hir, &initial_types).map_err(CompileError::ownership)?;
        let (types, diagnostics) =
            crate::typecheck::check(&mut hir).map_err(CompileError::types)?;
        validate_groups_and_effects(&hir).map_err(CompileError::effects)?;
        check_closure_ownership(&hir).map_err(CompileError::ownership)?;
        let ownership =
            crate::ownership::build_and_check(&hir, &types).map_err(CompileError::ownership)?;
        let compilation = Self {
            package,
            hir,
            types,
            diagnostics,
            ownership,
        };
        if let Some(main) = compilation
            .hir
            .module_named("main")
            .and_then(|module| compilation.hir.function_named(module, "main"))
        {
            crate::entry::accepts_arguments(&compilation, main).map_err(CompileError::types)?;
        }
        Ok(compilation)
    }
}
