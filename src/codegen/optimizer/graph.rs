//! CFG simplification and value liveness over canonical SSA.
use super::*;

pub(super) fn edges(terminator: &ir::Terminator) -> Vec<(ir::Block, &[Value])> {
    match terminator {
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
        ir::Terminator::Return(_) => vec![],
    }
}

pub(super) fn simplify(
    function: &mut ir::Function,
    constants: &mut Vec<Constant>,
    writes: &mut HashMap<Value, Value>,
    pool: &mut HashMap<Scalar, u16>,
) -> bool {
    let exposed = exposed_homes(function);
    let safe = |value: Value| {
        !function
            .values
            .hint(value.index())
            .is_some_and(|home| exposed.contains(&home))
    };
    // Only immutable scalar identities enter this lattice. Unknown loop inputs
    // remain unknown; facts are never guessed from a single predecessor.
    let mut predecessors = vec![Vec::new(); function.blocks.len()];
    for block in &function.blocks {
        for (target, arguments) in edges(&block.terminator) {
            predecessors[target.0 as usize].push(arguments);
        }
    }
    predecessors[function.entry.0 as usize].push(&function.entry_arguments);
    let mut known = HashMap::new();
    loop {
        let before = known.len();
        for (index, block) in function.blocks.iter().enumerate() {
            let incoming = &predecessors[index];
            for (slot, parameter) in block.parameters.iter().enumerate() {
                if !safe(*parameter) || incoming.is_empty() {
                    continue;
                }
                let Some(value) = incoming[0]
                    .get(slot)
                    .and_then(|arg| known.get(arg))
                    .copied()
                else {
                    continue;
                };
                if incoming
                    .iter()
                    .all(|args| args.get(slot).and_then(|arg| known.get(arg)) == Some(&value))
                {
                    known.insert(*parameter, value);
                }
            }
            for entry in &block.instructions {
                if let Some((destination, value)) = evaluate(&entry.instruction, &known, constants)
                    && safe(destination)
                    && function.values[destination.index()] == value.ty()
                {
                    known.insert(destination, value);
                }
            }
        }
        if before == known.len() {
            break;
        }
    }
    let mut changed = false;
    for block in &mut function.blocks {
        for entry in &mut block.instructions {
            if let I::Portable(P::Unary { destination, .. } | P::Binary { destination, .. }) =
                &entry.instruction
                && let Some(value) = known.get(destination)
            {
                let index = pool.get(value).copied().or_else(|| {
                    let index = u16::try_from(constants.len()).ok()?;
                    constants.push(value.pool());
                    pool.insert(*value, index);
                    Some(index)
                });
                if let Some(constant) = index {
                    entry.instruction = I::Portable(P::LoadConstant {
                        destination: *destination,
                        constant,
                    });
                    changed = true;
                }
            }
        }
        if let ir::Terminator::Branch {
            condition,
            then_target,
            then_arguments,
            else_target,
            else_arguments,
        } = &block.terminator
            && let Some(Scalar::Bool(value)) = known.get(condition)
        {
            block.terminator = if *value {
                ir::Terminator::Jump {
                    target: *then_target,
                    arguments: then_arguments.clone(),
                }
            } else {
                ir::Terminator::Jump {
                    target: *else_target,
                    arguments: else_arguments.clone(),
                }
            };
            changed = true;
        }
    }
    let mut reachable = HashSet::new();
    let mut pending = vec![function.entry];
    while let Some(block) = pending.pop() {
        if reachable.insert(block) {
            pending.extend(
                edges(&function.blocks[block.0 as usize].terminator)
                    .into_iter()
                    .map(|(target, _)| target),
            );
        }
    }
    if reachable.len() != function.blocks.len() {
        let map = (0..function.blocks.len())
            .map(|i| ir::Block(i as u32))
            .filter(|block| reachable.contains(block))
            .enumerate()
            .map(|(index, block)| (block, ir::Block(index as u32)))
            .collect::<HashMap<_, _>>();
        function.blocks = std::mem::take(&mut function.blocks)
            .into_iter()
            .enumerate()
            .filter(|(index, _)| reachable.contains(&ir::Block(*index as u32)))
            .map(|(_, mut block)| {
                match &mut block.terminator {
                    ir::Terminator::Jump { target, .. } => *target = map[target],
                    ir::Terminator::Branch {
                        then_target,
                        else_target,
                        ..
                    } => {
                        *then_target = map[then_target];
                        *else_target = map[else_target];
                    }
                    _ => {}
                }
                block
            })
            .collect();
        function.entry = map[&function.entry];
        changed = true;
    }
    // Releasing an unexposed scalar has no destructor or ownership effect.
    // Remove these uses first so dead scalar definitions can become dead too.
    for block in &mut function.blocks {
        block.instructions.retain(|entry| {
            let removable = matches!(entry.instruction, I::Portable(P::Drop { value })
                if matches!(function.values[value.index()], ir::Type::Unit | ir::Type::Bool | ir::Type::Int | ir::Type::Float | ir::Type::Byte | ir::Type::CodePoint)
                    && !function.values.hint(value.index()).is_some_and(|home| exposed.contains(&home)));
            changed |= removable;
            !removable
        });
    }
    loop {
        let mut used = HashSet::new();
        used.extend(&function.entry_arguments);
        used.extend(writes.values());
        for block in &function.blocks {
            for entry in &block.instructions {
                used.extend(entry.operands());
            }
            let mut terminator = block.terminator.clone();
            terminator.rewrite_values(|value| {
                used.insert(value);
                value
            });
        }
        let mut removed = false;
        for block in &mut function.blocks {
            block.instructions.retain(|entry| {
                // Preserve potentially trapping arithmetic, effects and all
                // writes whose storage identity may escape.
                let removable = matches!(
                    entry.instruction,
                    I::Portable(P::LoadConstant { .. } | P::Move { .. })
                ) && entry.destinations().iter().all(|value| {
                    !used.contains(value)
                        && matches!(
                            function.values[value.index()],
                            ir::Type::Unit
                                | ir::Type::Bool
                                | ir::Type::Int
                                | ir::Type::Float
                                | ir::Type::Byte
                                | ir::Type::CodePoint
                        )
                        && !function
                            .values
                            .hint(value.index())
                            .is_some_and(|home| exposed.contains(&home))
                });
                removed |= removable;
                !removable
            });
        }
        changed |= removed;
        if !removed {
            break;
        }
    }
    if changed {
        compact_values(function, writes);
    }
    changed
}

