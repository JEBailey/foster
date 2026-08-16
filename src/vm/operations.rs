use super::Value;
use crate::ast::{BinaryOp, UnaryOp};
use crate::error::FosterError;

use super::Constant;

pub(super) fn constant_value(constant: &Constant) -> Value {
    match constant {
        Constant::Unit => Value::Unit,
        Constant::Bool(value) => Value::Bool(*value),
        Constant::Integer(value) => Value::Integer(*value),
        Constant::Float(value) => Value::Float(*value),
        Constant::String(value) => Value::String(value.clone()),
        Constant::CodePoint(value) => Value::CodePoint(*value),
        Constant::Symbol(value) => Value::Symbol(value.clone()),
    }
}

pub(super) fn unary(operator: UnaryOp, value: &Value) -> Result<Value, FosterError> {
    match (operator, value) {
        (UnaryOp::Negate, Value::Integer(value)) => Ok(Value::Integer(-value)),
        (UnaryOp::Negate, Value::CodePoint(value)) => Ok(Value::Integer(-(*value as i64))),
        (UnaryOp::Negate, Value::Float(value)) => Ok(Value::Float(-value)),
        (UnaryOp::Not, Value::Bool(value)) => Ok(Value::Bool(!value)),
        _ => Err(FosterError::runtime(
            "invalid typed unary bytecode operation",
        )),
    }
}

pub(super) fn binary(
    operator: BinaryOp,
    left: &Value,
    right: &Value,
) -> Result<Value, FosterError> {
    use BinaryOp::*;
    if let (Some(left), Some(right)) = (integer_value(left), integer_value(right)) {
        return match operator {
            Add => checked_integer(left.checked_add(right)),
            Subtract => checked_integer(left.checked_sub(right)),
            Multiply => checked_integer(left.checked_mul(right)),
            Divide => checked_integer(left.checked_div(right)),
            Equal => Ok(Value::Bool(left == right)),
            NotEqual => Ok(Value::Bool(left != right)),
            Less => Ok(Value::Bool(left < right)),
            LessEqual => Ok(Value::Bool(left <= right)),
            Greater => Ok(Value::Bool(left > right)),
            GreaterEqual => Ok(Value::Bool(left >= right)),
        };
    }
    match (operator, left, right) {
        (Add, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
        (Subtract, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a - b)),
        (Multiply, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a * b)),
        (Divide, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a / b)),
        (Add, Value::String(a), Value::String(b)) => Ok(Value::String(a.clone() + b)),
        (Equal, a, b) => Ok(Value::Bool(a == b)),
        (NotEqual, a, b) => Ok(Value::Bool(a != b)),
        (Less, Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a < b)),
        (LessEqual, Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a <= b)),
        (Greater, Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a > b)),
        (GreaterEqual, Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a >= b)),
        _ => Err(FosterError::runtime(
            "invalid typed binary bytecode operation",
        )),
    }
}

fn integer_value(value: &Value) -> Option<i64> {
    match value {
        Value::Integer(value) => Some(*value),
        Value::CodePoint(value) => Some(*value as i64),
        _ => None,
    }
}

fn checked_integer(value: Option<i64>) -> Result<Value, FosterError> {
    value
        .map(Value::Integer)
        .ok_or_else(|| FosterError::runtime("integer arithmetic overflow or division by zero"))
}
