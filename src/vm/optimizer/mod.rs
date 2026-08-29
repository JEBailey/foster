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
    // Constant-free barrier functions can be set aside safely while independent
    // pure functions are optimized. This is important for owner-qualified
    // intrinsic methods that are installed with bootstrap types but never called.
    let deferred_ids = program
        .functions
        .iter()
        .filter(|(_, function)| {
            optimization_barrier(function)
                && !function
                    .instructions
                    .iter()
                    .any(|instruction| matches!(instruction, Instruction::LoadConstant { .. }))
        })
        .map(|(id, _)| *id)
        .collect::<Vec<_>>();
    let deferred = deferred_ids
        .into_iter()
        .filter_map(|id| program.functions.remove(&id).map(|function| (id, function)))
        .collect::<Vec<_>>();
    if program.functions.values().any(optimization_barrier) {
        program.functions.extend(deferred);
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
    program.functions.extend(deferred);
}

fn optimization_barrier(function: &super::BytecodeFunction) -> bool {
    function.mutable_parameters.iter().any(|mutable| *mutable)
        || function.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::StoreField { .. }
                    | Instruction::StoreIndex { .. }
                    | Instruction::MakeReference { .. }
                    | Instruction::MakeWholeReference { .. }
                    | Instruction::MakeFieldReference { .. }
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
                    | Instruction::CallContractMethod { .. }
            )
        })
}

/// Inserts deterministic register releases after all representational rewrites.
pub(crate) fn insert_drops(program: &mut Program) {
    drops::insert(program);
}
