use std::collections::HashMap;
use std::ops::Range;

use crate::hir::{FunctionId, LocalId, Projection};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TemporaryId(pub usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlaceRoot {
    Local(LocalId),
    Temporary(TemporaryId),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Place {
    pub root: PlaceRoot,
    pub projections: Vec<Projection>,
}

impl Place {
    pub fn local(local: LocalId) -> Self {
        Self {
            root: PlaceRoot::Local(local),
            projections: Vec::new(),
        }
    }

    pub fn temporary(temporary: TemporaryId) -> Self {
        Self {
            root: PlaceRoot::Temporary(temporary),
            projections: Vec::new(),
        }
    }

    pub fn from_hir(place: crate::hir::Place) -> Self {
        Self {
            root: PlaceRoot::Local(place.root),
            projections: place.projections,
        }
    }

    pub fn local_root(&self) -> Option<LocalId> {
        match self.root {
            PlaceRoot::Local(local) => Some(local),
            PlaceRoot::Temporary(_) => None,
        }
    }
}

pub(crate) fn place_contains(parent: &Place, child: &Place) -> bool {
    parent.root == child.root
        && parent.projections.len() <= child.projections.len()
        && parent
            .projections
            .iter()
            .zip(&child.projections)
            .all(|(left, right)| projections_equal(left, right))
}

pub(crate) fn places_overlap(left: &Place, right: &Place) -> bool {
    if left.root != right.root {
        return false;
    }
    for (left, right) in left.projections.iter().zip(&right.projections) {
        match (left, right) {
            (Projection::Field(left), Projection::Field(right)) if left != right => return false,
            (
                Projection::Index {
                    constant: Some(left),
                    ..
                },
                Projection::Index {
                    constant: Some(right),
                    ..
                },
            ) if left != right => return false,
            (Projection::Field(_), Projection::Field(_))
            | (Projection::Index { .. }, Projection::Index { .. })
            | (Projection::Dereference, Projection::Dereference) => {}
            _ => return true,
        }
    }
    true
}

fn projections_equal(left: &Projection, right: &Projection) -> bool {
    match (left, right) {
        (Projection::Field(left), Projection::Field(right)) => left == right,
        (
            Projection::Index {
                expression: left_expression,
                constant: left_constant,
            },
            Projection::Index {
                expression: right_expression,
                constant: right_constant,
            },
        ) => match (left_constant, right_constant) {
            (Some(left), Some(right)) => left == right,
            _ => left_expression == right_expression,
        },
        (Projection::Dereference, Projection::Dereference) => true,
        _ => false,
    }
}

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

impl Program {
    /// Deterministic ownership report suitable for diagnostics, golden tests,
    /// and bug reports. It deliberately avoids `HashMap` debug formatting.
    pub fn debug_dump(&self, hir: &crate::hir::PackageHir) -> String {
        use std::fmt::Write;

        let mut functions = self.functions.keys().copied().collect::<Vec<_>>();
        functions.sort_by_key(|id| {
            let function = &hir.functions[*id];
            (
                hir.modules[function.module].name.clone(),
                function.name.clone(),
            )
        });
        let mut output = format!(
            "foster-language={} ownership-model={}\n",
            super::LANGUAGE_VERSION,
            super::MODEL_VERSION
        );
        for id in functions {
            let definition = &hir.functions[id];
            let _ = writeln!(
                output,
                "\nfunction {}.{}",
                hir.modules[definition.module].name, definition.name
            );
            let function = &self.functions[&id];
            let _ = writeln!(output, "  result {:?}", function.result_provenance);
            for loan in &function.loans {
                let mut parents = loan.parents.iter().map(|loan| loan.0).collect::<Vec<_>>();
                parents.sort_unstable();
                let _ = writeln!(
                    output,
                    "  loan L{} origin={} issued=b{}:o{} parents={parents:?}",
                    loan.id.0,
                    place_label(hir, &loan.origin),
                    loan.issued_at.block,
                    loan.issued_at.operation,
                );
            }
            for (block, basic_block) in function.blocks.iter().enumerate() {
                let _ = writeln!(output, "  block b{block}");
                for (operation, value) in basic_block.operations.iter().enumerate() {
                    let _ = writeln!(output, "    o{operation} {value:?}");
                }
                let _ = writeln!(output, "    -> {:?}", basic_block.terminator);
            }
            if let Some(requirements) = self.requirements.get(&id) {
                for loan in &function.loans {
                    let mut points = Vec::new();
                    for (block, states) in requirements.points.iter().enumerate() {
                        let Some(states) = states else { continue };
                        for (operation, state) in states.iter().enumerate() {
                            if state.loans.contains_key(&loan.id) {
                                points.push(format!("b{block}:o{operation}"));
                            }
                        }
                    }
                    let _ = writeln!(
                        output,
                        "  region L{} = {{{}}}",
                        loan.id.0,
                        points.join(", ")
                    );
                }
            }
        }
        output
    }
}

fn place_label(hir: &crate::hir::PackageHir, place: &Place) -> String {
    let mut label = match place.root {
        PlaceRoot::Local(local) => hir.locals[local].name.clone(),
        PlaceRoot::Temporary(temporary) => format!("temporary#{}", temporary.0),
    };
    for projection in &place.projections {
        match projection {
            crate::hir::Projection::Field(field) => {
                label.push('.');
                label.push_str(field);
            }
            crate::hir::Projection::Index { constant, .. } => match constant {
                Some(index) => label.push_str(&format!("[{index}]")),
                None => label.push_str("[?]"),
            },
            crate::hir::Projection::Dereference => label.push_str(".*"),
        }
    }
    label
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
        place: Place,
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
    /// Semantic destruction at a full-expression or function-scope boundary.
    /// This is a no-op for an uninitialized or already-moved root. Runtime
    /// last-use drops may occur earlier when observationally equivalent.
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
    /// Terminates the current invocation with a runtime failure after its
    /// active ownership scopes have been destroyed.
    Fail,
}

impl Terminator {
    pub(crate) fn successors(&self) -> &[BlockId] {
        match self {
            Self::Goto(target) => std::slice::from_ref(target),
            Self::Branch(targets) => targets,
            Self::Unreachable | Self::Return | Self::Fail => &[],
        }
    }
}
