use crate::hir;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ArmFlow {
    pub yields_value: bool,
    pub falls_through: bool,
}

pub(crate) fn summarize_arm(body: &crate::block::Block<hir::Stmt>) -> ArmFlow {
    let mut flow = ArmFlow::default();
    let mut reachable = true;
    let last = body.len().checked_sub(1);

    for (index, statement) in body.iter().enumerate() {
        if !reachable {
            break;
        }
        let guarded = match statement {
            hir::Stmt::Return { guard, .. } => guard.is_some(),
            hir::Stmt::Break { guard } => guard.is_some(),
            hir::Stmt::Continue { guard } => guard.is_some(),
            hir::Stmt::Expr(_) if Some(index) == last => {
                flow.yields_value = true;
                true
            }
            _ => true,
        };
        if !guarded {
            reachable = false;
        }
    }

    if body.is_empty() {
        flow.yields_value = true;
    } else if reachable && !flow.yields_value {
        flow.falls_through = true;
    }
    flow
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct NodeId(pub usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BranchNode {
    Test {
        arm: usize,
        matched: NodeId,
        unmatched: Option<NodeId>,
    },
    Body {
        arm: usize,
        completed: Option<NodeId>,
    },
    Exit,
}

#[derive(Debug, Clone)]
pub(crate) struct BranchCfg {
    nodes: Vec<BranchNode>,
    entry: NodeId,
    exit: NodeId,
}

impl BranchCfg {
    pub fn new(arms: &[hir::BranchArm]) -> Self {
        let arm_count = arms.len();
        let exit = NodeId(arm_count * 2);
        let mut nodes = Vec::with_capacity(exit.0 + 1);
        for (arm, branch_arm) in arms.iter().enumerate() {
            let next = if arm + 1 < arm_count {
                NodeId((arm + 1) * 2)
            } else {
                exit
            };
            nodes.push(BranchNode::Test {
                arm,
                matched: NodeId(arm * 2 + 1),
                unmatched: (!matches!(branch_arm.test, hir::BranchTest::Wildcard)).then_some(next),
            });
            nodes.push(BranchNode::Body {
                arm,
                completed: summarize_arm(&branch_arm.body).yields_value.then_some(exit),
            });
        }
        nodes.push(BranchNode::Exit);
        Self {
            nodes,
            entry: if arm_count == 0 { exit } else { NodeId(0) },
            exit,
        }
    }

    pub fn entry(&self) -> NodeId {
        self.entry
    }

    pub fn exit(&self) -> NodeId {
        self.exit
    }

    pub fn nodes(&self) -> impl Iterator<Item = (NodeId, BranchNode)> + '_ {
        self.nodes
            .iter()
            .copied()
            .enumerate()
            .map(|(index, node)| (NodeId(index), node))
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LoopCfg {
    pub header: NodeId,
    pub body: NodeId,
    pub exit: NodeId,
}

impl LoopCfg {
    pub fn new() -> Self {
        Self {
            header: NodeId(0),
            body: NodeId(1),
            exit: NodeId(2),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_cfg_routes_completed_bodies_to_the_exit() {
        let arms = [hir::BranchArm {
            test: hir::BranchTest::Wildcard,
            body: crate::block::Block::new(),
        }];
        let cfg = BranchCfg::new(&arms);
        assert_eq!(
            cfg.nodes().nth(cfg.entry().0).unwrap().1,
            BranchNode::Test {
                arm: 0,
                matched: NodeId(1),
                unmatched: None,
            }
        );
        assert_eq!(
            cfg.nodes().nth(1).unwrap().1,
            BranchNode::Body {
                arm: 0,
                completed: Some(cfg.exit()),
            }
        );
    }

    #[test]
    fn branch_cfg_omits_completion_edges_after_unconditional_transfers() {
        let arms = [hir::BranchArm {
            test: hir::BranchTest::Wildcard,
            body: crate::block::Block::single(hir::Stmt::Break { guard: None }, 0..0),
        }];
        let cfg = BranchCfg::new(&arms);
        assert_eq!(
            cfg.nodes().nth(1).unwrap().1,
            BranchNode::Body {
                arm: 0,
                completed: None,
            }
        );
    }
}
