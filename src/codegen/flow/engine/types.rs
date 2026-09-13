//! Logical type compatibility, joins, projections, and scalar/intrinsic rules.
use super::FunctionSchema;
use super::{Body, Program};
use crate::ast::{BinaryOp, UnaryOp};
use crate::codegen::{metadata::Constant, types::ExecutableType};
use crate::error::FosterError;
use crate::hir::VariantId;
use crate::intrinsics::IntrinsicType;
use std::collections::HashMap;
pub(super) fn verification_field_type(
    program: &Program,
    receiver: &ExecutableType,
    field: &str,
) -> Option<ExecutableType> {
    match receiver {
        ExecutableType::Reference(pointee) => verification_field_type(program, pointee, field),
        ExecutableType::Record { record, arguments } => {
            let metadata = program.metadata.records.get(record)?;
            let index = metadata
                .layout()
                .names()
                .iter()
                .position(|name| name == field)?;
            let substitutions = metadata
                .parameters
                .iter()
                .cloned()
                .zip(arguments.iter().cloned())
                .collect::<HashMap<_, _>>();
            Some(metadata.fields().get(index)?.ty.substitute(&substitutions))
        }
        ExecutableType::List(element) => match field {
            "empty?" => Some(ExecutableType::Bool),
            "length" => Some(ExecutableType::Integer),
            "head" => Some((**element).clone()),
            "rest" => Some(receiver.clone()),
            _ => None,
        },
        ExecutableType::Unknown => Some(ExecutableType::Unknown),
        _ => None,
    }
}

pub(super) fn merge_types(
    _function: &Body,
    _index: usize,
    left: &ExecutableType,
    right: &ExecutableType,
) -> Result<ExecutableType, FosterError> {
    if left == right {
        return Ok(left.clone());
    }
    if matches!(left, ExecutableType::Unknown) || matches!(right, ExecutableType::Unknown) {
        return Ok(ExecutableType::Unknown);
    }
    match (left, right) {
        (ExecutableType::List(left), ExecutableType::List(right)) => Ok(ExecutableType::List(
            Box::new(merge_types(_function, _index, left, right)?),
        )),
        (ExecutableType::Reference(left), ExecutableType::Reference(right)) => Ok(
            ExecutableType::Reference(Box::new(merge_types(_function, _index, left, right)?)),
        ),
        (ExecutableType::Remote(left), ExecutableType::Remote(right)) => Ok(
            ExecutableType::Remote(Box::new(merge_types(_function, _index, left, right)?)),
        ),
        (ExecutableType::Future(left), ExecutableType::Future(right)) => Ok(
            ExecutableType::Future(Box::new(merge_types(_function, _index, left, right)?)),
        ),
        _ => Ok(ExecutableType::alternatives(vec![
            left.clone(),
            right.clone(),
        ])),
    }
}

pub(super) fn require_type(
    function: &Body,
    index: usize,
    found: &ExecutableType,
    expected: &ExecutableType,
    role: &str,
) -> Result<(), FosterError> {
    if compatible(found, expected) {
        Ok(())
    } else {
        Err(FosterError::runtime(format!(
            "bytecode function `{}` instruction {index} has {role} type {found:?}, expected {expected:?}",
            function.name
        )))
    }
}

