//! Shared semantic optimization, before backend representation and cleanup lowering.
use std::collections::{HashMap, HashSet};

use crate::ast::{BinaryOp, UnaryOp};
use crate::hir::{CaptureMode, FunctionId};

use super::ir::{self, Instruction as I, PortableInstruction as P, Value};
use super::metadata::Constant;

mod cse;
mod graph;
mod inlining;

pub(crate) fn run_shared(
    program: &mut super::program::Program,
    writes: &mut HashMap<FunctionId, HashMap<Value, Value>>,
) -> Result<HashSet<FunctionId>, crate::error::FosterError> {
    let signatures = program
        .bodies
        .iter()
        .map(|(id, body)| (*id, body.signature.clone()))
        .collect::<HashMap<_, _>>();
    let verify = |body: &ir::Function| {
        body.verify(&signatures).map_err(|error| {
            crate::error::FosterError::runtime(format!(
                "invalid optimized SSA in {}: {error}",
                body.name
            ))
        })
    };
    let mut changed = inlining::run(program, writes);
    let mut verified = changed.iter().copied().collect::<Vec<_>>();
    verified.sort();
    for id in &verified {
        verify(&program.bodies[id])?;
    }
    let mut ids = program.bodies.keys().copied().collect::<Vec<_>>();
    ids.sort();
    let mut pool = HashMap::new();
    for (index, constant) in program.metadata.constants.iter().enumerate() {
        if let (Some(value), Ok(index)) = (Scalar::from_pool(constant), u16::try_from(index)) {
            pool.entry(value).or_insert(index);
        }
    }
    for id in ids {
        let function = program.bodies.get_mut(&id).unwrap();
        // Branch pruning can expose another join to constant propagation.
        for _ in 0..4 {
            if !graph::simplify(
                function,
                &mut program.metadata.constants,
                writes.get_mut(&id).unwrap(),
                &mut pool,
            ) {
                break;
            }
            verify(function)?;
            changed.insert(id);
        }
        if crate::compiler::profile::measure("shared.cse", || {
            cse::run(
                function,
                &program.metadata.constants,
                writes.get_mut(&id).unwrap(),
            )
        }) {
            graph::simplify(
                function,
                &mut program.metadata.constants,
                writes.get_mut(&id).unwrap(),
                &mut pool,
            );
            verify(function)?;
            changed.insert(id);
        }
    }
    Ok(changed)
}
#[cfg(test)]
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Report {
    pub folded_scalars: usize,
}

