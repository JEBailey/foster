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
        Constant::Symbol(value) => Value::Symbol(value.clone()),
    }
}

pub(super) fn unary(operator: UnaryOp, value: &Value) -> Result<Value, FosterError> {
    match (operator, value) {
        (UnaryOp::Negate, Value::Integer(value)) => Ok(Value::Integer(-value)),
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
    match (operator, left, right) {
        (Add, Value::Integer(a), Value::Integer(b)) => checked_integer(a.checked_add(*b)),
        (Subtract, Value::Integer(a), Value::Integer(b)) => checked_integer(a.checked_sub(*b)),
        (Multiply, Value::Integer(a), Value::Integer(b)) => checked_integer(a.checked_mul(*b)),
        (Divide, Value::Integer(a), Value::Integer(b)) => checked_integer(a.checked_div(*b)),
        (Add, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
        (Subtract, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a - b)),
        (Multiply, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a * b)),
        (Divide, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a / b)),
        (Add, Value::String(a), Value::String(b)) => Ok(Value::String(a.clone() + b)),
        (Equal, a, b) => Ok(Value::Bool(a == b)),
        (NotEqual, a, b) => Ok(Value::Bool(a != b)),
        (Less, Value::Integer(a), Value::Integer(b)) => Ok(Value::Bool(a < b)),
        (LessEqual, Value::Integer(a), Value::Integer(b)) => Ok(Value::Bool(a <= b)),
        (Greater, Value::Integer(a), Value::Integer(b)) => Ok(Value::Bool(a > b)),
        (GreaterEqual, Value::Integer(a), Value::Integer(b)) => Ok(Value::Bool(a >= b)),
        (Less, Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a < b)),
        (LessEqual, Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a <= b)),
        (Greater, Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a > b)),
        (GreaterEqual, Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a >= b)),
        _ => Err(FosterError::runtime(
            "invalid typed binary bytecode operation",
        )),
    }
}

fn checked_integer(value: Option<i64>) -> Result<Value, FosterError> {
    value
        .map(Value::Integer)
        .ok_or_else(|| FosterError::runtime("integer arithmetic overflow or division by zero"))
}
