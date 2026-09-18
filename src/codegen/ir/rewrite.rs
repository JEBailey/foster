//! Value rewriting shared by graph transforms.
use super::{Instruction, PortableInstruction, Terminator, Value};
impl Instruction {
    pub(crate) fn rewrite_values(&mut self, mut rewrite: impl FnMut(Value) -> Value) {
        match self {
            Self::Constant { destination, .. } => {
                *destination = rewrite(*destination);
            }
            Self::Unary {
                destination,
                operand,
                ..
            } => {
                *destination = rewrite(*destination);
                *operand = rewrite(*operand);
            }
            Self::IntegerExtend {
                destination,
                operand,
                ..
            } => {
                *destination = rewrite(*destination);
                *operand = rewrite(*operand);
            }
            Self::Binary {
                destination,
                left,
                right,
                ..
            } => {
                *destination = rewrite(*destination);
                *left = rewrite(*left);
                *right = rewrite(*right);
            }
            Self::Call {
                destination,
                arguments,
                ..
            } => {
                *destination = rewrite(*destination);
                for value in arguments {
                    *value = rewrite(*value);
                }
            }
            Self::RuntimeCall {
                destination,
                arguments,
                ..
            } => {
                *destination = rewrite(*destination);
                for value in arguments {
                    *value = rewrite(*value);
                }
            }
            Self::WrapCallable {
                destination,
                source,
                ..
            } => {
                *destination = rewrite(*destination);
                *source = rewrite(*source);
            }
            Self::StringToBytes {
                destination,
                source,
                ..
            } => {
                *destination = rewrite(*destination);
                *source = rewrite(*source);
            }
            Self::BoxValue {
                destination,
                source,
                ..
            } => {
                *destination = rewrite(*destination);
                *source = rewrite(*source);
            }
            Self::UnboxValue {
                destination,
                source,
                ..
            } => {
                *destination = rewrite(*destination);
                *source = rewrite(*source);
            }
            Self::ConvertResultError {
                destination,
                source,
                ..
            } => {
                *destination = rewrite(*destination);
                *source = rewrite(*source);
            }
            Self::Assert {
                condition, message, ..
            } => {
                *condition = rewrite(*condition);
                if let Some(value) = message {
                    *value = rewrite(*value);
                }
            }
            Self::Portable(instruction) => instruction.rewrite_values(rewrite),
        }
    }
}
impl PortableInstruction {
    pub(crate) fn rewrite_values(&mut self, mut rewrite: impl FnMut(Value) -> Value) {
        match self {
            Self::Drop { value, .. } => {
                *value = rewrite(*value);
            }
            Self::LoadConstant { destination, .. } => {
                *destination = rewrite(*destination);
            }
            Self::Move {
                destination,
                source,
                ..
            } => {
                *destination = rewrite(*destination);
                *source = rewrite(*source);
            }
            Self::CopyOnWrite {
                destination,
                source,
                ..
            } => {
                *destination = rewrite(*destination);
                *source = rewrite(*source);
            }
            Self::Unary {
                destination,
                operand,
                ..
            } => {
                *destination = rewrite(*destination);
                *operand = rewrite(*operand);
            }
            Self::Binary {
                destination,
                left,
                right,
                ..
            } => {
                *destination = rewrite(*destination);
                *left = rewrite(*left);
                *right = rewrite(*right);
            }
            Self::MakeList {
                destination,
                elements,
                ..
            } => {
                *destination = rewrite(*destination);
                for value in elements {
                    *value = rewrite(*value);
                }
            }
            Self::Index {
                destination,
                object,
                index,
                ..
            } => {
                *destination = rewrite(*destination);
                *object = rewrite(*object);
                *index = rewrite(*index);
            }
            Self::MakeRecord {
                destination,
                fields,
                ..
            } => {
                *destination = rewrite(*destination);
                for (_, value) in fields {
                    *value = rewrite(*value);
                }
            }
            Self::MakeVariant {
                destination,
                payload,
                ..
            } => {
                *destination = rewrite(*destination);
                for value in payload {
                    *value = rewrite(*value);
                }
            }
            Self::LoadField {
                destination,
                object,
                ..
            } => {
                *destination = rewrite(*destination);
                *object = rewrite(*object);
            }
            Self::StoreField { object, source, .. } => {
                *object = rewrite(*object);
                *source = rewrite(*source);
            }
            Self::StoreIndex {
                object,
                index,
                source,
                ..
            } => {
                *object = rewrite(*object);
                *index = rewrite(*index);
                *source = rewrite(*source);
            }
            Self::MakeReference {
                destination,
                object,
                index,
                ..
            } => {
                *destination = rewrite(*destination);
                *object = rewrite(*object);
                *index = rewrite(*index);
            }
            Self::MakeWholeReference {
                destination,
                object,
                ..
            } => {
                *destination = rewrite(*destination);
                *object = rewrite(*object);
            }
            Self::MakeFieldReference {
                destination,
                object,
                ..
            } => {
                *destination = rewrite(*destination);
                *object = rewrite(*object);
            }
            Self::MoveOut {
                destination,
                source,
                ..
            } => {
                *destination = rewrite(*destination);
                *source = rewrite(*source);
            }
            Self::Push {
                destination,
                object,
                value,
                ..
            } => {
                *destination = rewrite(*destination);
                *object = rewrite(*object);
                *value = rewrite(*value);
            }
            Self::Append {
                destination,
                object,
                value,
                ..
            } => {
                *destination = rewrite(*destination);
                *object = rewrite(*object);
                *value = rewrite(*value);
            }
            Self::Contains {
                destination,
                value,
                candidates,
                ..
            } => {
                *destination = rewrite(*destination);
                *value = rewrite(*value);
                for value in candidates {
                    *value = rewrite(*value);
                }
            }
            Self::Builtin {
                destination,
                arguments,
                ..
            } => {
                *destination = rewrite(*destination);
                for value in arguments {
                    *value = rewrite(*value);
                }
            }
            Self::SpawnRemote {
                destination, value, ..
            } => {
                *destination = rewrite(*destination);
                *value = rewrite(*value);
            }
            Self::SpawnRemoteBorrow {
                destination,
                source,
                ..
            } => {
                *destination = rewrite(*destination);
                *source = rewrite(*source);
            }
            Self::RemoteCall {
                destination,
                remote,
                arguments,
                ..
            } => {
                *destination = rewrite(*destination);
                *remote = rewrite(*remote);
                for (_, value) in arguments {
                    *value = rewrite(*value);
                }
            }
            Self::Await {
                destination,
                future,
                ..
            } => {
                *destination = rewrite(*destination);
                *future = rewrite(*future);
            }
            Self::MatchPattern {
                destination,
                subject,
                bindings,
                ..
            } => {
                *destination = rewrite(*destination);
                *subject = rewrite(*subject);
                for value in bindings {
                    *value = rewrite(*value);
                }
            }
            Self::Assert {
                condition, message, ..
            } => {
                *condition = rewrite(*condition);
                if let Some(value) = message {
                    *value = rewrite(*value);
                }
            }
            Self::Call {
                destination,
                arguments,
                ..
            } => {
                *destination = rewrite(*destination);
                for value in arguments {
                    *value = rewrite(*value);
                }
            }
            Self::CallMethod {
                destination,
                receiver,
                arguments,
                ..
            } => {
                *destination = rewrite(*destination);
                *receiver = rewrite(*receiver);
                for value in arguments {
                    *value = rewrite(*value);
                }
            }
            Self::CallContractMethod {
                destination,
                receiver,
                arguments,
                ..
            } => {
                *destination = rewrite(*destination);
                *receiver = rewrite(*receiver);
                for value in arguments {
                    *value = rewrite(*value);
                }
            }
            Self::MakeClosure {
                destination,
                captures,
                ..
            } => {
                *destination = rewrite(*destination);
                for (_, value) in captures {
                    *value = rewrite(*value);
                }
            }
            Self::CallValue {
                destination,
                callee,
                arguments,
                ..
            } => {
                *destination = rewrite(*destination);
                *callee = rewrite(*callee);
                for value in arguments {
                    *value = rewrite(*value);
                }
            }
            Self::CallClosure {
                destination,
                captures,
                arguments,
                ..
            } => {
                *destination = rewrite(*destination);
                for (_, value) in captures {
                    *value = rewrite(*value);
                }
                for value in arguments {
                    *value = rewrite(*value);
                }
            }
        }
    }
}
impl Terminator {
    pub(crate) fn rewrite_values(&mut self, mut rewrite: impl FnMut(Value) -> Value) {
        match self {
            Self::Jump { arguments, .. } => {
                for value in arguments {
                    *value = rewrite(*value);
                }
            }
            Self::Branch {
                condition,
                then_arguments,
                else_arguments,
                ..
            } => {
                *condition = rewrite(*condition);
                for value in then_arguments {
                    *value = rewrite(*value);
                }
                for value in else_arguments {
                    *value = rewrite(*value);
                }
            }
            Self::Unreachable => {}
            Self::Return(value) => *value = rewrite(*value),
        }
    }
}