/// The caller owns the working graph; publish it only after this succeeds.
#[cfg(test)]
pub(crate) fn optimize(
    function: &mut ir::Function,
    signatures: &HashMap<FunctionId, ir::Signature>,
    constants: &[Constant],
) -> Result<Report, ir::VerifyError> {
    function.verify(signatures)?;
    let folded_scalars = fold_scalars(function, constants);
    function.verify(signatures)?;
    Ok(Report { folded_scalars })
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Scalar {
    Int(i64),
    Bool(bool),
    Float(u64),
    CodePoint(char),
    Unit,
}

impl Scalar {
    #[cfg(test)]
    fn constant(self) -> ir::Constant {
        match self {
            Self::Int(value) => ir::Constant::Integer(value),
            Self::Bool(value) => ir::Constant::Bool(value),
            Self::Float(value) => ir::Constant::Float(f64::from_bits(value)),
            Self::CodePoint(value) => ir::Constant::CodePoint(value),
            Self::Unit => ir::Constant::Unit,
        }
    }
    fn ty(self) -> ir::Type {
        match self {
            Self::Int(_) => ir::Type::Int,
            Self::Bool(_) => ir::Type::Bool,
            Self::Float(_) => ir::Type::Float,
            Self::CodePoint(_) => ir::Type::CodePoint,
            Self::Unit => ir::Type::Unit,
        }
    }
    fn from_pool(value: &Constant) -> Option<Self> {
        Some(match value {
            Constant::Integer(value) => Self::Int(*value),
            Constant::Bool(value) => Self::Bool(*value),
            Constant::Float(value) => Self::Float(value.to_bits()),
            Constant::CodePoint(value) => Self::CodePoint(*value),
            Constant::Unit => Self::Unit,
            _ => return None,
        })
    }
    fn pool(self) -> Constant {
        match self {
            Self::Int(value) => Constant::Integer(value),
            Self::Bool(value) => Constant::Bool(value),
            Self::Float(value) => Constant::Float(f64::from_bits(value)),
            Self::CodePoint(value) => Constant::CodePoint(value),
            Self::Unit => Constant::Unit,
        }
    }
}

fn scalar_instruction(instruction: &I) -> bool {
    matches!(
        instruction,
        I::Constant { .. }
            | I::Unary { .. }
            | I::Binary { .. }
            | I::Portable(
                P::LoadConstant { .. } | P::Move { .. } | P::Unary { .. } | P::Binary { .. }
            )
    )
}

fn exposed_homes(function: &ir::Function) -> HashSet<u16> {
    exposed_homes_with(function, std::iter::empty())
}

fn exposed_homes_with(
    function: &ir::Function,
    additional: impl IntoIterator<Item = u16>,
) -> HashSet<u16> {
    let mut exposed = function
        .parameters
        .iter()
        .copied()
        .chain(function.captures.iter().map(|capture| capture.value))
        .filter_map(|value| function.values.hint(value.index()))
        .collect::<HashSet<_>>();
    exposed.extend(additional);
    let mut expose = |value: Value| {
        if let Some(home) = function.values.hint(value.index()) {
            exposed.insert(home);
        }
    };
    for entry in function.blocks.iter().flat_map(|block| &block.instructions) {
        if !scalar_instruction(&entry.instruction) {
            for value in entry.destinations() {
                expose(value);
            }
        }
        match &entry.instruction {
            I::Portable(
                P::MakeReference { object, .. }
                | P::MakeWholeReference { object, .. }
                | P::MakeFieldReference { object, .. }
                | P::LoadField {
                    object,
                    by_reference: true,
                    ..
                },
            ) => expose(*object),
            I::Portable(P::MakeClosure { captures, .. } | P::CallClosure { captures, .. }) => {
                for (mode, value) in captures {
                    if *mode == CaptureMode::Ref {
                        expose(*value);
                    }
                }
            }
            _ => {}
        }
    }
    loop {
        let before = exposed.len();
        for block in &function.blocks {
            for entry in &block.instructions {
                if let I::Portable(P::Move {
                    destination,
                    source,
                }) = &entry.instruction
                {
                    expose_alias(function, &mut exposed, *destination, *source);
                }
            }
            // Place handles can also travel through SSA joins and loop edges.
            let mut edge = |target: ir::Block, arguments: &[Value]| {
                for (source, destination) in arguments
                    .iter()
                    .zip(&function.blocks[target.0 as usize].parameters)
                {
                    expose_alias(function, &mut exposed, *source, *destination);
                }
            };
            match &block.terminator {
                ir::Terminator::Jump { target, arguments } => edge(*target, arguments),
                ir::Terminator::Branch {
                    then_target,
                    then_arguments,
                    else_target,
                    else_arguments,
                    ..
                } => {
                    edge(*then_target, then_arguments);
                    edge(*else_target, else_arguments);
                }
                ir::Terminator::Return(_) => {}
            }
        }
        for (source, destination) in function
            .entry_arguments
            .iter()
            .zip(&function.blocks[function.entry.0 as usize].parameters)
        {
            expose_alias(function, &mut exposed, *source, *destination);
        }
        if before == exposed.len() {
            return exposed;
        }
    }
}

fn expose_alias(function: &ir::Function, exposed: &mut HashSet<u16>, left: Value, right: Value) {
    let homes = [
        function.values.hint(left.index()),
        function.values.hint(right.index()),
    ];
    if homes.iter().flatten().any(|home| exposed.contains(home)) {
        exposed.extend(homes.into_iter().flatten());
    }
}

#[cfg(test)]
fn fold_scalars(function: &mut ir::Function, constants: &[Constant]) -> usize {
    let exposed = exposed_homes(function);
    let mut folded = 0;
    for block in &mut function.blocks {
        let mut known: HashMap<Value, Scalar> = HashMap::new();
        let mut home_values = HashMap::new();
        for entry in &mut block.instructions {
            // Calls, mutation, allocation, ownership operations and unknown
            // instructions invalidate facts here, not throughout the function.
            if !scalar_instruction(&entry.instruction) {
                known.clear();
                home_values.clear();
                continue;
            }
            let evaluated = evaluate(&entry.instruction, &known, constants);
            for destination in entry.destinations() {
                if let Some(home) = function.values.hint(destination.index())
                    && let Some(previous) = home_values.insert(home, destination)
                {
                    known.remove(&previous);
                }
            }
            let Some((destination, value)) = evaluated else {
                continue;
            };
            if function
                .values
                .hint(destination.index())
                .is_some_and(|home| exposed.contains(&home))
                || function.values[destination.index()] != value.ty()
            {
                continue;
            }
            if matches!(
                entry.instruction,
                I::Unary { .. }
                    | I::Binary { .. }
                    | I::Portable(P::Unary { .. } | P::Binary { .. })
            ) {
                entry.instruction = I::Constant {
                    destination,
                    value: value.constant(),
                };
                folded += 1;
            }
            known.insert(destination, value);
        }
    }
    folded
}

fn evaluate(
    instruction: &I,
    known: &HashMap<Value, Scalar>,
    constants: &[Constant],
) -> Option<(Value, Scalar)> {
    let (destination, value) = match instruction {
        I::Constant {
            destination,
            value: ir::Constant::Integer(value),
        } => (*destination, Scalar::Int(*value)),
        I::Constant {
            destination,
            value: ir::Constant::Bool(value),
        } => (*destination, Scalar::Bool(*value)),
        I::Portable(P::LoadConstant {
            destination,
            constant,
        }) => (
            *destination,
            Scalar::from_pool(constants.get(usize::from(*constant))?)?,
        ),
        I::Portable(P::Move {
            destination,
            source,
        }) => (*destination, *known.get(source)?),
        I::Unary {
            destination,
            operator,
            operand,
        }
        | I::Portable(P::Unary {
            destination,
            operator,
            operand,
        }) => (
            *destination,
            match (operator, known.get(operand)?) {
                (UnaryOp::Negate, Scalar::Int(value)) => Scalar::Int(value.checked_neg()?),
                (UnaryOp::Not, Scalar::Bool(value)) => Scalar::Bool(!value),
                (UnaryOp::Negate, Scalar::Float(value)) => {
                    Scalar::Float((-f64::from_bits(*value)).to_bits())
                }
                (UnaryOp::Negate, Scalar::CodePoint(value)) => Scalar::Int(-(*value as i64)),
                _ => return None,
            },
        ),
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
        }) => (
            *destination,
            binary(*operator, *known.get(left)?, *known.get(right)?)?,
        ),
        _ => return None,
    };
    Some((destination, value))
}

