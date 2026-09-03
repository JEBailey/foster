use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use crate::ast::{BinaryOp, UnaryOp};
use crate::hir::{FunctionId, RecordId, VariantId, VariantTypeId};
use crate::intrinsics::Builtin;
use crate::types::{DispatchSlot, NominalTypeId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Register(pub u16);

/// Concrete generic substitutions attached to a statically resolved call.
///
/// Names are sorted so the same instantiation has one stable bytecode and native-code identity.
pub type Specialization = Vec<(String, VerificationType)>;

/// Runtime-visible type information retained solely for bytecode verification.
///
/// Groups and effects have already served their purpose by this stage. Generic identities and
/// nominal arguments are retained for physical layout selection. `Unknown` is used for erased
/// structural types and acts as the verifier's top type:
/// availability and ownership are still checked, while representation-specific checks are deferred
/// to the already type-checked compiler boundary.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VerificationType {
    Unknown,
    Generic(String),
    Unit,
    Bool,
    Integer,
    Float,
    CodePoint,
    Byte,
    Bytes,
    ByteBuffer,
    List(Box<VerificationType>),
    Reference(Box<VerificationType>),
    Remote(Box<VerificationType>),
    Future(Box<VerificationType>),
    Function {
        parameters: Vec<VerificationType>,
        parameter_modes: Vec<crate::ast::ParameterMode>,
        result: Box<VerificationType>,
    },
    Record {
        record: RecordId,
        arguments: Vec<VerificationType>,
    },
    Variant {
        variant: VariantTypeId,
        arguments: Vec<VerificationType>,
    },
    /// A control-flow join whose alternatives retain different runtime representations.
    Union(Vec<VerificationType>),
}

impl VerificationType {
    pub(crate) fn indexed_element(&self) -> Option<Self> {
        match self {
            Self::Reference(pointee) => pointee.indexed_element(),
            Self::List(element) => Some((**element).clone()),
            Self::ByteBuffer => Some(Self::Byte),
            Self::Unknown | Self::Generic(_) => Some(Self::Unknown),
            _ => None,
        }
    }

    pub(crate) fn depth(&self) -> usize {
        match self {
            Self::List(value)
            | Self::Reference(value)
            | Self::Remote(value)
            | Self::Future(value) => 1 + value.depth(),
            Self::Function {
                parameters, result, ..
            } => {
                1 + parameters
                    .iter()
                    .chain(std::iter::once(result.as_ref()))
                    .map(Self::depth)
                    .max()
                    .unwrap_or(0)
            }
            Self::Record { arguments, .. }
            | Self::Variant { arguments, .. }
            | Self::Union(arguments) => 1 + arguments.iter().map(Self::depth).max().unwrap_or(0),
            _ => 1,
        }
    }

    pub(crate) fn contains_generic(&self) -> bool {
        match self {
            Self::Generic(_) => true,
            Self::List(value)
            | Self::Reference(value)
            | Self::Remote(value)
            | Self::Future(value) => value.contains_generic(),
            Self::Function {
                parameters, result, ..
            } => parameters.iter().any(Self::contains_generic) || result.contains_generic(),
            Self::Record { arguments, .. }
            | Self::Variant { arguments, .. }
            | Self::Union(arguments) => arguments.iter().any(Self::contains_generic),
            _ => false,
        }
    }

    /// Replace generic leaves using a named substitution map.
    pub(crate) fn substitute(&self, substitutions: &HashMap<String, VerificationType>) -> Self {
        self.substitute_with(&|name| substitutions.get(name).cloned())
    }

    /// Replace generic leaves using the stable, sorted specialization carried by bytecode calls.
    pub(crate) fn specialize(&self, substitutions: &Specialization) -> Self {
        self.substitute_with(&|name| {
            substitutions
                .binary_search_by(|(candidate, _)| candidate.as_str().cmp(name))
                .ok()
                .map(|index| substitutions[index].1.clone())
        })
    }

