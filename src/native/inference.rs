//! Native representation inference over immutable SSA definitions and block arguments.
use crate::native::{
    FosterError, HashMap, NativeIrEnvironment, NativeType, SpecializationKey, VerifiedRemoteCall,
    ir, native_error, resolve_specialization,
};
use ir::PortableInstruction as Instruction;
mod instructions;
mod representations;
#[cfg(test)]
mod tests;
use instructions::{infer_instruction, representation_operand};
use representations::join_representation;
pub(super) use representations::{
    dereference_native_type, field_type, native_intrinsic_result_type, native_verification_type,
};

#[derive(Debug, PartialEq, Eq)]
pub(super) struct InferredRepresentations {
    pub(super) types: Vec<NativeType>,
    /// Definitions carrying only uninitialized or already-dropped storage tokens.
    pub(super) empty: std::collections::HashSet<ir::Value>,
}

pub(super) fn infer_value_types(
    function: &ir::Function,
    parameter_types: &[NativeType],
    instance: &SpecializationKey,
    environment: NativeIrEnvironment<'_>,
    remote_calls: &HashMap<ir::Value, VerifiedRemoteCall>,
) -> Result<InferredRepresentations, FosterError> {
    let mut result = vec![None; function.values.len()];
    let inputs = function
        .captures
        .iter()
        .map(|capture| capture.value)
        .chain(function.parameters.iter().copied())
        .collect::<Vec<_>>();
    if inputs.len() != parameter_types.len() {
        return Err(native_error("SSA inputs do not match the native signature"));
    }
    for (value, ty) in inputs.iter().zip(parameter_types) {
        result[value.index()] = Some(*ty);
    }
    // Edges describe representation joins independently of storage hints or block order.
    let mut edges = Vec::new();
    for (argument, parameter) in function
        .entry_arguments
        .iter()
        .zip(&function.blocks[function.entry.0 as usize].parameters)
    {
        edges.push((*argument, *parameter, true));
    }
    for block in &function.blocks {
        let successors = match &block.terminator {
            ir::Terminator::Jump { target, arguments } => vec![(*target, arguments)],
            ir::Terminator::Branch {
                then_target,
                then_arguments,
                else_target,
                else_arguments,
                ..
            } => vec![
                (*then_target, then_arguments),
                (*else_target, else_arguments),
            ],
            ir::Terminator::Unreachable | ir::Terminator::Return(_) => Vec::new(),
        };
        for (target, arguments) in successors {
            for (argument, parameter) in arguments
                .iter()
                .zip(&function.blocks[target.0 as usize].parameters)
            {
                edges.push((*argument, *parameter, false));
            }
        }
    }
    let mut empty = function
        .blocks
        .iter()
        .flat_map(|block| block.parameters.iter().copied())
        .collect::<std::collections::HashSet<_>>();
    empty.extend(function.entry_seeds.iter().copied());
    // Only tokens whose incoming paths all originate at empty seeds may acquire
    // a representation from a successor. Never constrain a real producer backwards.
    loop {
        let mut changed = false;
        for &(argument, parameter, _) in &edges {
            if !empty.contains(&argument) {
                changed |= empty.remove(&parameter);
            }
        }
        if !changed {
            break;
        }
    }
    // Unreachable blocks can contain calls on unavailable bindings. Such tokens
    // have no producer layout; the callee's ABI supplies their use-site constraint.
    for instruction in function.blocks.iter().flat_map(|block| &block.instructions) {
        let ir::Instruction::Portable(instruction) = &instruction.instruction else {
            continue;
        };
        let (callee, specialization, arguments) = match instruction {
            Instruction::Call {
                function,
                specialization,
                arguments,
                ..
            } => (function, specialization, arguments.clone()),
            Instruction::CallMethod {
                function,
                specialization,
                receiver,
                arguments,
                ..
            } => (
                function,
                specialization,
                std::iter::once(*receiver)
                    .chain(arguments.iter().copied())
                    .collect(),
            ),
            Instruction::CallClosure {
                function,
                specialization,
                captures,
                arguments,
                ..
            } => (
                function,
                specialization,
                captures
                    .iter()
                    .map(|(_, value)| *value)
                    .chain(arguments.iter().copied())
                    .collect(),
            ),
            _ => continue,
        };
        let target = environment.instances[&SpecializationKey {
            function: *callee,
            substitutions: resolve_specialization(specialization, &instance.substitutions),
        }];
        let expected = &environment.function_types[&target].parameters;
        if arguments.len() != expected.len() {
            return Err(native_error(
                "SSA call arguments do not match the native signature",
            ));
        }
        for (argument, ty) in arguments.iter().zip(expected) {
            if empty.contains(argument) {
                result[argument.index()] = Some(match result[argument.index()] {
                    Some(previous) => join_representation(previous, *ty, environment)?,
                    None => *ty,
                });
            }
        }
    }
    loop {
        let previous = result.clone();
        for &(argument, parameter, entry) in &edges {
            if empty.contains(&argument) {
                continue;
            }
            if let Some(mut ty) = result[argument.index()] {
                if entry && inputs.contains(&argument) {
                    // The native prologue loads reference inputs before the implicit entry edge.
                    ty = dereference_native_type(ty, environment)?;
                }
                result[parameter.index()] = Some(match result[parameter.index()] {
                    Some(current) => join_representation(current, ty, environment)?,
                    None => ty,
                });
            }
        }
        // An unavailable binding can travel through several empty block arguments.
        // Its first initialized successor supplies the representation of that token.
        for &(argument, parameter, _) in edges.iter().rev() {
            if result[argument.index()].is_none() && empty.contains(&argument) {
                result[argument.index()] = result[parameter.index()];
            }
        }
        for block in &function.blocks {
            for instruction in &block.instructions {
                match &instruction.instruction {
                    ir::Instruction::Portable(instruction) => {
                        if representation_operand(instruction)
                            .is_some_and(|value| result[value.index()].is_none())
                        {
                            continue;
                        }
                        infer_instruction(
                            instruction,
                            function,
                            instance,
                            environment,
                            remote_calls,
                            &mut result,
                        )?;
                    }
                    _ => {
                        return Err(native_error(
                            "native inference requires sealed portable SSA",
                        ));
                    }
                }
            }
        }
        if result == previous {
            break;
        }
    }
    let types = result
        .into_iter()
        .enumerate()
        .map(|(index, ty)| {
            ty.or_else(|| {
                empty
                    .contains(&function.values.value(index).unwrap())
                    .then_some(function.values[index])
            })
            .ok_or_else(|| {
                native_error(format!(
                    "cannot determine representation of SSA value %{index} in `{}`",
                    function.name
                ))
            })
        })
        .collect::<Result<_, _>>()?;
    Ok(InferredRepresentations { types, empty })
}
