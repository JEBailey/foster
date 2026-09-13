use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};

use crate::vm::Value;

use super::super::{Constant, Instruction, Program, Register, operations};
use super::analysis::definitions;

type Constants = HashMap<Register, Constant>;

pub(super) fn fold(program: &mut Program) {
    let (constants, functions) = (&mut program.constants, &mut program.functions);
    let mut interner = None;
    for function in functions.values_mut() {
        // Converge before rewriting: back edges can invalidate initial facts.
        // Keep snapshots only at block entries, not after every instruction.
        let facts = constant_blocks(&function.instructions, constants);
        for (range, incoming) in facts.ranges.into_iter().zip(facts.incoming) {
            let Some(mut known) = incoming else { continue };
            for index in range {
                let instruction = &mut function.instructions[index];
                if let Instruction::JumpIfFalse { condition, target } = instruction
                    && let Some(Constant::Bool(condition)) = known.get(condition)
                {
                    *instruction = Instruction::Jump {
                        target: if *condition { index + 1 } else { *target },
                    };
                } else if matches!(
                    instruction,
                    Instruction::Unary { .. } | Instruction::Binary { .. }
                ) && let Some((destination, value)) =
                    evaluate(instruction, &known, constants)
                    && let Some(constant) = interner
                        .get_or_insert_with(|| ConstantInterner::new(constants))
                        .intern(constants, value)
                {
                    *instruction = Instruction::LoadConstant {
                        destination,
                        constant,
                    };
                }
                known = transfer(instruction, known, constants);
            }
        }
    }
}

struct ConstantBlocks {
    ranges: Vec<std::ops::Range<usize>>,
    incoming: Vec<Option<Constants>>,
}