    fn substitute_with(&self, lookup: &impl Fn(&str) -> Option<Self>) -> Self {
        match self {
            Self::Generic(name) => lookup(name).unwrap_or_else(|| self.clone()),
            Self::List(value) => Self::List(Box::new(value.substitute_with(lookup))),
            Self::Reference(value) => Self::Reference(Box::new(value.substitute_with(lookup))),
            Self::Remote(value) => Self::Remote(Box::new(value.substitute_with(lookup))),
            Self::Future(value) => Self::Future(Box::new(value.substitute_with(lookup))),
            Self::Function {
                parameters,
                parameter_modes,
                result,
            } => Self::Function {
                parameters: parameters
                    .iter()
                    .map(|ty| ty.substitute_with(lookup))
                    .collect(),
                parameter_modes: parameter_modes.clone(),
                result: Box::new(result.substitute_with(lookup)),
            },
            Self::Record { record, arguments } => Self::Record {
                record: *record,
                arguments: arguments
                    .iter()
                    .map(|ty| ty.substitute_with(lookup))
                    .collect(),
            },
            Self::Variant { variant, arguments } => Self::Variant {
                variant: *variant,
                arguments: arguments
                    .iter()
                    .map(|ty| ty.substitute_with(lookup))
                    .collect(),
            },
            Self::Union(members) => Self::Union(
                members
                    .iter()
                    .map(|ty| ty.substitute_with(lookup))
                    .collect(),
            ),
            _ => self.clone(),
        }
    }
}

#[cfg(test)]
mod verification_type_tests {
    use super::*;

