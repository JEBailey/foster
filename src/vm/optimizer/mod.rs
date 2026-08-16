//! Semantics-preserving rewrites over Foster's executable register IR.

use super::{Instruction, Program};

mod analysis;
mod closures;
mod constants;
mod control_flow;
mod copies;
mod drops;
mod inlining;
mod registers;

/// Optimizes a complete bytecode program while retaining one source span per instruction.
pub fn optimize(program: &mut Program) {
    // Ownership, mutation, and concurrency instructions are optimization barriers
    // until the data-flow passes model their aliasing and suspension semantics.
    if program.functions.values().any(|function| {
        function.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::StoreField { .. }
                    | Instruction::StoreIndex { .. }
                    | Instruction::MakeReference { .. }
                    | Instruction::MoveOut { .. }
                    | Instruction::Push { .. }
                    | Instruction::Append { .. }
                    | Instruction::Contains { .. }
                    | Instruction::Builtin { .. }
                    | Instruction::SpawnRemote { .. }
                    | Instruction::SpawnRemoteBorrow { .. }
                    | Instruction::RemoteCall { .. }
                    | Instruction::Await { .. }
                    | Instruction::CallMethod { .. }
            )
        })
    }) {
        return;
    }
    inlining::inline_small_leaf_functions(program);
    constants::fold(program);
    control_flow::simplify(program);
    copies::propagate(program);
    registers::eliminate_dead_writes(program);
    control_flow::simplify(program);
    closures::specialize_non_escaping(program);
    registers::eliminate_dead_writes(program);
    control_flow::simplify(program);
    registers::compact(program);
    control_flow::simplify(program);
    registers::compact(program);
    constants::deduplicate(program);
}

/// Inserts deterministic register releases after all representational rewrites.
pub(crate) fn insert_drops(program: &mut Program) {
    drops::insert(program);
}
