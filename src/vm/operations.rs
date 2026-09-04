use super::Value;
use crate::ast::{BinaryOp, UnaryOp};
use crate::error::RuntimeError;

use super::Constant;

pub(super) fn constant_value(
    constant: &Constant,
    string_record: Option<crate::hir::RecordId>,
    symbol_record: Option<crate::hir::RecordId>,
) -> Value {
    match constant {
        Constant::Unit => Value::Unit,
        Constant::Bool(value) => Value::Bool(*value),
        Constant::Integer(value) => Value::Integer(*value),
        Constant::Float(value) => Value::Float(*value),
        Constant::String(value) => Value::string(string_record, value.as_bytes().to_vec()),
        Constant::CodePoint(value) => Value::CodePoint(*value),
        Constant::Symbol(value) => {
            Value::symbol(symbol_record, string_record, value.as_bytes().to_vec())
        }
    }
}

pub(super) fn unary(operator: UnaryOp, value: &Value) -> Result<Value, RuntimeError> {
    match (operator, value) {
        (UnaryOp::Negate, Value::Integer(value)) => checked_integer(value.checked_neg()),
        (UnaryOp::Negate, Value::CodePoint(value)) => Ok(Value::Integer(-(*value as i64))),
        (UnaryOp::Negate, Value::Byte(value)) => Ok(Value::Integer(-i64::from(*value))),
        (UnaryOp::Negate, Value::Float(value)) => Ok(Value::Float(-value)),
        (UnaryOp::Not, Value::Bool(value)) => Ok(Value::Bool(!value)),
        (UnaryOp::BitNot, Value::Byte(value)) => Ok(Value::Byte(!value)),
        _ => Err(RuntimeError::runtime(
            "invalid typed unary bytecode operation",
        )),
    }
}

pub(super) fn binary(
    operator: BinaryOp,
    left: &Value,
    right: &Value,
) -> Result<Value, RuntimeError> {
    use BinaryOp::*;
    if let (Value::Byte(left), Value::Byte(right)) = (left, right) {
        match operator {
            BitAnd => return Ok(Value::Byte(left & right)),
            BitOr => return Ok(Value::Byte(left | right)),
            BitXor => return Ok(Value::Byte(left ^ right)),
            _ => {}
        }
    }
    if let (Value::Byte(left), Value::Integer(right)) = (left, right)
        && matches!(operator, ShiftLeft | ShiftRight)
    {
        let shift = u32::try_from(*right)
            .ok()
            .filter(|shift| *shift < 8)
            .ok_or_else(|| RuntimeError::runtime("Byte shift must be between 0 and 7"))?;
        return Ok(Value::Byte(match operator {
            ShiftLeft => left << shift,
            ShiftRight => left >> shift,
            _ => unreachable!(),
        }));
    }
    if let (Some(left), Some(right)) = (integer_value(left), integer_value(right)) {
        return match operator {
            Add => checked_integer(left.checked_add(right)),
            Subtract => checked_integer(left.checked_sub(right)),
            Multiply => checked_integer(left.checked_mul(right)),
            Divide => checked_integer(left.checked_div(right)),
            BitAnd | BitOr | BitXor | ShiftLeft | ShiftRight => Err(RuntimeError::runtime(
                "bitwise operations require Byte operands",
            )),
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
        (Add, a, b) if a.string_bytes().is_some() && b.string_bytes().is_some() => {
            let mut value = a.string_bytes().unwrap().to_vec();
            value.extend_from_slice(b.string_bytes().unwrap());
            let record = match a {
                Value::Record { record, .. } => *record,
                _ => None,
            };
            Ok(Value::string(record, value))
        }
        (Equal, a, b) => Ok(Value::Bool(a == b)),
        (NotEqual, a, b) => Ok(Value::Bool(a != b)),
        (Less, Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a < b)),
        (LessEqual, Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a <= b)),
        (Greater, Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a > b)),
        (GreaterEqual, Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a >= b)),
        _ => Err(RuntimeError::runtime(
            "invalid typed binary bytecode operation",
        )),
    }
}

fn integer_value(value: &Value) -> Option<i64> {
    match value {
        Value::Integer(value) => Some(*value),
        Value::CodePoint(value) => Some(*value as i64),
        Value::Byte(value) => Some(i64::from(*value)),
        _ => None,
    }
}

fn checked_integer(value: Option<i64>) -> Result<Value, RuntimeError> {
    value
        .map(Value::Integer)
        .ok_or_else(|| RuntimeError::runtime("integer arithmetic overflow or division by zero"))
}
