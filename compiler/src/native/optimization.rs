//! Native-only cleanup. Keep ownership instructions and addressable storage identities intact.
use super::{FailureCleanup, HashMap, HashSet, NativeType, abi, ir};

pub(super) fn addressable_homes(function: &ir::Function) -> HashSet<u16> {
    function
        .blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .filter_map(|entry| match &entry.instruction {
            ir::Instruction::Portable(ir::PortableInstruction::MakeWholeReference {
                object,
                ..
            }) => function.values.hint(object.index()),
            _ => None,
        })
        .collect()
}

fn scalar(ty: NativeType) -> bool {
    matches!(
        ty,
        NativeType::Unit
            | NativeType::Bool
            | NativeType::Int
            | NativeType::Float
            | NativeType::CodePoint
            | NativeType::Byte
    )
}

fn edges(term: &ir::Terminator) -> Vec<(ir::Block, &[ir::Value])> {
    match term {
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
        _ => Vec::new(),
    }
}

fn mutate_edges(
    term: &mut ir::Terminator,
    mut apply: impl FnMut(&mut ir::Block, &mut Vec<ir::Value>),
) {
    match term {
        ir::Terminator::Jump { target, arguments } => apply(target, arguments),
        ir::Terminator::Branch {
            then_target,
            then_arguments,
            else_target,
            else_arguments,
            ..
        } => {
            apply(then_target, then_arguments);
            apply(else_target, else_arguments);
        }
        _ => {}
    }
}

fn terminator_operands(term: &ir::Terminator) -> Vec<ir::Value> {
    let mut values = edges(term)
        .into_iter()
        .flat_map(|(_, args)| args.iter().copied())
        .collect::<Vec<_>>();
    match term {
        ir::Terminator::Branch { condition, .. } => values.push(*condition),
        ir::Terminator::Return(value) => values.push(*value),
        _ => {}
    }
    values
}

