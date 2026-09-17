//! Dominating common expressions over immutable, non-trapping scalar values.
use super::*;

#[derive(Clone, PartialEq, Eq, Hash)]
enum Key {
    Constant(Scalar),
    Unary(u8, Value),
    Binary(u8, Value, Value),
}

fn resolve(aliases: &HashMap<Value, Value>, mut value: Value) -> Value {
    while let Some(next) = aliases.get(&value) {
        value = *next;
    }
    value
}

fn candidate(
    instruction: &I,
    values: &ir::ValueTable,
    constants: &[Constant],
) -> Option<(Value, Key)> {
    let ty = |value: Value| values[value.index()];
    let scalar = |t| {
        matches!(
            t,
            ir::Type::Unit | ir::Type::Bool | ir::Type::Int | ir::Type::Byte | ir::Type::CodePoint
        )
    };
    match instruction {
        I::Portable(P::LoadConstant {
            destination,
            constant,
        }) => {
            let value = Scalar::from_pool(constants.get(*constant as usize)?)?;
            (scalar(ty(*destination)) && ty(*destination) == value.ty())
                .then_some((*destination, Key::Constant(value)))
        }
        I::Unary {
            destination,
            operator,
            operand,
        }
        | I::Portable(P::Unary {
            destination,
            operator,
            operand,
        }) => {
            let valid = matches!(
                (operator, ty(*operand), ty(*destination)),
                (UnaryOp::Not, ir::Type::Bool, ir::Type::Bool)
                    | (UnaryOp::BitNot, ir::Type::Byte, ir::Type::Byte)
            );
            valid.then_some((*destination, Key::Unary(*operator as u8, *operand)))
        }
        I::Binary {
            destination,
            operator,
            left,
            right,
        }
        | I::Portable(P::Binary {
            destination,
            operator,
            left,
            right,
        }) => {
            let input = ty(*left);
            let valid = input == ty(*right)
                && match operator {
                    BinaryOp::Equal | BinaryOp::NotEqual => {
                        scalar(input) && ty(*destination) == ir::Type::Bool
                    }
                    BinaryOp::Less
                    | BinaryOp::LessEqual
                    | BinaryOp::Greater
                    | BinaryOp::GreaterEqual => {
                        matches!(input, ir::Type::Int | ir::Type::Byte | ir::Type::CodePoint)
                            && ty(*destination) == ir::Type::Bool
                    }
                    BinaryOp::BitAnd | BinaryOp::BitOr | BinaryOp::BitXor => {
                        input == ir::Type::Byte && ty(*destination) == ir::Type::Byte
                    }
                    // Checked arithmetic, shifts, floating point and overloaded operations
                    // require additional failure/representation proofs before reuse.
                    _ => false,
                };
            valid.then_some((*destination, Key::Binary(*operator as u8, *left, *right)))
        }
        _ => None,
    }
}

