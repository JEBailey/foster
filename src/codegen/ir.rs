//! Typed, block-structured SSA IR shared by native code-generation stages.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;

use crate::ast::{BinaryOp, UnaryOp};
use crate::hir::FunctionId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Representation {
    I8,
    I32,
    I64,
    F64,
    Pointer,
}

impl fmt::Display for Representation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::I8 => "i8",
            Self::I32 => "i32",
            Self::I64 => "i64",
            Self::F64 => "f64",
            Self::Pointer => "ptr",
        })
    }
}

/// A Foster scalar type retained until target-specific representation lowering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Type {
    Unit,
    Bool,
    Int,
    Float,
    CodePoint,
    Byte,
    String,
    Arguments,
    StringList,
}

impl Type {
    pub fn representation(self) -> Representation {
        match self {
            Self::Unit | Self::Bool | Self::Byte => Representation::I8,
            Self::CodePoint => Representation::I32,
            Self::Int => Representation::I64,
            Self::Float => Representation::F64,
            Self::String | Self::Arguments | Self::StringList => Representation::Pointer,
        }
    }
}

impl fmt::Display for Type {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}/{}", self, self.representation())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    pub parameters: Vec<Type>,
    pub result: Type,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Value(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Block(pub u32);

#[derive(Debug)]
pub struct Function {
    pub name: String,
    pub signature: Signature,
    /// SSA definitions supplied by the caller before the implicit entry edge.
    pub parameters: Vec<Value>,
    pub entry: Block,
    pub entry_arguments: Vec<Value>,
    pub value_types: Vec<Type>,
    pub blocks: Vec<BlockData>,
}

impl Function {
    pub fn value_type(&self, value: Value) -> Type {
        self.value_types[value.0 as usize]
    }

    pub fn verify(&self, signatures: &HashMap<FunctionId, Signature>) -> Result<(), VerifyError> {
        Verifier::new(self, signatures).verify()
    }
}

#[derive(Debug)]
pub struct BlockData {
    pub parameters: Vec<Value>,
    pub instructions: Vec<Instruction>,
    pub terminator: Terminator,
}

#[derive(Debug)]
pub enum Instruction {
    Constant {
        destination: Value,
        value: Constant,
    },
    Unary {
        destination: Value,
        operator: UnaryOp,
        operand: Value,
    },
    IntegerExtend {
        destination: Value,
        operand: Value,
    },
    Binary {
        destination: Value,
        operator: BinaryOp,
        left: Value,
        right: Value,
    },
    Call {
        destination: Value,
        function: FunctionId,
        arguments: Vec<Value>,
    },
    RuntimeCall {
        destination: Value,
        helper: &'static str,
        signature: Signature,
        arguments: Vec<Value>,
    },
    Assert {
        condition: Value,
        message: Option<Value>,
    },
}

impl Instruction {
    pub fn destination(&self) -> Option<Value> {
        match self {
            Self::Constant { destination, .. }
            | Self::Unary { destination, .. }
            | Self::IntegerExtend { destination, .. }
            | Self::Binary { destination, .. }
            | Self::Call { destination, .. }
            | Self::RuntimeCall { destination, .. } => Some(*destination),
            Self::Assert { .. } => None,
        }
    }

    fn operands(&self) -> Vec<Value> {
        match self {
            Self::Constant { .. } => Vec::new(),
            Self::Unary { operand, .. } | Self::IntegerExtend { operand, .. } => vec![*operand],
            Self::Binary { left, right, .. } => vec![*left, *right],
            Self::Call { arguments, .. } | Self::RuntimeCall { arguments, .. } => arguments.clone(),
            Self::Assert { condition, message } => {
                let mut operands = vec![*condition];
                operands.extend(message);
                operands
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Constant {
    Unit,
    Bool(bool),
    Integer(i64),
    Float(f64),
    CodePoint(char),
    RuntimeString(u64),
}

impl Constant {
    fn ty(self) -> Type {
        match self {
            Self::Unit => Type::Unit,
            Self::Bool(_) => Type::Bool,
            Self::Integer(_) => Type::Int,
            Self::Float(_) => Type::Float,
            Self::CodePoint(_) => Type::CodePoint,
            Self::RuntimeString(_) => Type::String,
        }
    }
}

#[derive(Debug)]
pub enum Terminator {
    Jump {
        target: Block,
        arguments: Vec<Value>,
    },
    Branch {
        condition: Value,
        then_target: Block,
        then_arguments: Vec<Value>,
        else_target: Block,
        else_arguments: Vec<Value>,
    },
    Return(Value),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyError {
    message: String,
}

impl VerifyError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for VerifyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for VerifyError {}

#[derive(Clone, Copy)]
enum Definition {
    Parameter,
    BlockParameter(Block),
    Instruction(Block, usize),
}

struct Verifier<'a> {
    function: &'a Function,
    signatures: &'a HashMap<FunctionId, Signature>,
    definitions: HashMap<Value, Definition>,
    predecessors: Vec<Vec<Block>>,
    dominators: Vec<HashSet<Block>>,
    reachable: HashSet<Block>,
}

impl<'a> Verifier<'a> {
    fn new(function: &'a Function, signatures: &'a HashMap<FunctionId, Signature>) -> Self {
        Self {
            function,
            signatures,
            definitions: HashMap::new(),
            predecessors: vec![Vec::new(); function.blocks.len()],
            dominators: vec![HashSet::new(); function.blocks.len()],
            reachable: HashSet::new(),
        }
    }

    fn verify(mut self) -> Result<(), VerifyError> {
        if self.function.blocks.is_empty() {
            return Err(VerifyError::new("function has no blocks"));
        }
        self.block(self.function.entry)?;
        if self.function.parameters.len() != self.function.signature.parameters.len() {
            return Err(VerifyError::new(format!(
                "function parameter count {} does not match signature count {}",
                self.function.parameters.len(),
                self.function.signature.parameters.len()
            )));
        }
        for (value, ty) in self
            .function
            .parameters
            .iter()
            .zip(&self.function.signature.parameters)
        {
            self.define(*value, Definition::Parameter)?;
            self.require_type(*value, *ty, "function parameter")?;
        }
        for (block_index, block) in self.function.blocks.iter().enumerate() {
            let block_id = Block(block_index as u32);
            for parameter in &block.parameters {
                self.define(*parameter, Definition::BlockParameter(block_id))?;
            }
            for (instruction_index, instruction) in block.instructions.iter().enumerate() {
                if let Some(destination) = instruction.destination() {
                    self.define(
                        destination,
                        Definition::Instruction(block_id, instruction_index),
                    )?;
                }
            }
            for target in terminator_targets(&block.terminator) {
                self.block(target)?;
                self.predecessors[target.0 as usize].push(block_id);
            }
        }
        if self.definitions.len() != self.function.value_types.len() {
            return Err(VerifyError::new(format!(
                "{} typed values have no unique definition",
                self.function.value_types.len() - self.definitions.len()
            )));
        }
        self.compute_dominators();
        self.verify_edge(
            None,
            &self.function.entry_arguments,
            self.function.entry,
            "entry",
        )?;
        for (block_index, block) in self.function.blocks.iter().enumerate() {
            let block_id = Block(block_index as u32);
            for (instruction_index, instruction) in block.instructions.iter().enumerate() {
                for operand in instruction.operands() {
                    self.verify_use(operand, block_id, instruction_index)?;
                }
                self.verify_instruction(instruction)?;
            }
            self.verify_terminator(block_id, block)?;
        }
        Ok(())
    }

    fn value_type(&self, value: Value) -> Result<Type, VerifyError> {
        self.function
            .value_types
            .get(value.0 as usize)
            .copied()
            .ok_or_else(|| VerifyError::new(format!("value v{} is out of range", value.0)))
    }

    fn block(&self, block: Block) -> Result<&BlockData, VerifyError> {
        self.function
            .blocks
            .get(block.0 as usize)
            .ok_or_else(|| VerifyError::new(format!("block b{} is out of range", block.0)))
    }

    fn define(&mut self, value: Value, definition: Definition) -> Result<(), VerifyError> {
        self.value_type(value)?;
        if self.definitions.insert(value, definition).is_some() {
            return Err(VerifyError::new(format!(
                "value v{} has multiple definitions",
                value.0
            )));
        }
        Ok(())
    }

    fn require_type(&self, value: Value, expected: Type, role: &str) -> Result<(), VerifyError> {
        let found = self.value_type(value)?;
        if found == expected {
            Ok(())
        } else {
            Err(VerifyError::new(format!(
                "{role} v{} has type {found}, expected {expected}",
                value.0
            )))
        }
    }

    fn compute_dominators(&mut self) {
        let entry = self.function.entry;
        let mut queue = VecDeque::from([entry]);
        self.reachable.insert(entry);
        while let Some(block) = queue.pop_front() {
            for successor in terminator_targets(&self.function.blocks[block.0 as usize].terminator)
            {
                if self.reachable.insert(successor) {
                    queue.push_back(successor);
                }
            }
        }
        for block in &self.reachable {
            self.dominators[block.0 as usize] = if *block == entry {
                HashSet::from([entry])
            } else {
                self.reachable.clone()
            };
        }
        loop {
            let mut changed = false;
            for block in self
                .reachable
                .iter()
                .copied()
                .filter(|block| *block != entry)
            {
                let predecessors = self.predecessors[block.0 as usize]
                    .iter()
                    .filter(|predecessor| self.reachable.contains(predecessor));
                let mut intersection: Option<HashSet<Block>> = None;
                for predecessor in predecessors {
                    intersection = Some(match intersection {
                        Some(current) => current
                            .intersection(&self.dominators[predecessor.0 as usize])
                            .copied()
                            .collect(),
                        None => self.dominators[predecessor.0 as usize].clone(),
                    });
                }
                let mut next = intersection.unwrap_or_default();
                next.insert(block);
                if next != self.dominators[block.0 as usize] {
                    self.dominators[block.0 as usize] = next;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }

    fn verify_use(
        &self,
        value: Value,
        block: Block,
        instruction: usize,
    ) -> Result<(), VerifyError> {
        let definition = self.definitions.get(&value).ok_or_else(|| {
            VerifyError::new(format!(
                "use of undefined value v{} in b{}",
                value.0, block.0
            ))
        })?;
        let dominates = match *definition {
            Definition::Parameter => true,
            Definition::BlockParameter(definition) => {
                definition == block
                    || self.reachable.contains(&block)
                        && self.dominators[block.0 as usize].contains(&definition)
            }
            Definition::Instruction(definition, definition_index) => {
                definition == block && definition_index < instruction
                    || definition != block
                        && self.reachable.contains(&block)
                        && self.dominators[block.0 as usize].contains(&definition)
            }
        };
        if dominates {
            Ok(())
        } else {
            Err(VerifyError::new(format!(
                "definition of v{} does not dominate its use in b{}",
                value.0, block.0
            )))
        }
    }

    fn verify_instruction(&self, instruction: &Instruction) -> Result<(), VerifyError> {
        match instruction {
            Instruction::Constant { destination, value } => {
                self.require_type(*destination, value.ty(), "constant result")
            }
            Instruction::Unary {
                destination,
                operator,
                operand,
            } => {
                let operand_type = self.value_type(*operand)?;
                let expected = match operator {
                    UnaryOp::Negate if matches!(operand_type, Type::Int | Type::Float) => {
                        operand_type
                    }
                    UnaryOp::Not if operand_type == Type::Bool => Type::Bool,
                    UnaryOp::BitNot if operand_type == Type::Byte => Type::Byte,
                    _ => {
                        return Err(VerifyError::new(format!(
                            "invalid unary operator {operator:?} for {operand_type}"
                        )));
                    }
                };
                self.require_type(*destination, expected, "unary result")
            }
            Instruction::IntegerExtend {
                destination,
                operand,
            } => {
                let operand_type = self.value_type(*operand)?;
                if !matches!(operand_type, Type::Byte | Type::CodePoint) {
                    return Err(VerifyError::new(format!(
                        "integer extension cannot widen {operand_type}"
                    )));
                }
                self.require_type(*destination, Type::Int, "integer extension result")
            }
            Instruction::Binary {
                destination,
                operator,
                left,
                right,
            } => {
                let operand_type = self.value_type(*left)?;
                self.require_type(*right, operand_type, "binary right operand")?;
                let equality = matches!(operator, BinaryOp::Equal | BinaryOp::NotEqual);
                let ordering = matches!(
                    operator,
                    BinaryOp::Less
                        | BinaryOp::LessEqual
                        | BinaryOp::Greater
                        | BinaryOp::GreaterEqual
                );
                let arithmetic = matches!(
                    operator,
                    BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide
                );
                let bits = matches!(
                    operator,
                    BinaryOp::BitAnd
                        | BinaryOp::BitOr
                        | BinaryOp::BitXor
                        | BinaryOp::ShiftLeft
                        | BinaryOp::ShiftRight
                );
                let valid = equality
                    || ordering
                        && matches!(
                            operand_type,
                            Type::Int | Type::Float | Type::CodePoint | Type::Byte
                        )
                    || arithmetic && matches!(operand_type, Type::Int | Type::Float)
                    || bits && operand_type == Type::Byte;
                if !valid {
                    return Err(VerifyError::new(format!(
                        "invalid binary operator {operator:?} for {operand_type}"
                    )));
                }
                let expected = if equality || ordering {
                    Type::Bool
                } else {
                    operand_type
                };
                self.require_type(*destination, expected, "binary result")
            }
            Instruction::Call {
                destination,
                function,
                arguments,
            } => {
                let signature = self.signatures.get(function).ok_or_else(|| {
                    VerifyError::new(format!("call references missing function {function:?}"))
                })?;
                self.verify_call(arguments, signature, "call")?;
                self.require_type(*destination, signature.result, "call result")
            }
            Instruction::RuntimeCall {
                destination,
                signature,
                arguments,
                ..
            } => {
                self.verify_call(arguments, signature, "runtime call")?;
                self.require_type(*destination, signature.result, "runtime call result")
            }
            Instruction::Assert { condition, message } => {
                self.require_type(*condition, Type::Bool, "assert condition")?;
                if let Some(message) = message {
                    self.require_type(*message, Type::String, "assert message")?;
                }
                Ok(())
            }
        }
    }

    fn verify_call(
        &self,
        arguments: &[Value],
        signature: &Signature,
        role: &str,
    ) -> Result<(), VerifyError> {
        if arguments.len() != signature.parameters.len() {
            return Err(VerifyError::new(format!(
                "{role} has {} arguments, expected {}",
                arguments.len(),
                signature.parameters.len()
            )));
        }
        for (argument, ty) in arguments.iter().zip(&signature.parameters) {
            self.require_type(*argument, *ty, role)?;
        }
        Ok(())
    }

    fn verify_terminator(&self, block_id: Block, block: &BlockData) -> Result<(), VerifyError> {
        let end = block.instructions.len();
        match &block.terminator {
            Terminator::Jump { target, arguments } => {
                for value in arguments {
                    self.verify_use(*value, block_id, end)?;
                }
                self.verify_edge(Some(block_id), arguments, *target, "jump")
            }
            Terminator::Branch {
                condition,
                then_target,
                then_arguments,
                else_target,
                else_arguments,
            } => {
                self.verify_use(*condition, block_id, end)?;
                self.require_type(*condition, Type::Bool, "branch condition")?;
                for value in then_arguments.iter().chain(else_arguments) {
                    self.verify_use(*value, block_id, end)?;
                }
                self.verify_edge(Some(block_id), then_arguments, *then_target, "branch")?;
                self.verify_edge(Some(block_id), else_arguments, *else_target, "branch")
            }
            Terminator::Return(value) => {
                self.verify_use(*value, block_id, end)?;
                self.require_type(*value, self.function.signature.result, "return value")
            }
        }
    }

    fn verify_edge(
        &self,
        source: Option<Block>,
        arguments: &[Value],
        target: Block,
        role: &str,
    ) -> Result<(), VerifyError> {
        let target = self.block(target)?;
        if arguments.len() != target.parameters.len() {
            return Err(VerifyError::new(format!(
                "{role} passes {} arguments to a block with {} parameters",
                arguments.len(),
                target.parameters.len()
            )));
        }
        for (argument, parameter) in arguments.iter().zip(&target.parameters) {
            if source.is_none()
                && !matches!(self.definitions.get(argument), Some(Definition::Parameter))
            {
                return Err(VerifyError::new(format!(
                    "entry argument v{} is not a function parameter",
                    argument.0
                )));
            }
            let expected = self.value_type(*parameter)?;
            self.require_type(*argument, expected, role)?;
        }
        Ok(())
    }
}

fn terminator_targets(terminator: &Terminator) -> Vec<Block> {
    match terminator {
        Terminator::Jump { target, .. } => vec![*target],
        Terminator::Branch {
            then_target,
            else_target,
            ..
        } => vec![*then_target, *else_target],
        Terminator::Return(_) => Vec::new(),
    }
}

impl fmt::Display for Function {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "function {}(", self.name)?;
        for (index, parameter) in self.parameters.iter().enumerate() {
            if index != 0 {
                formatter.write_str(", ")?;
            }
            write!(
                formatter,
                "v{}: {}",
                parameter.0,
                self.value_type(*parameter)
            )?;
        }
        writeln!(formatter, ") -> {} {{", self.signature.result)?;
        write!(formatter, "  entry -> b{}(", self.entry.0)?;
        write_values(formatter, &self.entry_arguments)?;
        writeln!(formatter, ")")?;
        for (index, block) in self.blocks.iter().enumerate() {
            write!(formatter, "  b{index}(")?;
            for (parameter_index, parameter) in block.parameters.iter().enumerate() {
                if parameter_index != 0 {
                    formatter.write_str(", ")?;
                }
                write!(
                    formatter,
                    "v{}: {}",
                    parameter.0,
                    self.value_type(*parameter)
                )?;
            }
            writeln!(formatter, "):")?;
            for instruction in &block.instructions {
                formatter.write_str("    ")?;
                display_instruction(self, instruction, formatter)?;
                writeln!(formatter)?;
            }
            formatter.write_str("    ")?;
            display_terminator(&block.terminator, formatter)?;
            writeln!(formatter)?;
        }
        writeln!(formatter, "}}")
    }
}

fn display_instruction(
    function: &Function,
    instruction: &Instruction,
    formatter: &mut fmt::Formatter<'_>,
) -> fmt::Result {
    if let Some(destination) = instruction.destination() {
        write!(
            formatter,
            "v{}: {} = ",
            destination.0,
            function.value_type(destination)
        )?;
    }
    match instruction {
        Instruction::Constant { value, .. } => write!(formatter, "const {value:?}"),
        Instruction::Unary {
            operator, operand, ..
        } => write!(formatter, "unary {operator:?} v{}", operand.0),
        Instruction::IntegerExtend { operand, .. } => {
            write!(formatter, "integer_extend v{}", operand.0)
        }
        Instruction::Binary {
            operator,
            left,
            right,
            ..
        } => write!(formatter, "binary {operator:?} v{}, v{}", left.0, right.0),
        Instruction::Call {
            function,
            arguments,
            ..
        } => {
            write!(formatter, "call #{}(", function.into_raw().into_u32())?;
            write_values(formatter, arguments)?;
            formatter.write_str(")")
        }
        Instruction::RuntimeCall {
            helper, arguments, ..
        } => {
            write!(formatter, "runtime {helper}(")?;
            write_values(formatter, arguments)?;
            formatter.write_str(")")
        }
        Instruction::Assert { condition, message } => match message {
            Some(message) => write!(formatter, "assert v{}, v{}", condition.0, message.0),
            None => write!(formatter, "assert v{}", condition.0),
        },
    }
}

fn display_terminator(terminator: &Terminator, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    match terminator {
        Terminator::Jump { target, arguments } => {
            write!(formatter, "jump b{}(", target.0)?;
            write_values(formatter, arguments)?;
            formatter.write_str(")")
        }
        Terminator::Branch {
            condition,
            then_target,
            then_arguments,
            else_target,
            else_arguments,
        } => {
            write!(formatter, "branch v{} b{}(", condition.0, then_target.0)?;
            write_values(formatter, then_arguments)?;
            write!(formatter, ") b{}(", else_target.0)?;
            write_values(formatter, else_arguments)?;
            formatter.write_str(")")
        }
        Terminator::Return(value) => write!(formatter, "return v{}", value.0),
    }
}

fn write_values(formatter: &mut fmt::Formatter<'_>, values: &[Value]) -> fmt::Result {
    for (index, value) in values.iter().enumerate() {
        if index != 0 {
            formatter.write_str(", ")?;
        }
        write!(formatter, "v{}", value.0)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifier_rejects_mistyped_entry_arguments() {
        let function = Function {
            name: "invalid".to_owned(),
            signature: Signature {
                parameters: vec![Type::Bool],
                result: Type::Int,
            },
            parameters: vec![Value(0)],
            entry: Block(0),
            entry_arguments: vec![Value(0)],
            value_types: vec![Type::Bool, Type::Int],
            blocks: vec![BlockData {
                parameters: vec![Value(1)],
                instructions: Vec::new(),
                terminator: Terminator::Return(Value(1)),
            }],
        };
        let error = function.verify(&HashMap::new()).unwrap_err();
        assert!(error.to_string().contains("entry v0 has type Bool/i8"));
    }

    #[test]
    fn verifier_rejects_non_dominating_values() {
        let function = Function {
            name: "invalid".to_owned(),
            signature: Signature {
                parameters: Vec::new(),
                result: Type::Int,
            },
            parameters: Vec::new(),
            entry: Block(0),
            entry_arguments: Vec::new(),
            value_types: vec![Type::Int],
            blocks: vec![
                BlockData {
                    parameters: Vec::new(),
                    instructions: Vec::new(),
                    terminator: Terminator::Return(Value(0)),
                },
                BlockData {
                    parameters: Vec::new(),
                    instructions: vec![Instruction::Constant {
                        destination: Value(0),
                        value: Constant::Integer(1),
                    }],
                    terminator: Terminator::Return(Value(0)),
                },
            ],
        };
        let error = function.verify(&HashMap::new()).unwrap_err();
        assert!(error.to_string().contains("does not dominate"));
    }
}
