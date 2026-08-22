#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub imports: Vec<Import>,
    pub constants: Vec<ConstDecl>,
    pub records: Vec<RecordDecl>,
    pub variants: Vec<VariantDecl>,
    pub functions: Vec<Function>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConstDecl {
    pub span: std::ops::Range<usize>,
    pub documentation: Option<String>,
    pub name: String,
    pub public: bool,
    pub value: Expr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariantDecl {
    pub span: std::ops::Range<usize>,
    pub documentation: Option<String>,
    pub name: String,
    pub public: bool,
    pub parameters: Vec<String>,
    pub alternatives: Vec<VariantAlternative>,
    pub compositions: Vec<TypeExpr>,
    pub methods: Vec<MethodRequirement>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariantAlternative {
    pub name: String,
    pub payload: Vec<TypeExpr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordDecl {
    pub span: std::ops::Range<usize>,
    pub documentation: Option<String>,
    pub name: String,
    pub public: bool,
    pub intrinsic: bool,
    pub parameters: Vec<String>,
    pub compositions: Vec<TypeExpr>,
    pub fields: Vec<RecordField>,
    pub methods: Vec<MethodRequirement>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordField {
    pub name: String,
    pub public: bool,
    pub ty: TypeExpr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodRequirement {
    pub span: std::ops::Range<usize>,
    pub documentation: Option<String>,
    pub name: String,
    pub public: bool,
    pub type_parameters: Vec<String>,
    pub groups: Vec<GroupParameter>,
    pub parameters: Vec<Parameter>,
    pub return_type: Option<TypeExpr>,
    pub effects: Vec<Effect>,
    pub suspends: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    pub span: std::ops::Range<usize>,
    pub path: Vec<String>,
    pub alias: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    pub span: std::ops::Range<usize>,
    pub documentation: Option<String>,
    pub name: String,
    pub public: bool,
    pub intrinsic: Option<String>,
    pub type_parameters: Vec<String>,
    pub groups: Vec<GroupParameter>,
    pub parameters: Vec<Parameter>,
    pub return_type: Option<TypeExpr>,
    pub effects_explicit: bool,
    pub effects: Vec<Effect>,
    pub effect_spans: Vec<std::ops::Range<usize>>,
    pub suspends: bool,
    pub suspend_span: Option<std::ops::Range<usize>>,
    pub body: Vec<Stmt>,
    pub statement_spans: Vec<std::ops::Range<usize>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupParameter {
    pub name: String,
    pub element: TypeExpr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parameter {
    pub name: String,
    pub ty: Option<TypeExpr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeExpr {
    Named(String, Vec<TypeExpr>),
    Intersection(Vec<TypeExpr>),
    Reference {
        group: String,
        value: Box<TypeExpr>,
    },
    Function {
        parameters: Vec<TypeExpr>,
        parameter_modes: Vec<ParameterMode>,
        result: Box<TypeExpr>,
        effects: Vec<Effect>,
        suspends: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParameterMode {
    Borrow,
    Consume,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Effect {
    pub kind: EffectKind,
    pub target: GroupPath,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GroupPath {
    pub root: String,
    pub children: Vec<String>,
}

impl GroupPath {
    pub fn root(name: impl Into<String>) -> Self {
        Self {
            root: name.into(),
            children: Vec::new(),
        }
    }

    pub fn is_root(&self) -> bool {
        self.children.is_empty()
    }

    pub fn child(mut self, child: impl Into<String>) -> Self {
        self.children.push(child.into());
        self
    }

    pub fn with_children(mut self, children: &[String]) -> Self {
        self.children.extend_from_slice(children);
        self
    }

    pub fn covers(&self, actual: &Self) -> bool {
        self.root == actual.root
            && self.children.len() <= actual.children.len()
            && self
                .children
                .iter()
                .zip(&actual.children)
                .all(|(expected, actual)| expected == actual)
    }
}

impl From<String> for GroupPath {
    fn from(root: String) -> Self {
        Self::root(root)
    }
}

impl From<&str> for GroupPath {
    fn from(root: &str) -> Self {
        Self::root(root)
    }
}

impl std::fmt::Display for GroupPath {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.root)?;
        for child in &self.children {
            write!(formatter, ".{child}")?;
        }
        Ok(())
    }
}

impl PartialEq<str> for GroupPath {
    fn eq(&self, other: &str) -> bool {
        let mut parts = other.split('.');
        parts.next() == Some(self.root.as_str())
            && parts.eq(self.children.iter().map(String::as_str))
    }
}

impl PartialEq<&str> for GroupPath {
    fn eq(&self, other: &&str) -> bool {
        self == *other
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EffectKind {
    Read,
    Mut,
    Reshape,
    Consume,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    Return { value: Expr, guard: Option<Expr> },
    Bind { name: String, value: Expr },
    Function(Box<Function>),
    Set { place: Expr, value: Expr },
    Expr(Expr),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Spanned {
        expression: Box<Expr>,
        span: std::ops::Range<usize>,
    },
    Unit,
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(String),
    CodePoint(String),
    Symbol(String),
    Name(String),
    List(Vec<Expr>),
    Call {
        callee: Box<Expr>,
        arguments: Vec<Expr>,
    },
    Member {
        object: Box<Expr>,
        name: String,
    },
    Index {
        object: Box<Expr>,
        index: Box<Expr>,
    },
    Reference(Box<Expr>),
    MoveOut(Box<Expr>),
    Remote(Box<Expr>),
    Await(Box<Expr>),
    Record {
        constructor: Box<Expr>,
        fields: Vec<RecordFieldValue>,
    },
    Unary {
        operator: UnaryOp,
        operand: Box<Expr>,
    },
    Binary {
        left: Box<Expr>,
        operator: BinaryOp,
        right: Box<Expr>,
    },
    Branch {
        subject: Option<Box<Expr>>,
        arms: Vec<BranchArm>,
    },
    Closure {
        captures: Vec<CaptureSpec>,
        parameters: Vec<Parameter>,
        effects: Vec<Effect>,
        suspends: bool,
        body: ClosureBody,
    },
    Placeholder,
}

impl Expr {
    pub fn unspanned(&self) -> &Self {
        match self {
            Self::Spanned { expression, .. } => expression.unspanned(),
            expression => expression,
        }
    }

    pub fn span(&self) -> Option<std::ops::Range<usize>> {
        match self {
            Self::Spanned { span, .. } => Some(span.clone()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecordFieldValue {
    pub name: String,
    pub value: Expr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureSpec {
    pub mode: CaptureMode,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureMode {
    Copy,
    Move,
    Ref,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClosureBody {
    Expression(Box<Expr>),
    Block(Vec<Stmt>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct BranchArm {
    pub test: BranchTest,
    pub value: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BranchTest {
    Condition(Expr),
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
    Binding(String),
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(String),
    CodePoint(String),
    Symbol(String),
    Variant {
        path: Vec<String>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Negate,
    Not,
    BitNot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Subtract,
    Multiply,
    Divide,
    BitAnd,
    BitOr,
    BitXor,
    ShiftLeft,
    ShiftRight,
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
}