pub(super) fn run(
    function: &mut ir::Function,
    constants: &[Constant],
    writes: &mut HashMap<Value, Value>,
) -> bool {
    // Even a scalar may cross a consuming ABI or be moved into an owning
    // aggregate. Do not extend its lifetime across those storage operations.
    let ownership_operands = function
        .blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .filter(|entry| {
            !scalar_instruction(&entry.instruction)
                && !matches!(
                    entry.instruction,
                    I::Portable(P::Drop { .. } | P::Assert { .. })
                )
        })
        .flat_map(|entry| entry.operands())
        .collect::<HashSet<_>>();
    let exposed = exposed_homes_with(
        function,
        ownership_operands
            .iter()
            .filter_map(|v| function.values.hint(v.index())),
    );
    let safe = |value: Value| {
        !ownership_operands.contains(&value)
            && !function
                .values
                .hint(value.index())
                .is_some_and(|home| exposed.contains(&home))
    };
    if !function
        .blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .any(|entry| {
            candidate(&entry.instruction, &function.values, constants)
                .is_some_and(|(value, _)| safe(value))
        })
    {
        return false;
    }

    let dom = dominators(function);
    let mut order = (0..function.blocks.len())
        .filter(|b| !dom[*b].is_empty())
        .collect::<Vec<_>>();
    order.sort_by_key(|b| (dom[*b].len(), *b));
    let mut available: HashMap<Key, Vec<(usize, Value)>> = HashMap::new();
    let mut aliases = HashMap::new();
    let mut retained = HashSet::new();
    for block in order {
        for entry in &function.blocks[block].instructions {
            let Some((destination, mut key)) =
                candidate(&entry.instruction, &function.values, constants)
            else {
                continue;
            };
            if !safe(destination) || entry.operands().iter().any(|v| !safe(*v)) {
                continue;
            }
            match &mut key {
                Key::Unary(_, operand) => *operand = resolve(&aliases, *operand),
                Key::Binary(_, left, right) => {
                    *left = resolve(&aliases, *left);
                    *right = resolve(&aliases, *right);
                }
                Key::Constant(_) => {}
            }
            let previous = available
                .get(&key)
                .and_then(|items| items.iter().find(|(site, _)| dom[block].contains(site)));
            if let Some((_, value)) = previous {
                aliases.insert(destination, *value);
                retained.insert(*value);
            } else {
                available.entry(key).or_default().push((block, destination));
            }
        }
    }
    if aliases.is_empty() {
        return false;
    }
    for block in &mut function.blocks {
        block.instructions.retain(|entry| {
            if let I::Portable(P::Drop { value }) = entry.instruction {
                // A reused scalar must stay available until its final use. Its
                // release is unobservable and backend liveness handles the slot.
                if aliases.contains_key(&value) || retained.contains(&value) {
                    return false;
                }
            }
            !entry.destinations().iter().any(|v| aliases.contains_key(v))
        });
        for entry in &mut block.instructions {
            entry.instruction.rewrite_values(|v| resolve(&aliases, v));
        }
        block.terminator.rewrite_values(|v| resolve(&aliases, v));
    }
    writes.retain(|destination, _| !aliases.contains_key(destination));
    for source in writes.values_mut() {
        *source = resolve(&aliases, *source);
    }
    // VM lowering honors storage hints. Reused results need distinct storage
    // from subsequent writes to their original, unexposed construction home.
    let mut values = std::mem::take(&mut function.values).into_builder();
    for value in retained {
        values.set_storage_hint(value, None);
    }
    function.values = values.finish();
    graph::compact_values(function, writes);
    true
}