pub(super) fn compact_values(function: &mut ir::Function, writes: &mut HashMap<Value, Value>) {
    let mut defined = HashSet::new();
    defined.extend(&function.parameters);
    defined.extend(&function.entry_seeds);
    defined.extend(function.captures.iter().map(|capture| capture.value));
    for block in &function.blocks {
        defined.extend(&block.parameters);
        for entry in &block.instructions {
            defined.extend(entry.destinations());
        }
    }
    let mut values = ir::ValueBuilder::default();
    let mut map = HashMap::new();
    for index in 0..function.values.len() {
        let old = Value(index as u32);
        if defined.contains(&old) {
            map.insert(
                old,
                values.allocate(function.values[index], function.values.hint(index)),
            );
        }
    }
    let rewrite = |value: Value| map[&value];
    for value in function
        .parameters
        .iter_mut()
        .chain(&mut function.entry_seeds)
        .chain(&mut function.entry_arguments)
    {
        *value = rewrite(*value);
    }
    for capture in &mut function.captures {
        capture.value = rewrite(capture.value);
    }
    for block in &mut function.blocks {
        for parameter in &mut block.parameters {
            *parameter = rewrite(*parameter);
        }
        for entry in &mut block.instructions {
            entry.instruction.rewrite_values(rewrite);
        }
        block.terminator.rewrite_values(rewrite);
    }
    *writes = writes
        .iter()
        .filter_map(|(destination, source)| Some((*map.get(destination)?, *map.get(source)?)))
        .collect();
    function.values = values.finish();
}
