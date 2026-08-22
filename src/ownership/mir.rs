use std::collections::HashMap;
use std::ops::Range;

use crate::hir::{FunctionId, LocalId, Place};

pub type BlockId = usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LoanId(pub usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MirPoint {
    pub block: BlockId,
    pub operation: usize,
}

#[derive(Debug, Default)]
pub struct Program {
    pub functions: HashMap<FunctionId, Function>,
    pub provenance: HashMap<FunctionId, ProvenanceAnalysis>,
    pub requirements: HashMap<FunctionId, RequirementAnalysis>,
}

#[derive(Debug)]
pub struct Function {
    pub entry: BlockId,
    pub blocks: Vec<BasicBlock>,
    pub loans: Vec<LoanDefinition>,
    pub result_provenance: ResultProvenance,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResultProvenance {
    pub parameters: Vec<usize>,
    pub receiver: bool,
    pub fresh_owned: bool,
}

#[derive(Debug, Clone)]
pub struct LoanDefinition {
    pub id: LoanId,
    pub origin: Place,
    pub issued_at: MirPoint,
    /// Loans contained by the place from which this loan was derived. There
    /// may be more than one after a control-flow join.
    pub parents: std::collections::HashSet<LoanId>,
    pub span: Range<usize>,
}

#[derive(Debug, Clone)]
pub enum BorrowValue {
    Empty,
    Loan(LoanId),
    /// A newly issued loan that flattens to loans already contained by its
    /// immediate origin, or remains itself when borrowing owned storage.
    Reborrow {
        loan: LoanId,
        origin: Place,
    },
    Place(Place),
    MovePlace(Place),
    Merge(Vec<BorrowValue>),
    Fields(Vec<(Vec<crate::hir::Projection>, BorrowValue)>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidationKind {
    Reshape,
    Consume,
    Replace,
}

#[derive(Debug, Clone, Default)]
pub struct ProvenanceAnalysis {
    pub entries: Vec<Option<ProvenanceState>>,
    pub exits: Vec<Option<ProvenanceState>>,
    /// State before each operation, followed by the state after the block's
    /// final operation. Unreachable blocks have no point states.
    pub points: Vec<Option<Vec<ProvenanceState>>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProvenanceState {
    pub contents: HashMap<Place, std::collections::HashSet<LoanId>>,
}

#[derive(Debug, Clone, Default)]
pub struct RequirementAnalysis {
    pub entries: Vec<Option<RequirementState>>,
    pub exits: Vec<Option<RequirementState>>,
    /// State before each operation, followed by the state after the block's
    /// final operation. Unreachable blocks have no point states.
    pub points: Vec<Option<Vec<RequirementState>>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequirementState {
    /// Each required loan maps to a representative later use that keeps its
    /// region live. This is also the third site in invalidation diagnostics.
    pub loans: HashMap<LoanId, RequiredUse>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredUse {
    pub place: Place,
    pub mode: UseMode,
    pub span: Range<usize>,
}

#[derive(Debug, Default)]
pub struct BasicBlock {
    pub operations: Vec<Operation>,
    pub terminator: Terminator,
}

#[derive(Debug, Clone)]
pub enum Operation {
    Use {
        place: Place,
        mode: UseMode,
        span: Range<usize>,
    },
    Initialize {
        local: LocalId,
        span: Range<usize>,
    },
    StoreBorrower {
        destination: Place,
        value: BorrowValue,
        span: Range<usize>,
    },
    ReturnBorrower {
        value: BorrowValue,
        kind: ReturnKind,
        span: Range<usize>,
    },
    Invalidate {
        place: Place,
        kind: InvalidationKind,
        span: Range<usize>,
    },
    Suspend {
        span: Range<usize>,
    },
    /// Semantic destruction at function-scope exit. Runtime last-use drops
    /// may occur earlier when they are observationally equivalent.
    Destroy {
        place: Place,
        span: Range<usize>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReturnKind {
    Reference,
    Closure,
    Aggregate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UseMode {
    Read,
    Copy,
    Move,
    Borrow,
    /// Access needed only to replace a destination. Existing borrower
    /// contents are not inspected and therefore do not extend their regions.
    Write,
    Call,
}

#[derive(Debug, Clone, Default)]
pub enum Terminator {
    #[default]
    Unreachable,
    Goto(BlockId),
    Branch(Vec<BlockId>),
    Return,
}

impl Terminator {
    pub(crate) fn successors(&self) -> &[BlockId] {
        match self {
            Self::Goto(target) => std::slice::from_ref(target),
            Self::Branch(targets) => targets,
            Self::Unreachable | Self::Return => &[],
        }
    }
}