fn constant_blocks(instructions: &[Instruction], constants: &[Constant]) -> ConstantBlocks {
    let count = instructions.len();
    if count == 0 {
        return ConstantBlocks {
            ranges: Vec::new(),
            incoming: Vec::new(),
        };
    }
    let mut leaders = vec![false; count];
    leaders[0] = true;
    for (index, instruction) in instructions.iter().enumerate() {
        match instruction {
            Instruction::Jump { target } | Instruction::JumpIfFalse { target, .. } => {
                leaders[*target] = true;
            }
            Instruction::Return { .. } => {}
            _ => continue,
        }
        if index + 1 < count {
            leaders[index + 1] = true;
        }
    }
    let mut starts = leaders
        .iter()
        .enumerate()
        .filter_map(|(index, &leader)| leader.then_some(index))
        .collect::<Vec<_>>();
    starts.push(count);
    let ranges = starts
        .windows(2)
        .map(|pair| pair[0]..pair[1])
        .collect::<Vec<_>>();
    let mut block_at = vec![0; count];
    for (block, range) in ranges.iter().enumerate() {
        block_at[range.clone()].fill(block);
    }
    let successors = ranges
        .iter()
        .map(|range| {
            super::analysis::successors(instructions, range.end - 1)
                .into_iter()
                .map(|index| block_at[index])
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    // None is unreachable; missing registers in a reachable map are unknown.
    // Meet retains bit-identical constants from every incoming path.
    let mut incoming = vec![None; ranges.len()];
    incoming[0] = Some(Constants::new());
    let mut pending = VecDeque::from([0]);
    let mut queued = vec![false; ranges.len()];
    queued[0] = true;
    while let Some(block) = pending.pop_front() {
        queued[block] = false;
        let mut outgoing = incoming[block].as_ref().unwrap().clone();
        for index in ranges[block].clone() {
            outgoing = transfer(&instructions[index], outgoing, constants);
        }
        for &next in &successors[block] {
            let changed = match &mut incoming[next] {
                None => {
                    incoming[next] = Some(outgoing.clone());
                    true
                }
                Some(known) => {
                    let old_len = known.len();
                    known.retain(|register, value| {
                        outgoing
                            .get(register)
                            .is_some_and(|other| same_constant(value, other))
                    });
                    known.len() != old_len
                }
            };
            if changed && !queued[next] {
                queued[next] = true;
                pending.push_back(next);
            }
        }
    }
    ConstantBlocks { ranges, incoming }
}

#[cfg(test)]
fn available_constants(
    instructions: &[Instruction],
    constants: &[Constant],
) -> Vec<Option<Constants>> {
    let facts = constant_blocks(instructions, constants);
    let mut incoming = vec![None; instructions.len()];
    for (range, known) in facts.ranges.into_iter().zip(facts.incoming) {
        let Some(mut known) = known else { continue };
        for index in range {
            incoming[index] = Some(known.clone());
            known = transfer(&instructions[index], known, constants);
        }
    }
    incoming
}

fn evaluate(
    instruction: &Instruction,
    known: &Constants,
    constants: &[Constant],
) -> Option<(Register, Constant)> {
    let (destination, value) = match instruction {
        Instruction::LoadConstant {
            destination,
            constant,
        } => (*destination, constants[usize::from(*constant)].clone()),
        Instruction::Move {
            destination,
            source,
        } => (*destination, known.get(source)?.clone()),
        Instruction::Unary {
            destination,
            operator,
            operand,
        } => (
            *destination,
            value_constant(
                operations::unary(
                    *operator,
                    &operations::constant_value(known.get(operand)?, None, None),
                )
                .ok()?,
            )?,
        ),
        Instruction::Binary {
            destination,
            operator,
            left,
            right,
        } => (
            *destination,
            value_constant(
                operations::binary(
                    *operator,
                    &operations::constant_value(known.get(left)?, None, None),
                    &operations::constant_value(known.get(right)?, None, None),
                )
                .ok()?,
            )?,
        ),
        _ => return None,
    };
    Some((destination, value))
}

fn transfer(instruction: &Instruction, mut known: Constants, constants: &[Constant]) -> Constants {
    let value = evaluate(instruction, &known, constants);
    if matches!(
        instruction,
        Instruction::Call { .. } | Instruction::CallValue { .. } | Instruction::CallClosure { .. }
    ) {
        known.clear();
    }
    // Kill definitions even when evaluation fails or an operand is unknown.
    for definition in definitions(instruction) {
        known.remove(&definition);
    }
    if let Instruction::MakeClosure { captures, .. } = instruction {
        for (_, register) in captures {
            known.remove(register);
        }
    }
    if let Some((destination, value)) = value {
        known.insert(destination, value);
    }
    known
}

pub(super) fn deduplicate(program: &mut Program) {
    let old = std::mem::take(&mut program.constants);
    let mut unique = Vec::new();
    let mut interner = ConstantInterner::new(&[]);
    // Most instructions reuse an existing constant index. Remap each old index
    // once rather than hashing its value at every use.
    let mut remapped = vec![None; old.len()];
    for function in program.functions.values_mut() {
        for instruction in &mut function.instructions {
            if let Instruction::LoadConstant { constant, .. } = instruction {
                let old_index = usize::from(*constant);
                *constant = *remapped[old_index].get_or_insert_with(|| {
                    interner
                        .intern(&mut unique, old[old_index].clone())
                        .expect("referenced constants fit the bytecode index space")
                });
            }
        }
    }
    program.constants = unique;
}

fn value_constant(value: Value) -> Option<Constant> {
    match value {
        Value::Unit => Some(Constant::Unit),
        Value::Bool(value) => Some(Constant::Bool(value)),
        Value::Integer(value) => Some(Constant::Integer(value)),
        Value::Float(value) => Some(Constant::Float(value)),
        Value::Record { .. } if value.string_bytes().is_some() => Some(Constant::String(
            std::str::from_utf8(value.string_bytes()?).ok()?.to_owned(),
        )),
        Value::CodePoint(value) => Some(Constant::CodePoint(value)),
        value if value.symbol_bytes().is_some() => Some(Constant::Symbol(
            String::from_utf8(value.symbol_bytes()?.to_vec()).ok()?,
        )),
        _ => None,
    }
}

// Float keys use bit identity: signed zero and distinct NaN payloads must not
// be merged, and identical NaNs must remain reflexive HashMap keys.
#[derive(Clone, Debug)]
struct ConstantKey(Constant);

impl PartialEq for ConstantKey {
    fn eq(&self, other: &Self) -> bool {
        same_constant(&self.0, &other.0)
    }
}
impl Eq for ConstantKey {}
impl Hash for ConstantKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(&self.0).hash(state);
        match &self.0 {
            Constant::Unit => {}
            Constant::Bool(value) => value.hash(state),
            Constant::Integer(value) => value.hash(state),
            Constant::Float(value) => value.to_bits().hash(state),
            Constant::String(value) | Constant::Symbol(value) => value.hash(state),
            Constant::CodePoint(value) => value.hash(state),
        }
    }
}

struct ConstantInterner {
    indices: HashMap<ConstantKey, usize>,
}
impl ConstantInterner {
    fn new(constants: &[Constant]) -> Self {
        let mut indices = HashMap::with_capacity(constants.len());
        for (index, value) in constants.iter().enumerate() {
            // Keep the first index, matching the previous linear search.
            indices.entry(ConstantKey(value.clone())).or_insert(index);
        }
        Self { indices }
    }

