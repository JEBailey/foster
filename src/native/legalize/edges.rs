//! Representation changes on SSA edges, before parallel block-argument assignment.
use super::conversions::ReturnConversion;
use crate::native::{
    FailureCleanup, FosterError, NativeIrEnvironment, NativeType, ir, native_error,
};

/// Edge conversions retain their inputs. If a later release invokes a failing
/// destructor, clean up both the newly converted owners and the surviving inputs.
pub(super) fn record_cleanup(
    instructions: &[ir::Instruction],
    live: impl IntoIterator<Item = ir::Value>,
    block: usize,
    offset: usize,
    values: &ir::ValueTable,
    cleanup: &mut FailureCleanup,
) {
    let mut live = live.into_iter().collect::<std::collections::BTreeSet<_>>();
    for (index, instruction) in instructions.iter().enumerate() {
        if let ir::Instruction::Portable(ir::PortableInstruction::Drop { value }) = instruction {
            live.remove(value);
        }
        cleanup.values.insert(
            (block, offset + index),
            live.iter()
                .rev()
                .copied()
                .filter(|value| {
                    matches!(
                        values[value.index()],
                        NativeType::String | NativeType::Object(_)
                    )
                })
                .collect(),
        );
        live.extend(instruction.destinations());
    }
}

pub(super) fn adapt_arguments(
    arguments: &mut [ir::Value],
    parameters: &[ir::Value],
    values: &mut ir::ValueBuilder,
    seeds: &mut Vec<ir::Value>,
    empty: &std::collections::HashSet<ir::Value>,
    environment: NativeIrEnvironment<'_>,
) -> Result<Vec<ir::Instruction>, FosterError> {
    if arguments.len() != parameters.len() {
        return Err(native_error(
            "native SSA edge has inconsistent argument count",
        ));
    }
    let mut instructions = Vec::new();
    for (argument, parameter) in arguments.iter_mut().zip(parameters) {
        let source = values[argument.index()];
        let target = values[parameter.index()];
        if source == target {
            continue;
        }
        if seeds.contains(argument) || empty.contains(argument) {
            // An empty ownership token carries no runtime value to convert.
            *argument = values.allocate(target, None);
            seeds.push(*argument);
            continue;
        }
        let conversion =
            ReturnConversion::between(source, target, environment)?.ok_or_else(|| {
                native_error(format!(
                    "cannot adapt SSA edge from {source:?} to {target:?}"
                ))
            })?;
        let converted = values.allocate(target, None);
        instructions.push(conversion.instruction(converted, *argument));
        *argument = converted;
    }
    Ok(instructions)
}
