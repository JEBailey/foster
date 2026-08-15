use std::collections::HashMap;
use std::ops::Range;

use crate::hir::{FunctionId, LocalId, Place};

pub type BlockId = usize;

#[derive(Debug, Default)]
pub struct Program {
    pub functions: HashMap<FunctionId, Function>,
}

#[derive(Debug)]
pub struct Function {
    pub entry: BlockId,
    pub blocks: Vec<BasicBlock>,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UseMode {
    Read,
    Copy,
    Move,
    Borrow,
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