pub(crate) fn compatible(found: &ExecutableType, expected: &ExecutableType) -> bool {
    if found == expected
        || matches!(found, ExecutableType::Unknown | ExecutableType::Generic(_))
        || matches!(
            expected,
            ExecutableType::Unknown | ExecutableType::Generic(_)
        )
    {
        return true;
    }
    match (found, expected) {
        (ExecutableType::Alternatives(found), expected) => {
            found.iter().all(|found| compatible(found, expected))
        }
        (found, ExecutableType::Alternatives(expected)) => {
            expected.iter().any(|expected| compatible(found, expected))
        }
        (ExecutableType::CodePoint | ExecutableType::Byte, ExecutableType::Integer) => true,
        // Structural record and variant conformance is resolved before bytecode lowering. The
        // verification vocabulary retains runtime representation, not the source contract proof.
        (ExecutableType::Record { .. }, ExecutableType::Record { .. })
        | (ExecutableType::Variant { .. }, ExecutableType::Variant { .. }) => true,
        (ExecutableType::List(found), ExecutableType::List(expected))
        | (ExecutableType::Reference(found), ExecutableType::Reference(expected))
        | (ExecutableType::Remote(found), ExecutableType::Remote(expected))
        | (ExecutableType::Future(found), ExecutableType::Future(expected)) => {
            compatible(found, expected)
        }
        (
            ExecutableType::Function {
                parameters: found_parameters,
                result: found_result,
            },
            ExecutableType::Function {
                parameters: expected_parameters,
                result: expected_result,
            },
        ) => {
            // Callable representation erasure can adapt source ownership modes while the
            // closure still carries its concrete callee modes for execution.
            found_parameters.len() == expected_parameters.len()
                && found_parameters
                    .iter()
                    .zip(expected_parameters)
                    .all(|(found, expected)| compatible(&found.ty, &expected.ty))
                && compatible(found_result, expected_result)
        }
        _ => false,
    }
}

pub(super) fn unary_type(
    function: &Body,
    index: usize,
    operator: UnaryOp,
    operand: ExecutableType,
) -> Result<ExecutableType, FosterError> {
    Ok(match (operator, operand) {
        (UnaryOp::Negate, ExecutableType::Float) => ExecutableType::Float,
        (
            UnaryOp::Negate,
            ExecutableType::Integer | ExecutableType::CodePoint | ExecutableType::Byte,
        ) => ExecutableType::Integer,
        (UnaryOp::Not, ExecutableType::Bool) => ExecutableType::Bool,
        (UnaryOp::BitNot, ExecutableType::Byte) => ExecutableType::Byte,
        (_, ExecutableType::Unknown) => ExecutableType::Unknown,
        (_, found) => return type_error(function, index, "valid unary operand", &found),
    })
}

pub(super) fn binary_type(
    function: &Body,
    index: usize,
    operator: BinaryOp,
    left: ExecutableType,
    right: ExecutableType,
) -> Result<ExecutableType, FosterError> {
    use BinaryOp::*;
    if matches!(operator, Equal | NotEqual) {
        if !(is_integer_like(&left) && is_integer_like(&right)) {
            require_type(function, index, &left, &right, "binary operand")?;
        }
        return Ok(ExecutableType::Bool);
    }
    if left == ExecutableType::Unknown || right == ExecutableType::Unknown {
        return Ok(ExecutableType::Unknown);
    }
    if matches!(operator, BitAnd | BitOr | BitXor)
        && left == ExecutableType::Byte
        && right == ExecutableType::Byte
    {
        return Ok(ExecutableType::Byte);
    }
    if matches!(operator, ShiftLeft | ShiftRight)
        && left == ExecutableType::Byte
        && right == ExecutableType::Integer
    {
        return Ok(ExecutableType::Byte);
    }
    if is_integer_like(&left) && is_integer_like(&right) {
        return Ok(
            if matches!(operator, Less | LessEqual | Greater | GreaterEqual) {
                ExecutableType::Bool
            } else if matches!(operator, Add | Subtract | Multiply | Divide) {
                ExecutableType::Integer
            } else {
                return type_error(function, index, "valid integer operation", &left);
            },
        );
    }
    if left == ExecutableType::Float && right == ExecutableType::Float {
        return Ok(
            if matches!(operator, Less | LessEqual | Greater | GreaterEqual) {
                ExecutableType::Bool
            } else if matches!(operator, Add | Subtract | Multiply | Divide) {
                ExecutableType::Float
            } else {
                return type_error(function, index, "valid float operation", &left);
            },
        );
    }
    if operator == Add && left == right {
        return Ok(left);
    }
    Err(FosterError::runtime(format!(
        "bytecode function `{}` instruction {index} applies {operator:?} to incompatible types {left:?} and {right:?}",
        function.name
    )))
}

pub(super) fn is_integer_like(ty: &ExecutableType) -> bool {
    matches!(
        ty,
        ExecutableType::Integer | ExecutableType::CodePoint | ExecutableType::Byte
    )
}