    #[test]
    fn substitutions_walk_nested_types_consistently() {
        let generic = VerificationType::Function {
            parameters: vec![VerificationType::List(Box::new(VerificationType::Generic(
                "T".into(),
            )))],
            parameter_modes: vec![crate::ast::ParameterMode::Borrow],
            result: Box::new(VerificationType::Reference(Box::new(
                VerificationType::Generic("T".into()),
            ))),
        };
        let expected = VerificationType::Function {
            parameters: vec![VerificationType::List(Box::new(VerificationType::Integer))],
            parameter_modes: vec![crate::ast::ParameterMode::Borrow],
            result: Box::new(VerificationType::Reference(Box::new(
                VerificationType::Integer,
            ))),
        };
        let map = HashMap::from([("T".into(), VerificationType::Integer)]);
        let specialization = vec![("T".into(), VerificationType::Integer)];

        assert_eq!(generic.substitute(&map), expected);
        assert_eq!(generic.specialize(&specialization), expected);
        assert_eq!(generic.depth(), 3);
        assert!(generic.contains_generic());
        assert!(!expected.contains_generic());
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Constant {
    Unit,
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(String),
    CodePoint(char),
    Symbol(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Instruction {
    /// Releases this frame's ownership of an inline register or promoted slot.
    ///
    /// A promoted register detaches rather than writing through its slot because
    /// reference captures may still own and observe that slot.
    Drop {
        register: Register,
    },
    LoadConstant {
        destination: Register,
        constant: u16,
    },
    Move {
        destination: Register,
        source: Register,
    },
    Unary {
        destination: Register,
        operator: UnaryOp,
        operand: Register,
    },
    Binary {
        destination: Register,
        operator: BinaryOp,
        left: Register,
        right: Register,
    },
    MakeList {
        destination: Register,
        element_type: VerificationType,
        elements: Vec<Register>,
    },
    Index {
        destination: Register,
        object: Register,
        index: Register,
    },
    MakeRecord {
        destination: Register,
        record: RecordId,
        type_arguments: Vec<VerificationType>,
        fields: Vec<(String, Register)>,
    },
    MakeVariant {
        destination: Register,
        variant: VariantId,
        type_arguments: Vec<VerificationType>,
        payload: Vec<Register>,
    },
    LoadField {
        destination: Register,
        object: Register,
        field: String,
        by_reference: bool,
    },
    StoreField {
        object: Register,
        field: String,
        source: Register,
    },
    StoreIndex {
        object: Register,
        index: Register,
        source: Register,
    },
    MakeReference {
        destination: Register,
        pointee_type: VerificationType,
        object: Register,
        index: Register,
    },
    MakeWholeReference {
        destination: Register,
        pointee_type: VerificationType,
        object: Register,
    },
    MakeFieldReference {
        destination: Register,
        pointee_type: VerificationType,
        object: Register,
        field: String,
    },
    MoveOut {
        destination: Register,
        source: Register,
    },
    Push {
        destination: Register,
        object: Register,
        value: Register,
    },
    Append {
        destination: Register,
        object: Register,
        value: Register,
    },
    Contains {
        destination: Register,
        value: Register,
        candidates: Vec<Register>,
    },
    Builtin {
        destination: Register,
        builtin: Builtin,
        arguments: Vec<Register>,
    },
    SpawnRemote {
        destination: Register,
        value: Register,
    },
    SpawnRemoteBorrow {
        destination: Register,
        source: Register,
    },
    RemoteCall {
        destination: Register,
        remote: Register,
        function: FunctionId,
        arguments: Vec<(crate::ast::ParameterMode, Register)>,
    },
    Await {
        destination: Register,
        future: Register,
    },
    MatchPattern {
        destination: Register,
        subject: Register,
        pattern: crate::hir::Pattern,
        bindings: Vec<Register>,
    },
    Jump {
        target: usize,
    },
    JumpIfFalse {
        condition: Register,
        target: usize,
    },
    Assert {
        condition: Register,
        message: Option<Register>,
    },
    Call {
        destination: Register,
        function: FunctionId,
        specialization: Specialization,
        arguments: Vec<Register>,
    },
    CallMethod {
        destination: Register,
        receiver: Register,
        function: FunctionId,
        specialization: Specialization,
        arguments: Vec<Register>,
    },
    CallContractMethod {
        destination: Register,
        receiver: Register,
        slot: DispatchSlot,
        name: String,
        arguments: Vec<Register>,
    },
    MakeClosure {
        destination: Register,
        function: FunctionId,
        specialization: Specialization,
        captures: Vec<(crate::hir::CaptureMode, Register)>,
    },
    CallValue {
        destination: Register,
        callee: Register,
        arguments: Vec<Register>,
    },
    CallClosure {
        destination: Register,
        function: FunctionId,
        specialization: Specialization,
        captures: Vec<(crate::hir::CaptureMode, Register)>,
        arguments: Vec<Register>,
    },
    Return {
        source: Register,
    },
}

impl Instruction {
    pub(crate) fn visit_registers(&self, mut visit: impl FnMut(Register)) {
        match self {
            Self::Drop { register } => visit(*register),
            Self::LoadConstant { destination, .. } => visit(*destination),
            Self::Move {
                destination,
                source,
            } => {
                visit(*destination);
                visit(*source);
            }
            Self::Unary {
                destination,
                operand,
                ..
            } => {
                visit(*destination);
                visit(*operand);
            }
            Self::Binary {
                destination,
                left,
                right,
                ..
            } => {
                visit(*destination);
                visit(*left);
                visit(*right);
            }
            Self::MakeList {
                destination,
                elements,
                ..
            } => {
                visit(*destination);
                elements.iter().copied().for_each(&mut visit);
            }
            Self::Index {
                destination,
                object,
                index,
            } => {
                visit(*destination);
                visit(*object);
                visit(*index);
            }
            Self::MakeRecord {
                destination,
                fields,
                ..
            } => {
                visit(*destination);
                fields.iter().for_each(|(_, register)| visit(*register));
            }
            Self::MakeVariant {
                destination,
                payload,
                ..
            } => {
                visit(*destination);
                payload.iter().copied().for_each(&mut visit);
            }
            Self::LoadField {
                destination,
                object,
                ..
            } => {
                visit(*destination);
                visit(*object);
            }
            Self::StoreField { object, source, .. } => {
                visit(*object);
                visit(*source);
            }
            Self::StoreIndex {
                object,
                index,
                source,
            } => {
                visit(*object);
                visit(*index);
                visit(*source);
            }
            Self::MakeReference {
                destination,
                object,
                index,
                ..
            } => {
                visit(*destination);
                visit(*object);
                visit(*index);
            }
            Self::MakeWholeReference {
                destination,
                object,
                ..
            } => {
                visit(*destination);
                visit(*object);
            }
            Self::MakeFieldReference {
                destination,
                object,
                ..
            } => {
                visit(*destination);
                visit(*object);
            }
            Self::MoveOut {
                destination,
                source,
            } => {
                visit(*destination);
                visit(*source);
            }
            Self::Push {
                destination,
                object,
                value,
            }
            | Self::Append {
                destination,
                object,
                value,
            } => {
                visit(*destination);
                visit(*object);
                visit(*value);
            }
            Self::Contains {
                destination,
                value,
                candidates,
            } => {
                visit(*destination);
                visit(*value);
                candidates.iter().copied().for_each(&mut visit);
            }
            Self::Builtin {
                destination,
                arguments,
                ..
            } => {
                visit(*destination);
                arguments.iter().copied().for_each(&mut visit);
            }
            Self::SpawnRemote { destination, value } => {
                visit(*destination);
                visit(*value);
            }
            Self::SpawnRemoteBorrow {
                destination,
                source,
            } => {
                visit(*destination);
                visit(*source);
            }
            Self::RemoteCall {
                destination,
                remote,
                arguments,
                ..
            } => {
                visit(*destination);
                visit(*remote);
                arguments.iter().for_each(|(_, register)| visit(*register));
            }
            Self::Await {
                destination,
                future,
            } => {
                visit(*destination);
                visit(*future);
            }
            Self::MatchPattern {
                destination,
                subject,
                bindings,
                ..
            } => {
                visit(*destination);
                visit(*subject);
                bindings.iter().copied().for_each(&mut visit);
            }
            Self::Jump { .. } => {}
            Self::JumpIfFalse { condition, .. } => visit(*condition),
            Self::Assert { condition, message } => {
                visit(*condition);
                message.iter().copied().for_each(&mut visit);
            }
            Self::Call {
                destination,
                arguments,
                ..
            } => {
                visit(*destination);
                arguments.iter().copied().for_each(visit);
            }
            Self::CallMethod {
                destination,
                receiver,
                arguments,
                ..
            }
            | Self::CallContractMethod {
                destination,
                receiver,
                arguments,
                ..
            } => {
                visit(*destination);
                visit(*receiver);
                arguments.iter().copied().for_each(&mut visit);
            }
            Self::MakeClosure {
                destination,
                captures,
                ..
            } => {
                visit(*destination);
                captures.iter().for_each(|(_, register)| visit(*register));
            }
            Self::CallValue {
                destination,
                callee,
                arguments,
            } => {
                visit(*destination);
                visit(*callee);
                arguments.iter().copied().for_each(&mut visit);
            }
            Self::CallClosure {
                destination,
                captures,
                arguments,
                ..
            } => {
                visit(*destination);
                captures.iter().for_each(|(_, register)| visit(*register));
                arguments.iter().copied().for_each(&mut visit);
            }
            Self::Return { source } => visit(*source),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BytecodeFunction {
    pub name: String,
    /// A source intrinsic declaration whose executable call sites lower to `Builtin`.
    pub intrinsic_stub: bool,
    pub parameters: u16,
    pub parameter_types: Vec<VerificationType>,
    pub parameter_modes: Vec<crate::ast::ParameterMode>,
    pub mutable_parameters: Vec<bool>,
    /// Whether `Return` transfers a live place handle instead of reading its current value.
    pub returns_reference: bool,
    pub captures: u16,
    pub capture_types: Vec<VerificationType>,
    pub result_type: VerificationType,
    pub registers: u16,
    pub instructions: Vec<Instruction>,
    pub instruction_spans: Vec<Range<usize>>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Program {
    pub constants: Vec<Constant>,
    pub functions: HashMap<FunctionId, BytecodeFunction>,
    pub main: Option<FunctionId>,
    /// Whether `main` receives one `std.process.Arguments` value.
    pub main_arguments: bool,
    pub string_record: Option<RecordId>,
    pub symbol_record: Option<RecordId>,
    pub records: HashMap<RecordId, RuntimeRecord>,
    pub dispatch: HashMap<(NominalTypeId, DispatchSlot), FunctionId>,
    pub variants: HashMap<VariantId, RuntimeVariant>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeRecord {
    pub name: String,
    /// Generic parameters in declaration order.
    pub parameters: Vec<String>,
    pub layout: Arc<super::value::RecordLayout>,
    /// Declared field types in the same canonical order as `layout`.
    pub field_types: Vec<VerificationType>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeVariant {
    pub parent: VariantTypeId,
    pub type_name: Arc<str>,
    /// Generic parameters of the parent enum in declaration order.
    pub parameters: Vec<String>,
    pub alternative: Arc<str>,
    /// Enum cases currently have zero or one declared payload value.
    pub payload: Vec<VerificationType>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProgramMetrics {
    pub functions: usize,
    pub instructions: usize,
    pub registers: usize,
    pub constants: usize,
}

impl Program {
    pub fn metrics(&self) -> ProgramMetrics {
        ProgramMetrics {
            functions: self.functions.len(),
            instructions: self
                .functions
                .values()
                .map(|function| function.instructions.len())
                .sum(),
            registers: self
                .functions
                .values()
                .map(|function| usize::from(function.registers))
                .sum(),
            constants: self.constants.len(),
        }
    }
}