fn dominators(function: &ir::Function) -> Vec<HashSet<usize>> {
    let entry = function.entry.0 as usize;
    let mut reachable = HashSet::new();
    let mut pending = vec![entry];
    let mut predecessors = vec![Vec::new(); function.blocks.len()];
    while let Some(block) = pending.pop() {
        if !reachable.insert(block) {
            continue;
        }
        for (target, _) in graph::edges(&function.blocks[block].terminator) {
            let target = target.0 as usize;
            predecessors[target].push(block);
            pending.push(target);
        }
    }
    let mut dom = vec![HashSet::new(); function.blocks.len()];
    for &block in &reachable {
        dom[block] = if block == entry {
            HashSet::from([entry])
        } else {
            reachable.clone()
        };
    }
    loop {
        let mut changed = false;
        for block in 0..function.blocks.len() {
            if block == entry || !reachable.contains(&block) {
                continue;
            }
            let mut next = reachable.clone();
            for &predecessor in &predecessors[block] {
                next.retain(|b| dom[predecessor].contains(b));
            }
            next.insert(block);
            if next != dom[block] {
                dom[block] = next;
                changed = true;
            }
        }
        if !changed {
            return dom;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ir::{Block, BlockData, Signature, SpannedInstruction, Terminator, Type, ValueBuilder};

    fn block(instructions: Vec<I>, terminator: Terminator) -> BlockData {
        let spans = (0..instructions.len()).map(|i| i..i + 1).collect();
        BlockData {
            parameters: vec![],
            instructions: SpannedInstruction::from_parts(instructions, spans),
            terminator,
            terminator_span: 90..91,
        }
    }
    fn comparison(destination: Value, left: Value, right: Value) -> I {
        I::Portable(P::Binary {
            destination,
            operator: BinaryOp::Less,
            left,
            right,
        })
    }
    fn fixture() -> ir::Function {
        let mut values = ValueBuilder::default();
        let x = values.allocate(Type::Int, None);
        let y = values.allocate(Type::Int, None);
        let first = values.allocate(Type::Bool, None);
        let second = values.allocate(Type::Bool, None);
        ir::Function {
            name: "cse".into(),
            signature: Signature {
                parameters: vec![Type::Int, Type::Int],
                result: Type::Bool,
            },
            parameters: vec![x, y],
            captures: vec![],
            entry_seeds: vec![],
            entry: Block(0),
            entry_arguments: vec![],
            values: values.finish(),
            blocks: vec![block(
                vec![comparison(first, x, y), comparison(second, x, y)],
                Terminator::Return(second),
            )],
        }
    }
    fn verify(function: &ir::Function) {
        function.verify(&HashMap::new()).unwrap();
    }
    #[test]
    fn reuses_dominating_comparisons_and_preserves_first_span() {
        let mut f = fixture();
        verify(&f);
        let mut writes = HashMap::from([(Value(3), Value(2))]);
        assert!(run(&mut f, &[], &mut writes));
        verify(&f);
        assert_eq!(f.blocks[0].instructions.len(), 1);
        assert_eq!(f.blocks[0].instructions[0].span, 0..1);
        assert!(writes.is_empty());
        assert!(!run(&mut f, &[], &mut writes));
    }
    #[test]
    fn entry_expression_dominates_loop_but_sibling_expression_does_not() {
        let mut f = fixture();
        f.blocks = vec![
            block(
                vec![comparison(Value(2), Value(0), Value(1))],
                Terminator::Jump {
                    target: Block(1),
                    arguments: vec![],
                },
            ),
            block(
                vec![comparison(Value(3), Value(0), Value(1))],
                Terminator::Branch {
                    condition: Value(3),
                    then_target: Block(1),
                    then_arguments: vec![],
                    else_target: Block(2),
                    else_arguments: vec![],
                },
            ),
            block(vec![], Terminator::Return(Value(2))),
        ];
        verify(&f);
        assert!(run(&mut f, &[], &mut HashMap::new()));
        verify(&f);
        assert!(f.blocks[1].instructions.is_empty());
        let mut f = fixture();
        f.blocks = vec![
            block(
                vec![],
                Terminator::Jump {
                    target: Block(1),
                    arguments: vec![],
                },
            ),
            block(
                vec![comparison(Value(2), Value(0), Value(1))],
                Terminator::Branch {
                    condition: Value(2),
                    then_target: Block(2),
                    then_arguments: vec![],
                    else_target: Block(3),
                    else_arguments: vec![],
                },
            ),
            block(vec![], Terminator::Return(Value(2))),
            block(
                vec![I::Portable(P::Binary {
                    destination: Value(3),
                    operator: BinaryOp::Greater,
                    left: Value(0),
                    right: Value(1),
                })],
                Terminator::Return(Value(3)),
            ),
        ];
        // Put equal expressions in siblings, with an independent branch condition.
        f.blocks[0] = block(
            vec![comparison(Value(2), Value(0), Value(1))],
            Terminator::Branch {
                condition: Value(2),
                then_target: Block(1),
                then_arguments: vec![],
                else_target: Block(3),
                else_arguments: vec![],
            },
        );
        let mut values = std::mem::take(&mut f.values).into_builder();
        let extra = values.allocate(Type::Bool, None);
        f.values = values.finish();
        f.blocks[1] = block(
            vec![I::Portable(P::Binary {
                destination: extra,
                operator: BinaryOp::Greater,
                left: Value(0),
                right: Value(1),
            })],
            Terminator::Jump {
                target: Block(2),
                arguments: vec![],
            },
        );
        verify(&f);
        assert!(!run(&mut f, &[], &mut HashMap::new()));
    }
    #[test]
    fn excludes_trapping_arithmetic_floats_and_exposed_storage() {
        for operator in [
            BinaryOp::Add,
            BinaryOp::Subtract,
            BinaryOp::Multiply,
            BinaryOp::Divide,
            BinaryOp::ShiftLeft,
            BinaryOp::ShiftRight,
        ] {
            let mut f = fixture();
            let mut values = std::mem::take(&mut f.values).into_builder();
            let shift = matches!(operator, BinaryOp::ShiftLeft | BinaryOp::ShiftRight);
            let result = if shift { Type::Byte } else { Type::Int };
            values[0] = result;
            values[2] = result;
            values[3] = result;
            f.values = values.finish();
            f.signature.parameters[0] = result;
            f.signature.result = result;
            for entry in &mut f.blocks[0].instructions {
                if let I::Portable(P::Binary { operator: op, .. }) = &mut entry.instruction {
                    *op = operator;
                }
            }
            verify(&f);
            assert!(!run(&mut f, &[], &mut HashMap::new()));
        }
        let mut f = fixture();
        let mut values = std::mem::take(&mut f.values).into_builder();
        values[0] = Type::Float;
        values[1] = Type::Float;
        f.values = values.finish();
        f.signature.parameters = vec![Type::Float, Type::Float];
        verify(&f);
        assert!(!run(&mut f, &[], &mut HashMap::new()));
        let mut f = fixture();
        let mut values = std::mem::take(&mut f.values).into_builder();
        values.set_storage_hint(Value(0), Some(0)); // Parameter storage may be borrowed/mutated.
        f.values = values.finish();
        verify(&f);
        assert!(!run(&mut f, &[], &mut HashMap::new()));
    }
    #[test]
    fn reused_result_detaches_from_overwritten_vm_home() {
        let mut f = fixture();
        f.parameters.clear();
        f.signature.parameters.clear();
        let mut values = std::mem::take(&mut f.values).into_builder();
        values.set_storage_hint(Value(2), Some(2));
        let replacement = values.allocate(Type::Bool, Some(2));
        f.values = values.finish();
        f.blocks[0].instructions.insert(
            1,
            SpannedInstruction {
                instruction: I::Portable(P::LoadConstant {
                    destination: replacement,
                    constant: 0,
                }),
                span: 7..8,
            },
        );
        f.blocks[0].instructions.splice(
            0..0,
            [
                I::Portable(P::LoadConstant {
                    destination: Value(0),
                    constant: 1,
                })
                .with_span(0..1),
                I::Portable(P::LoadConstant {
                    destination: Value(1),
                    constant: 2,
                })
                .with_span(0..1),
            ],
        );
        let constants = vec![
            Constant::Bool(false),
            Constant::Integer(1),
            Constant::Integer(2),
        ];
        let execute = |function: &ir::Function| {
            let id = FunctionId::from_raw(la_arena::RawIdx::from_u32(0));
            let mut program = crate::vm::Program::default();
            program.metadata.main = Some(id);
            program.metadata.constants = constants.clone();
            let body = crate::codegen::vm::lower_function(
                function,
                &HashMap::new(),
                &mut program.metadata.constants,
                crate::codegen::vm::FunctionMetadata {
                    result_type: Some(crate::codegen::types::ExecutableType::Bool),
                    ..Default::default()
                },
            )
            .unwrap();
            program.functions.insert(id, body);
            crate::vm::verify(&program).unwrap();
            crate::vm::Machine::new(&program).run_main().unwrap()
        };
        verify(&f);
        assert_eq!(execute(&f), crate::vm::Value::Bool(true));
        assert!(run(&mut f, &constants, &mut HashMap::new()));
        verify(&f);
        assert_eq!(f.values.hint(2), None);
        assert_eq!(execute(&f), crate::vm::Value::Bool(true));
    }

    #[test]
    fn source_loop_eliminates_comparisons_but_keeps_checked_additions() {
        let compilation =
            crate::compile(include_str!("../../../benchmarks/scalar_cse.fos")).unwrap();
        let shared = crate::codegen::compile(&compilation).unwrap();
        let id = shared.metadata().main.unwrap();
        let counts = |f: &ir::Function, op: BinaryOp| {
            f.blocks.iter().flat_map(|b| &b.instructions)
            .filter(|entry| matches!(entry.instruction, I::Portable(P::Binary { operator, .. }) if operator == op)).count()
        };
        let before = &shared.functions()[&id];
        let comparisons = counts(before, BinaryOp::Less);
        let additions = counts(before, BinaryOp::Add);
        let optimized = shared.optimized().unwrap();
        let after = &optimized.functions()[&id];
        assert!(counts(after, BinaryOp::Less) < comparisons);
        assert_eq!(counts(after, BinaryOp::Add), additions);
    }

    #[test]
    fn moved_scalar_is_not_reused_after_ownership_transfer() {
        let mut f = fixture();
        let mut values = std::mem::take(&mut f.values).into_builder();
        let moved = values.allocate(Type::Bool, None);
        f.values = values.finish();
        f.blocks[0].instructions.insert(
            1,
            I::Portable(P::MoveOut {
                destination: moved,
                source: Value(2),
                by_reference: false,
            })
            .with_span(5..6),
        );
        verify(&f);
        assert!(!run(&mut f, &[], &mut HashMap::new()));
        assert_eq!(f.blocks[0].instructions.len(), 3);
    }
}