    fn intern(&mut self, constants: &mut Vec<Constant>, value: Constant) -> Option<u16> {
        let key = ConstantKey(value.clone());
        if let Some(&index) = self.indices.get(&key) {
            return u16::try_from(index).ok();
        }
        let index = u16::try_from(constants.len()).ok()?;
        self.indices.insert(key, usize::from(index));
        constants.push(value);
        Some(index)
    }
}

fn same_constant(left: &Constant, right: &Constant) -> bool {
    match (left, right) {
        (Constant::Float(left), Constant::Float(right)) => left.to_bits() == right.to_bits(),
        _ => left == right,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::BinaryOp;

    fn load(register: u16, constant: u16) -> Instruction {
        Instruction::LoadConstant {
            destination: Register(register),
            constant,
        }
    }
    fn ret() -> Instruction {
        Instruction::Return {
            source: Register(0),
        }
    }
    fn diamond() -> Vec<Instruction> {
        vec![
            Instruction::JumpIfFalse {
                condition: Register(2),
                target: 3,
            },
            load(0, 0),
            Instruction::Jump { target: 4 },
            load(0, 1),
            ret(),
        ]
    }

    #[test]
    fn joins_require_bit_identical_constants() {
        for (left, right, agrees) in [
            (Constant::Integer(42), Constant::Integer(42), true),
            (Constant::Integer(42), Constant::Integer(43), false),
            (Constant::Float(0.0), Constant::Float(-0.0), false),
            (Constant::Float(f64::NAN), Constant::Float(f64::NAN), true),
        ] {
            let facts = available_constants(&diamond(), &[left, right]);
            assert_eq!(
                facts[4].as_ref().unwrap().contains_key(&Register(0)),
                agrees
            );
        }
    }

    #[test]
    fn loop_back_edges_invalidate_constants_but_keep_invariants() {
        let instructions = vec![
            load(0, 0),
            load(1, 0),
            Instruction::JumpIfFalse {
                condition: Register(2),
                target: 5,
            },
            load(0, 1),
            Instruction::Jump { target: 2 },
            ret(),
        ];
        let facts =
            available_constants(&instructions, &[Constant::Integer(1), Constant::Integer(2)]);
        let header = facts[2].as_ref().unwrap();
        assert!(!header.contains_key(&Register(0)));
        assert_eq!(header.get(&Register(1)), Some(&Constant::Integer(1)));
    }

    #[test]
    fn unreachable_predecessors_do_not_destroy_facts() {
        let instructions = vec![
            load(0, 0),
            Instruction::Jump { target: 3 },
            load(0, 1),
            ret(),
        ];
        let facts = available_constants(
            &instructions,
            &[Constant::Integer(42), Constant::Integer(0)],
        );
        assert!(facts[2].is_none());
        assert_eq!(
            facts[3].as_ref().unwrap().get(&Register(0)),
            Some(&Constant::Integer(42))
        );
    }

    #[test]
    fn calls_and_failed_evaluation_kill_stale_facts() {
        let known = Constants::from([(Register(0), Constant::Integer(1))]);
        let unknown = Instruction::Binary {
            destination: Register(0),
            operator: BinaryOp::Add,
            left: Register(0),
            right: Register(1),
        };
        assert!(transfer(&unknown, known.clone(), &[]).is_empty());
        let zero = Constants::from([(Register(0), Constant::Integer(0))]);
        let division = Instruction::Binary {
            destination: Register(0),
            operator: BinaryOp::Divide,
            left: Register(0),
            right: Register(0),
        };
        assert!(transfer(&division, zero, &[]).is_empty());
        let call = Instruction::CallValue {
            destination: Register(1),
            callee: Register(2),
            arguments: vec![],
        };
        assert!(transfer(&call, known, &[]).is_empty());
    }
    #[test]
    fn interning_preserves_typed_bit_identity_and_first_indices() {
        let nan = f64::from_bits(0x7ff8_0000_0000_0001);
        let mut pool = vec![
            Constant::Float(nan),
            Constant::Float(nan),
            Constant::Float(f64::from_bits(0x7ff8_0000_0000_0002)),
            Constant::Float(0.0),
            Constant::Float(-0.0),
            Constant::Integer(1),
            Constant::Float(1.0),
            Constant::Bool(true),
            Constant::String("x".into()),
            Constant::Symbol("x".into()),
            Constant::CodePoint('x'),
            Constant::Unit,
        ];
        let original = pool.clone();
        let mut interner = ConstantInterner::new(&pool);
        for value in &original {
            let expected = original
                .iter()
                .position(|other| same_constant(value, other))
                .unwrap() as u16;
            assert_eq!(interner.intern(&mut pool, value.clone()), Some(expected));
        }
        assert_eq!(pool.len(), original.len());
        let added = Constant::String("new".into());
        assert_eq!(
            interner.intern(&mut pool, added.clone()),
            Some(original.len() as u16)
        );
        assert_eq!(
            interner.intern(&mut pool, added),
            Some(original.len() as u16)
        );
    }

    #[test]
    fn full_pool_can_reuse_constants_but_cannot_append() {
        let mut pool = (0..=u16::MAX)
            .map(|index| Constant::Integer(i64::from(index)))
            .collect::<Vec<_>>();
        let mut interner = ConstantInterner::new(&pool);
        assert_eq!(interner.intern(&mut pool, Constant::Integer(42)), Some(42));
        assert_eq!(interner.intern(&mut pool, Constant::Integer(-1)), None);
        assert_eq!(pool.len(), usize::from(u16::MAX) + 1);
    }

    #[test]
    fn deduplication_remaps_repeated_indices_without_changing_values() {
        let compilation = crate::compile("func main() -> Int { 0 }").unwrap();
        let mut program = crate::vm::compile_library(&compilation).unwrap();
        let nan = f64::from_bits(0x7ff8_0000_0000_0001);
        let old = vec![
            Constant::Float(nan),
            Constant::Float(nan),
            Constant::Float(0.0),
            Constant::Float(-0.0),
            Constant::Integer(99),
        ];
        let indices = [3, 1, 0, 2, 3, 1];
        let function = program
            .functions
            .values_mut()
            .find(|f| f.name == "main")
            .unwrap();
        function.instructions = indices.iter().map(|&index| load(0, index)).collect();
        program.constants = old.clone();
        deduplicate(&mut program);
        let function = program
            .functions
            .values()
            .find(|f| f.name == "main")
            .unwrap();
        for (&old_index, instruction) in indices.iter().zip(&function.instructions) {
            let Instruction::LoadConstant { constant, .. } = instruction else {
                unreachable!()
            };
            assert!(same_constant(
                &old[usize::from(old_index)],
                &program.constants[usize::from(*constant)]
            ));
        }
        assert_eq!(program.constants.len(), 3);
    }
    fn reference_constants(
        instructions: &[Instruction],
        constants: &[Constant],
    ) -> Vec<Option<Constants>> {
        // None means unreachable; a missing register in a reachable map is unknown.
        // Meet retains only bit-identical constants from all incoming paths.
        let mut incoming = vec![None; instructions.len()];
        if instructions.is_empty() {
            return incoming;
        }
        incoming[0] = Some(Constants::new());
        let mut pending = VecDeque::from([0]);
        let mut queued = vec![false; instructions.len()];
        queued[0] = true;
        while let Some(index) = pending.pop_front() {
            queued[index] = false;
            let outgoing = transfer(
                &instructions[index],
                incoming[index].as_ref().unwrap().clone(),
                constants,
            );
            for next in crate::vm::optimizer::analysis::successors(instructions, index) {
                let changed = match &mut incoming[next] {
                    None => {
                        incoming[next] = Some(outgoing.clone());
                        true
                    }
                    Some(known) => {
                        let old_len = known.len();
                        known.retain(|register, value| {
                            outgoing
                                .get(register)
                                .is_some_and(|other| same_constant(value, other))
                        });
                        known.len() != old_len
                    }
                };
                if changed && !queued[next] {
                    queued[next] = true;
                    pending.push_back(next);
                }
            }
        }
        incoming
    }

    #[test]
    fn block_states_match_instruction_states_on_generated_control_flow() {
        let constants = [
            Constant::Integer(1),
            Constant::Integer(2),
            Constant::Bool(true),
        ];
        let mut seed = 19u32;
        for case in 0..32 {
            let mut next = || {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                seed
            };
            let instructions = (0..32)
                .map(|_| {
                    let opcode = next() % 7;
                    let destination = Register((next() % 8) as u16);
                    let source = Register((next() % 8) as u16);
                    let target = next() as usize % 32;
                    match opcode {
                        0 => load(destination.0, (next() % 3) as u16),
                        1 => Instruction::Move {
                            destination,
                            source,
                        },
                        2 => Instruction::Binary {
                            destination,
                            operator: BinaryOp::Add,
                            left: source,
                            right: Register(0),
                        },
                        3 => Instruction::JumpIfFalse {
                            condition: source,
                            target,
                        },
                        4 => Instruction::Jump { target },
                        5 => Instruction::Return { source },
                        _ => Instruction::CallValue {
                            destination,
                            callee: source,
                            arguments: vec![],
                        },
                    }
                })
                .collect::<Vec<_>>();
            assert_eq!(
                available_constants(&instructions, &constants),
                reference_constants(&instructions, &constants),
                "case {case}"
            );
        }
    }
}