pub(super) fn run(function: &mut ir::Function, cleanup: &mut FailureCleanup) {
    let addressable = addressable_homes(function);
    let values = &function.values;
    let safe = |value: ir::Value| {
        scalar(values[value.index()])
            && !values
                .hint(value.index())
                .is_some_and(|h| addressable.contains(&h))
    };
    let mut copies = HashMap::new();
    for block in &function.blocks {
        for entry in &block.instructions {
            if let ir::Instruction::Portable(ir::PortableInstruction::Move {
                destination,
                source,
            }) = entry.instruction
                && safe(destination)
                && safe(source)
                && function.value_type(destination) == function.value_type(source)
            {
                copies.insert(destination, source);
            }
        }
    }
    let resolve = |mut value: ir::Value| {
        while let Some(source) = copies.get(&value) {
            value = *source;
        }
        value
    };
    let mut sites = HashMap::new();
    for (block_index, block) in function.blocks.iter_mut().enumerate() {
        let mut kept = Vec::new();
        for (index, mut entry) in std::mem::take(&mut block.instructions)
            .into_iter()
            .enumerate()
        {
            let discard = match &entry.instruction {
                ir::Instruction::Portable(ir::PortableInstruction::Move {
                    destination, ..
                }) => copies.contains_key(destination),
                ir::Instruction::Portable(ir::PortableInstruction::Drop { value }) => safe(*value),
                _ => false,
            };
            if discard {
                // Keep value IDs stable for native metadata and verification;
                // unused definitions become zero-valued entry seeds.
                function.entry_seeds.extend(entry.destinations());
                continue;
            }
            if let Some(values) = cleanup.values.get(&(block_index, index)) {
                sites.insert(
                    (block_index, kept.len()),
                    values
                        .iter()
                        .copied()
                        .filter(|value| !safe(*value))
                        .map(resolve)
                        .collect(),
                );
            }
            entry.instruction.rewrite_values(resolve);
            kept.push(entry);
        }
        block.instructions = kept;
        block.terminator.rewrite_values(resolve);
    }
    function
        .entry_arguments
        .iter_mut()
        .for_each(|v| *v = resolve(*v));
    cleanup.values = sites;

    // A parameter referenced directly from another block cannot be substituted
    // independently on each incoming edge. Keep that defining block intact.
    let parameter_blocks = function
        .blocks
        .iter()
        .enumerate()
        .flat_map(|(index, block)| {
            block
                .parameters
                .iter()
                .copied()
                .map(move |value| (value, index))
        })
        .collect::<HashMap<_, _>>();
    let mut external_parameters = HashSet::new();
    for (index, block) in function.blocks.iter().enumerate() {
        for value in block
            .instructions
            .iter()
            .flat_map(|entry| entry.operands())
            .chain(terminator_operands(&block.terminator))
        {
            if parameter_blocks
                .get(&value)
                .is_some_and(|owner| *owner != index)
            {
                external_parameters.insert(value);
            }
        }
    }
    for ((index, _), values) in &cleanup.values {
        for value in values {
            if parameter_blocks
                .get(value)
                .is_some_and(|owner| owner != index)
            {
                external_parameters.insert(*value);
            }
        }
    }

    // Thread only poll-only trampolines. Cycles keep their polls; addressable
    // parameter rebinding cannot be bypassed because aliases can observe it.
    let threaded = |start: ir::Block, incoming: &[ir::Value]| {
        let mut target = start;
        let mut arguments = incoming.to_vec();
        let mut seen = HashSet::new();
        loop {
            if !seen.insert(target) {
                return (start, incoming.to_vec());
            }
            let block = &function.blocks[target.0 as usize];
            if target == function.entry
                || block.parameters.iter().any(|p| {
                    external_parameters.contains(p)
                        || function
                            .values
                            .hint(p.index())
                            .is_some_and(|h| addressable.contains(&h))
                })
            {
                break;
            }
            if !block.instructions.iter().all(|entry| matches!(&entry.instruction, ir::Instruction::RuntimeCall { helper, .. } if *helper == abi::CANCELLATION_POINT)) { break; }
            let ir::Terminator::Jump {
                target: next,
                arguments: outgoing,
            } = &block.terminator
            else {
                break;
            };
            let bindings = block
                .parameters
                .iter()
                .copied()
                .zip(arguments)
                .collect::<HashMap<_, _>>();
            arguments = outgoing
                .iter()
                .map(|v| bindings.get(v).copied().unwrap_or(*v))
                .collect();
            target = *next;
        }
        (target, arguments)
    };
    let mut terms = function
        .blocks
        .iter()
        .map(|b| b.terminator.clone())
        .collect::<Vec<_>>();
    for term in &mut terms {
        mutate_edges(term, |target, args| {
            let (next, values) = threaded(*target, args);
            *target = next;
            *args = values;
        });
    }
    for (block, term) in function.blocks.iter_mut().zip(terms) {
        block.terminator = term;
    }
    let mut reachable = HashSet::new();
    let mut pending = vec![function.entry];
    while let Some(block) = pending.pop() {
        if reachable.insert(block) {
            pending.extend(
                edges(&function.blocks[block.0 as usize].terminator)
                    .into_iter()
                    .map(|(b, _)| b),
            );
        }
    }
    let mut map = HashMap::new();
    for old in 0..function.blocks.len() {
        if reachable.contains(&ir::Block(old as u32)) {
            map.insert(ir::Block(old as u32), ir::Block(map.len() as u32));
        }
    }
    function.blocks = std::mem::take(&mut function.blocks)
        .into_iter()
        .enumerate()
        .filter_map(|(old, mut block)| {
            if !map.contains_key(&ir::Block(old as u32)) {
                function.entry_seeds.extend(block.parameters);
                function
                    .entry_seeds
                    .extend(block.instructions.iter().flat_map(|i| i.destinations()));
                return None;
            }
            mutate_edges(&mut block.terminator, |target, _| *target = map[target]);
            Some(block)
        })
        .collect();
    function.entry = map[&function.entry];
    cleanup.values = std::mem::take(&mut cleanup.values)
        .into_iter()
        .filter_map(|((block, site), values)| {
            map.get(&ir::Block(block as u32))
                .map(|b| ((b.0 as usize, site), values))
        })
        .collect();

    // Seed liveness with actual uses, rather than phi-to-phi cycles. Managed
    // bindings and address-taken scalar homes remain explicit ownership state.
    let mut live = function
        .blocks
        .iter()
        .flat_map(|b| b.instructions.iter().flat_map(|i| i.operands()))
        .collect::<HashSet<_>>();
    live.extend(cleanup.values.values().flatten().copied());
    for block in &function.blocks {
        match block.terminator {
            ir::Terminator::Branch { condition, .. } => {
                live.insert(condition);
            }
            ir::Terminator::Return(value) => {
                live.insert(value);
            }
            _ => {}
        }
        live.extend(
            block
                .parameters
                .iter()
                .filter(|p| {
                    !scalar(function.value_type(**p))
                        || function
                            .values
                            .hint(p.index())
                            .is_some_and(|h| addressable.contains(&h))
                })
                .copied(),
        );
    }
    loop {
        let before = live.len();
        for (target, args) in function
            .blocks
            .iter()
            .flat_map(|b| edges(&b.terminator))
            .chain(std::iter::once((
                function.entry,
                function.entry_arguments.as_slice(),
            )))
        {
            for (parameter, argument) in function.blocks[target.0 as usize]
                .parameters
                .iter()
                .zip(args)
            {
                if live.contains(parameter) {
                    live.insert(*argument);
                }
            }
        }
        if live.len() == before {
            break;
        }
    }
    let keep = function
        .blocks
        .iter()
        .map(|b| {
            b.parameters
                .iter()
                .map(|v| live.contains(v))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    for block in &mut function.blocks {
        mutate_edges(&mut block.terminator, |target, args| {
            let mut index = 0;
            args.retain(|_| {
                let retained = keep[target.0 as usize][index];
                index += 1;
                retained
            });
        });
    }
    let mut index = 0;
    function.entry_arguments.retain(|_| {
        let retained = keep[function.entry.0 as usize][index];
        index += 1;
        retained
    });
    for (block, keep) in function.blocks.iter_mut().zip(&keep) {
        let old = std::mem::take(&mut block.parameters);
        for (parameter, retained) in old.into_iter().zip(keep) {
            if *retained {
                block.parameters.push(parameter);
            } else {
                function.entry_seeds.push(parameter);
            }
        }
    }
    let mut seeds = HashSet::new();
    function.entry_seeds.retain(|v| seeds.insert(*v));
}
