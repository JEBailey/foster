//! Differential oracle comparing construction-slot and SSA use-site facts.
use super::SealingEvidence;
use crate::codegen::LowerError;
use crate::codegen::flow::PointFacts;
use crate::codegen::flow::{FunctionFacts, FunctionSchema};
use crate::codegen::ir::{self, Block};
use crate::codegen::storage;
use crate::hir::FunctionId;
use std::collections::HashMap;
#[cfg(test)]
fn construction_facts(
    program: &storage::Program,
    body: &storage::Function,
    shared: &ir::Function,
    sources: &SealingEvidence,
    schemas: &HashMap<FunctionId, FunctionSchema>,
) -> Result<FunctionFacts, LowerError> {
    let states = storage::verification::type_states(program, body, schemas)
        .map_err(|e| LowerError(e.to_string()))?;
    let mut values = vec![std::collections::BTreeSet::new(); shared.values.len()];
    let points = shared
        .blocks
        .iter()
        .enumerate()
        .map(|(block_index, block)| {
            sources.sites[block_index]
                .iter()
                .enumerate()
                .map(|(instruction_index, source)| {
                    let Some(state) = &states[*source] else {
                        return PointFacts::Unreachable;
                    };
                    let operands =
                        if let Some(instruction) = block.instructions.get(instruction_index) {
                            instruction.operands()
                        } else {
                            match &block.terminator {
                                ir::Terminator::Unreachable => vec![],
                                ir::Terminator::Return(value) => vec![*value],
                                ir::Terminator::Jump { arguments, .. } => arguments.clone(),
                                ir::Terminator::Branch {
                                    condition,
                                    then_arguments,
                                    else_arguments,
                                    ..
                                } => std::iter::once(*condition)
                                    .chain(then_arguments.iter().copied())
                                    .chain(else_arguments.iter().copied())
                                    .collect(),
                            }
                        };
                    let mut available = HashMap::new();
                    for value in operands {
                        let ty = sources.bindings[source]
                            .iter()
                            .enumerate()
                            .find_map(|(home, binding)| {
                                (*binding == Some(value))
                                    .then(|| state[home].as_ref())
                                    .flatten()
                            })
                            .or_else(|| {
                                shared
                                    .values
                                    .hint(value.index())
                                    .and_then(|home| state[usize::from(home)].as_ref())
                            });
                        available.insert(value, ty.cloned());
                        if let Some(ty) = ty {
                            values[value.index()].insert(ty.clone());
                        }
                    }
                    PointFacts::Reachable(available)
                })
                .collect()
        })
        .collect();
    Ok(FunctionFacts {
        points,
        values: values
            .into_iter()
            .map(|types| types.into_iter().collect())
            .collect(),
    })
}

#[cfg(test)]
pub(super) fn compare_flow(
    program: &storage::Program,
    body: &storage::Function,
    shared: &ir::Function,
    sources: &SealingEvidence,
    actual: &FunctionFacts,
    schemas: &HashMap<FunctionId, FunctionSchema>,
) -> Result<(), LowerError> {
    use crate::codegen::flow::{Site, ValueFact};
    let expected = construction_facts(program, body, shared, sources, schemas)?;
    for (b, block) in shared.blocks.iter().enumerate() {
        for i in 0..=block.instructions.len() {
            let instruction = block.instructions.get(i);
            let site = Site {
                block: Block(b as u32),
                instruction: i,
            };
            assert_eq!(
                actual.reachable(site),
                expected.reachable(site),
                "reachability in {} at {site:?}",
                body.name
            );
            if matches!(
                instruction.map(|i| &i.instruction),
                Some(ir::Instruction::Portable(
                    ir::PortableInstruction::Drop { .. }
                ))
            ) {
                continue;
            }
            let operands = match instruction {
                Some(instruction) => instruction.operands(),
                None => match &block.terminator {
                    ir::Terminator::Unreachable => vec![],
                    ir::Terminator::Return(value) => vec![*value],
                    ir::Terminator::Branch { condition, .. } => vec![*condition],
                    // Edge arguments may include true-only pattern bindings before the branch.
                    ir::Terminator::Jump { .. } => vec![],
                },
            };
            for value in operands {
                if let ValueFact::Known(expected) = expected.at(site, value) {
                    match actual.at(site, value) {
                        ValueFact::Known(found) => assert_eq!(
                            found, expected,
                            "flow in {} at {site:?} for {value:?}: {:?}",
                            body.name, instruction
                        ),
                        found => panic!(
                            "flow in {} at {site:?} for {value:?}: {found:?}, expected {expected:?}",
                            body.name
                        ),
                    }
                }
            }
        }
    }
    Ok(())
}