pub(super) fn nominal_record(record: crate::hir::RecordId) -> ExecutableType {
    ExecutableType::Record {
        record,
        arguments: Vec::new(),
    }
}

pub(super) fn constant_type(program: &Program, constant: &Constant) -> ExecutableType {
    match constant {
        Constant::Unit => ExecutableType::Unit,
        Constant::Bool(_) => ExecutableType::Bool,
        Constant::Integer(_) => ExecutableType::Integer,
        Constant::Float(_) => ExecutableType::Float,
        Constant::String(_) => program
            .metadata
            .string_record
            .map(nominal_record)
            .unwrap_or(ExecutableType::Unknown),
        Constant::CodePoint(_) => ExecutableType::CodePoint,
        Constant::Symbol(_) => program
            .metadata
            .symbol_record
            .map(nominal_record)
            .unwrap_or(ExecutableType::Unknown),
    }
}

pub(super) fn record_type(program: &Program, record: crate::hir::RecordId) -> ExecutableType {
    match Some(record) {
        id if id == program.metadata.list_record => {
            ExecutableType::List(Box::new(ExecutableType::Unknown))
        }
        id if id == program.metadata.bytes_record => ExecutableType::Bytes,
        _ => nominal_record(record),
    }
}

pub(super) fn is_foster_byte_buffer(program: &Program, ty: &ExecutableType) -> bool {
    let ExecutableType::Record { record, .. } = ty else {
        return false;
    };
    Some(*record) == program.metadata.byte_buffer_record
}

pub(super) fn indexed_element_type(
    program: &Program,
    ty: &ExecutableType,
) -> Option<ExecutableType> {
    match ty {
        ExecutableType::Reference(pointee) => indexed_element_type(program, pointee),
        _ if is_foster_byte_buffer(program, ty) => Some(ExecutableType::Byte),
        _ => ty.indexed_element(),
    }
}

pub(super) fn callable_type(function: &FunctionSchema) -> ExecutableType {
    ExecutableType::Function {
        parameters: function.parameters.clone(),
        result: Box::new(function.result_type.clone()),
    }
}

pub(super) fn intrinsic_verification_type(program: &Program, ty: IntrinsicType) -> ExecutableType {
    match ty {
        IntrinsicType::Any => ExecutableType::Unknown,
        IntrinsicType::Unit => ExecutableType::Unit,
        IntrinsicType::Bool => ExecutableType::Bool,
        IntrinsicType::Integer => ExecutableType::Integer,
        IntrinsicType::Float => ExecutableType::Float,
        IntrinsicType::CodePoint => ExecutableType::CodePoint,
        IntrinsicType::Byte => ExecutableType::Byte,
        IntrinsicType::Bytes => ExecutableType::Bytes,
        IntrinsicType::ByteBuffer => ExecutableType::ByteBuffer,
        IntrinsicType::String => program
            .metadata
            .string_record
            .map(nominal_record)
            .unwrap_or(ExecutableType::Unknown),
        IntrinsicType::ListByte => ExecutableType::List(Box::new(ExecutableType::Byte)),
    }
}

pub(super) fn pattern_irrefutable(pattern: &crate::hir::Pattern) -> bool {
    matches!(
        pattern.unspanned(),
        crate::hir::Pattern::Wildcard | crate::hir::Pattern::Binding(_)
    )
}

pub(super) fn fully_covered_variant(pattern: &crate::hir::Pattern) -> Option<VariantId> {
    let crate::hir::Pattern::Variant { variant, fields } = pattern.unspanned() else {
        return None;
    };
    fields.iter().all(pattern_irrefutable).then_some(*variant)
}

pub(super) fn invalid_instruction<T>(
    function: &Body,
    index: usize,
    message: impl std::fmt::Display,
) -> Result<T, FosterError> {
    Err(FosterError::runtime(format!(
        "bytecode function `{}` instruction {index} {message}",
        function.name
    )))
}

pub(super) fn type_error<T>(
    function: &Body,
    index: usize,
    expected: &str,
    found: &ExecutableType,
) -> Result<T, FosterError> {
    invalid_instruction(
        function,
        index,
        format!("requires {expected}, found {found:?}"),
    )
}
