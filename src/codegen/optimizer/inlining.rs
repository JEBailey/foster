//! Bounded leaf inlining, independent of VM register assignment and native ABI.
use super::*;

fn scalar(ty: &ir::Type) -> bool {
    matches!(
        ty,
        ir::Type::Unit
            | ir::Type::Bool
            | ir::Type::Int
            | ir::Type::Float
            | ir::Type::CodePoint
            | ir::Type::Byte
    )
}

fn acyclic(body: &ir::Function) -> bool {
    fn visit(body: &ir::Function, block: ir::Block, states: &mut [u8]) -> bool {
        if states[block.0 as usize] != 0 {
            return states[block.0 as usize] == 2;
        }
        states[block.0 as usize] = 1;
        for (target, _) in graph::edges(&body.blocks[block.0 as usize].terminator) {
            if !visit(body, target, states) {
                return false;
            }
        }
        states[block.0 as usize] = 2;
        true
    }
    let mut states = vec![0; body.blocks.len()];
    (0..body.blocks.len()).all(|index| visit(body, ir::Block(index as u32), &mut states))
}

fn eligible(body: &ir::Function, declaration: &super::super::program::FunctionDeclaration) -> bool {
    body.captures.is_empty()
        && !declaration.returns_reference
        && !declaration.mutable_parameters.iter().any(|value| *value)
        && body.values.iter().all(scalar)
        && body
            .blocks
            .iter()
            .map(|block| block.instructions.len() + 1)
            .sum::<usize>()
            <= 32
        && acyclic(body)
        && body.blocks.iter().all(|block| {
            block.instructions.iter().all(|entry| {
                matches!(
                    entry.instruction,
                    I::Portable(
                        P::LoadConstant { .. }
                            | P::Move { .. }
                            | P::Unary { .. }
                            | P::Binary { .. }
                            | P::Drop { .. }
                            | P::Assert { .. }
                    )
                )
            })
        })
}

pub(super) fn run(
    program: &mut super::super::program::Program,
    writes: &mut HashMap<FunctionId, HashMap<Value, Value>>,
) -> HashSet<FunctionId> {
    let mut changed = HashSet::new();
    for _ in 0..4 {
        let candidates = program
            .bodies
            .iter()
            .filter(|(id, body)| eligible(body, &program.functions[id]))
            .map(|(id, body)| (*id, body.clone()))
            .collect::<HashMap<_, _>>();
        let mut ids = program.bodies.keys().copied().collect::<Vec<_>>();
        ids.sort();
        let mut progress = false;
        for id in ids {
            let caller = program.bodies.get_mut(&id).unwrap();
            if !caller.values.iter().all(scalar)
                || program.functions[&id]
                    .mutable_parameters
                    .iter()
                    .any(|value| *value)
                || program.functions[&id].returns_reference
            {
                continue;
            }
            let mut size = caller
                .blocks
                .iter()
                .map(|block| block.instructions.len() + 1)
                .sum::<usize>();
            let mut block_index = 0;
            while block_index < caller.blocks.len() && size < 128 {
                let call = caller.blocks[block_index]
                    .instructions
                    .iter()
                    .enumerate()
                    .find_map(|(index, entry)| {
                        let I::Portable(P::Call {
                            destination,
                            function,
                            specialization,
                            arguments,
                        }) = &entry.instruction
                        else {
                            return None;
                        };
                        let callee = candidates.get(function)?;
                        if *function == id
                            || !specialization.is_empty()
                            || arguments.len() != callee.parameters.len()
                            || arguments
                                .iter()
                                .zip(&callee.signature.parameters)
                                .any(|(value, ty)| caller.values[value.index()] != *ty)
                            || caller.values[destination.index()] != callee.signature.result
                        {
                            return None;
                        }
                        Some((
                            index,
                            *destination,
                            *function,
                            arguments.clone(),
                            entry.span.clone(),
                        ))
                    });
                let Some((index, destination, target, arguments, span)) = call else {
                    block_index += 1;
                    continue;
                };
                let callee = &candidates[&target];
                let growth = callee
                    .blocks
                    .iter()
                    .map(|block| block.instructions.len() + 1)
                    .sum::<usize>()
                    + arguments.len();
                let next_home = caller
                    .values
                    .hints()
                    .flatten()
                    .copied()
                    .max()
                    .map_or(0u32, |home| u32::from(home) + 1);
                let home_count = callee
                    .values
                    .hints()
                    .flatten()
                    .copied()
                    .max()
                    .map_or(0u32, |home| u32::from(home) + 1);
                if size + growth > 128 || next_home + home_count > u32::from(u16::MAX) {
                    block_index += 1;
                    continue;
                }
                size += growth;
                let mut values = std::mem::take(&mut caller.values).into_builder();
                let map = (0..callee.values.len())
                    .map(|i| {
                        (
                            Value(i as u32),
                            values.allocate(
                                callee.values[i],
                                callee
                                    .values
                                    .hint(i)
                                    .map(|home| (next_home + u32::from(home)) as u16),
                            ),
                        )
                    })
                    .collect::<HashMap<_, _>>();
                caller.values = values.finish();
                caller
                    .entry_seeds
                    .extend(callee.entry_seeds.iter().map(|value| map[value]));
                let rewrite = |value: Value| map[&value];
                let base = caller.blocks.len() as u32;
                let continuation = ir::Block(base + callee.blocks.len() as u32);
                let tail = caller.blocks[block_index].instructions.split_off(index + 1);
                caller.blocks[block_index].instructions.pop();
                for (parameter, source) in callee.parameters.iter().zip(arguments) {
                    caller.blocks[block_index].instructions.push(
                        I::Portable(P::Move {
                            destination: rewrite(*parameter),
                            source,
                        })
                        .with_span(span.clone()),
                    );
                }
                let old_terminator = std::mem::replace(
                    &mut caller.blocks[block_index].terminator,
                    ir::Terminator::Jump {
                        target: ir::Block(base + callee.entry.0),
                        arguments: callee
                            .entry_arguments
                            .iter()
                            .copied()
                            .map(rewrite)
                            .collect(),
                    },
                );
                let old_span = std::mem::replace(
                    &mut caller.blocks[block_index].terminator_span,
                    span.clone(),
                );
                for mut block in callee.blocks.clone() {
                    for parameter in &mut block.parameters {
                        *parameter = rewrite(*parameter);
                    }
                    for entry in &mut block.instructions {
                        entry.instruction.rewrite_values(rewrite);
                        entry.span = span.clone();
                    }
                    block.terminator.rewrite_values(rewrite);
                    match &mut block.terminator {
                        ir::Terminator::Jump { target, .. } => target.0 += base,
                        ir::Terminator::Branch {
                            then_target,
                            else_target,
                            ..
                        } => {
                            then_target.0 += base;
                            else_target.0 += base;
                        }
                        ir::Terminator::Return(value) => {
                            block.terminator = ir::Terminator::Jump {
                                target: continuation,
                                arguments: vec![*value],
                            }
                        }
                    }
                    block.terminator_span = span.clone();
                    caller.blocks.push(block);
                }
                caller.blocks.push(ir::BlockData {
                    parameters: vec![destination],
                    instructions: tail,
                    terminator: old_terminator,
                    terminator_span: old_span,
                });
                let additions = writes[&target]
                    .iter()
                    .map(|(left, right)| (rewrite(*left), rewrite(*right)))
                    .collect::<Vec<_>>();
                writes.get_mut(&id).unwrap().extend(additions);
                changed.insert(id);
                progress = true;
                block_index += 1;
            }
        }
        if !progress {
            break;
        }
    }
    changed
}