fn binary(operator: BinaryOp, left: Scalar, right: Scalar) -> Option<Scalar> {
    use BinaryOp::*;
    let integer = |value| match value {
        Scalar::CodePoint(value) => Scalar::Int(value as i64),
        other => other,
    };
    let (left, right) = (integer(left), integer(right));
    Some(match (left, right) {
        (Scalar::Float(a), Scalar::Float(b)) => {
            let (a, b) = (f64::from_bits(a), f64::from_bits(b));
            match operator {
                Add => Scalar::Float((a + b).to_bits()),
                Subtract => Scalar::Float((a - b).to_bits()),
                Multiply => Scalar::Float((a * b).to_bits()),
                Divide => Scalar::Float((a / b).to_bits()),
                Equal => Scalar::Bool(a == b),
                NotEqual => Scalar::Bool(a != b),
                Less => Scalar::Bool(a < b),
                LessEqual => Scalar::Bool(a <= b),
                Greater => Scalar::Bool(a > b),
                GreaterEqual => Scalar::Bool(a >= b),
                _ => return None,
            }
        }
        (Scalar::Unit, Scalar::Unit) => match operator {
            Equal => Scalar::Bool(true),
            NotEqual => Scalar::Bool(false),
            _ => return None,
        },
        (Scalar::Int(a), Scalar::Int(b)) => match operator {
            Add => Scalar::Int(a.checked_add(b)?),
            Subtract => Scalar::Int(a.checked_sub(b)?),
            Multiply => Scalar::Int(a.checked_mul(b)?),
            Divide => Scalar::Int(a.checked_div(b)?),
            Equal => Scalar::Bool(a == b),
            NotEqual => Scalar::Bool(a != b),
            Less => Scalar::Bool(a < b),
            LessEqual => Scalar::Bool(a <= b),
            Greater => Scalar::Bool(a > b),
            GreaterEqual => Scalar::Bool(a >= b),
            _ => return None,
        },
        (Scalar::Bool(a), Scalar::Bool(b)) => match operator {
            Equal => Scalar::Bool(a == b),
            NotEqual => Scalar::Bool(a != b),
            _ => return None,
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_pass_folds_portable_and_native_ssa_without_moving_sites() {
        let compilation = crate::compile("func main() -> Int { (20 + 22) * 2 }").unwrap();
        let shared = crate::vm::compile_shared(&compilation).unwrap();
        let id = shared.metadata().main.unwrap();
        let mut function = shared.functions()[&id].clone();
        let sites = |f: &ir::Function| {
            f.blocks
                .iter()
                .map(|b| {
                    b.instructions
                        .iter()
                        .map(|i| i.span.clone())
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        };
        let before = sites(&function);
        let values = function.values.len();
        let report = optimize(
            &mut function,
            shared.signatures(),
            &shared.metadata().constants,
        )
        .unwrap();
        assert!(report.folded_scalars > 0);
        assert_eq!(sites(&function), before);
        assert_eq!(function.values.len(), values);
        assert_eq!(
            optimize(
                &mut function,
                shared.signatures(),
                &shared.metadata().constants
            )
            .unwrap()
            .folded_scalars,
            0
        );

        let prepared = crate::native::prepare(&compilation).unwrap();
        let mut native = prepared.functions()[0].ir().clone();
        let signatures = HashMap::new(); // This source contains no calls.
        let before = sites(&native);
        assert!(
            optimize(&mut native, &signatures, &shared.metadata().constants)
                .unwrap()
                .folded_scalars
                > 0
        );
        assert_eq!(sites(&native), before);
    }

    #[test]
    fn checked_arithmetic_keeps_failure_and_comparison_semantics() {
        use BinaryOp::*;
        for (op, a, b) in [
            (Add, i64::MAX, 1),
            (Subtract, i64::MIN, 1),
            (Multiply, i64::MAX, 2),
            (Divide, 42, 0),
            (Divide, i64::MIN, -1),
        ] {
            assert!(binary(op, Scalar::Int(a), Scalar::Int(b)).is_none());
        }
        assert!(matches!(
            binary(Less, Scalar::Int(20), Scalar::Int(22)),
            Some(Scalar::Bool(true))
        ));
        assert!(matches!(
            binary(Divide, Scalar::Int(-7), Scalar::Int(2)),
            Some(Scalar::Int(-3))
        ));
    }

    #[test]
    fn block_arguments_carry_exposed_storage_into_successors() {
        let mut values = ir::ValueBuilder::default();
        let parameter = values.allocate(ir::Type::Int, Some(0));
        let alias = values.allocate(ir::Type::Int, Some(1));
        let assigned = values.allocate(ir::Type::Int, Some(1));
        let literal = values.allocate(ir::Type::Int, Some(2));
        let result = values.allocate(ir::Type::Int, Some(3));
        let mut function = ir::Function {
            name: "borrowed_join".into(),
            signature: ir::Signature {
                parameters: vec![ir::Type::Int],
                result: ir::Type::Int,
            },
            parameters: vec![parameter],
            captures: vec![],
            entry_seeds: vec![],
            entry: ir::Block(0),
            entry_arguments: vec![],
            values: values.finish(),
            blocks: vec![
                ir::BlockData {
                    parameters: vec![],
                    instructions: vec![],
                    terminator_span: 0..1,
                    terminator: ir::Terminator::Jump {
                        target: ir::Block(1),
                        arguments: vec![parameter],
                    },
                },
                ir::BlockData {
                    parameters: vec![alias],
                    terminator_span: 4..5,
                    instructions: vec![
                        I::Constant {
                            destination: assigned,
                            value: ir::Constant::Integer(20),
                        }
                        .with_span(1..2),
                        I::Constant {
                            destination: literal,
                            value: ir::Constant::Integer(22),
                        }
                        .with_span(2..3),
                        I::Binary {
                            destination: result,
                            operator: BinaryOp::Add,
                            left: assigned,
                            right: literal,
                        }
                        .with_span(3..4),
                    ],
                    terminator: ir::Terminator::Return(result),
                },
            ],
        };
        // Writing through the joined parameter's home must not establish an
        // immutable scalar fact, even though its machine representation is Int.
        assert_eq!(
            optimize(&mut function, &HashMap::new(), &[])
                .unwrap()
                .folded_scalars,
            0
        );
    }

    #[test]
    fn malformed_ssa_is_rejected_before_rewriting() {
        let compilation = crate::compile("func main() -> Int { 20 + 22 }").unwrap();
        let shared = crate::vm::compile_shared(&compilation).unwrap();
        let mut function = shared.functions()[&shared.metadata().main.unwrap()].clone();
        function.blocks[0].terminator = ir::Terminator::Return(Value(u32::MAX));
        assert!(
            optimize(
                &mut function,
                shared.signatures(),
                &shared.metadata().constants
            )
            .is_err()
        );
    }
}
