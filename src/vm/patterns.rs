use super::Value;
use crate::hir::Pattern;

use super::Program;

pub(super) fn matches(
    program: &Program,
    pattern: &Pattern,
    value: &Value,
    bindings: &mut Vec<Value>,
) -> bool {
    match (pattern.unspanned(), value) {
        (Pattern::Wildcard, _) => true,
        (Pattern::Binding(_), value) => {
            bindings.push(value.clone());
            true
        }
        (Pattern::Bool(expected), Value::Bool(actual)) => expected == actual,
        (Pattern::Integer(expected), Value::Integer(actual)) => expected == actual,
        (Pattern::Float(expected), Value::Float(actual)) => expected == actual,
        (Pattern::String(expected), actual) => actual
            .string_bytes()
            .is_some_and(|bytes| bytes == expected.as_bytes()),
        (Pattern::CodePoint(expected), Value::CodePoint(actual)) => expected.starts_with(*actual),
        (Pattern::Symbol(expected), Value::Symbol(actual)) => expected == actual,
        (
            Pattern::Variant { variant, fields },
            Value::Variant {
                type_name,
                alternative,
                payload,
            },
        ) => {
            let (expected_type, expected_alternative) = &program.variants[variant];
            if type_name != expected_type
                || alternative != expected_alternative
                || fields.len() != payload.len()
            {
                return false;
            }
            let checkpoint = bindings.len();
            for (field, value) in fields.iter().zip(payload) {
                if !matches(program, field, value, bindings) {
                    bindings.truncate(checkpoint);
                    return false;
                }
            }
            true
        }
        _ => false,
    }
}
