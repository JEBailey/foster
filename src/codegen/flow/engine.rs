//! Worklist orchestration for shared logical flow analysis.
use super::FunctionSchema;
use crate::codegen::{ir::Value, metadata::ProgramMetadata};
use crate::error::FosterError;
use crate::hir::FunctionId;
use std::collections::{HashMap, VecDeque};
mod calls;
mod operations;
mod state;
mod transfer;
mod types;
pub(crate) use operations::Instruction;
pub(crate) use state::FlowState;
use state::merge_state;
use transfer::transfer;
#[cfg(test)]
pub(crate) use types::compatible;
use types::invalid_instruction;
pub(crate) struct Program<'a> {
    pub metadata: &'a ProgramMetadata,
    pub functions: &'a HashMap<FunctionId, FunctionSchema>,
}
/// Slot consumption checks ownership availability; SSA consumption preserves immutable type evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StoragePolicy {
    ConsumingSlots,
    ImmutableValues,
}
pub(crate) struct Body<'a> {
    pub schema: &'a FunctionSchema,
    pub instructions: Vec<Instruction>,
    pub value_count: usize,
    pub entry_values: Vec<Value>,
    pub write_bindings: &'a HashMap<Value, Value>,
    pub storage_policy: StoragePolicy,
}
impl std::ops::Deref for Body<'_> {
    type Target = FunctionSchema;
    fn deref(&self) -> &Self::Target {
        self.schema
    }
}
pub(crate) fn analyze_function_flow(
    program: &Program,
    function: &Body,
) -> Result<Vec<Option<FlowState>>, FosterError> {
    if function.intrinsic_stub {
        return Ok(vec![None; function.instructions.len()]);
    }
    let mut entry = FlowState {
        bindings: vec![None; function.value_count],
        pending_pattern: None,
        boolean_constants: HashMap::new(),
        excluded_variants: HashMap::new(),
    };
    for (index, ty) in function
        .captures
        .iter()
        .chain(function.parameters.iter().map(|p| &p.ty))
        .enumerate()
    {
        entry.bindings[function
            .entry_values
            .get(index)
            .map_or(index, |v| v.index())] = Some(ty.clone());
    }

    let mut states = vec![None; function.instructions.len()];
    states[0] = Some(entry);
    let mut pending = VecDeque::from([0usize]);
    while let Some(index) = pending.pop_front() {
        let state = states[index]
            .clone()
            .expect("queued bytecode instruction has an entry state");
        let successors = transfer(program, function, index, state)?;
        for (successor, incoming) in successors {
            if successor >= function.instructions.len() {
                return invalid_instruction(
                    function,
                    index,
                    "reachable control flow falls off the end",
                );
            }
            match &mut states[successor] {
                Some(current) => {
                    if merge_state(function, successor, current, &incoming)? {
                        pending.push_back(successor);
                    }
                }
                slot @ None => {
                    *slot = Some(incoming);
                    pending.push_back(successor);
                }
            }
        }
    }
    Ok(states)
}
