use super::Value;
use crate::hir::Pattern;

use super::Program;

pub(super) fn matches(
    program: &Program,
    pattern: &Pattern,
    value: &Value,
    bindings: &mut Vec<Value>,
) -> Result<bool, crate::error::RuntimeError> {
    if let Value::Reference(reference) = value {
        let actual = reference.read()?;
        let checkpoint = bindings.len();
        match (pattern.unspanned(), &actual) {
            (Pattern::Binding(_), _) => {
                bindings.push(value.clone());
                return Ok(true);
            }
            (Pattern::Record { fields: patterns }, Value::Record { fields, .. }) => {
                for (name, pattern) in patterns {
                    if !fields.contains_key(name)
                        || !matches(
                            program,
                            pattern,
                            &projected(reference.pattern_field(name))?,
                            bindings,
                        )?
                    {
                        bindings.truncate(checkpoint);
                        return Ok(false);
                    }
                }
                return Ok(true);
            }
            (
                Pattern::Variant { variant, fields },
                Value::Variant {
                    type_name,
                    alternative,
                    payload,
                    ..
                },
            ) => {
                let expected = &program.metadata.variants[variant];
                if type_name != &expected.type_name
                    || alternative != &expected.alternative
                    || fields.len() != payload.len()
                {
                    return Ok(false);
                }
                for (index, pattern) in fields.iter().enumerate() {
                    if !matches(
                        program,
                        pattern,
                        &projected(reference.payload(alternative, index))?,
                        bindings,
                    )? {
                        bindings.truncate(checkpoint);
                        return Ok(false);
                    }
                }
                return Ok(true);
            }
            _ => return matches(program, pattern, &actual, bindings),
        }
    }
    Ok(match (pattern.unspanned(), value) {
        (
            Pattern::IsType {
                conforming,
                binding,
                ..
            },
            value,
        ) => {
            use crate::codegen::types::ExecutableType as E;
            let matched = conforming.iter().any(|target| match (target, value) {
                (E::Unit, Value::Unit) => true,
                (E::Bool, Value::Bool(_))
                | (E::Integer, Value::Integer(_))
                | (E::Float, Value::Float(_))
                | (E::Byte, Value::Byte(_))
                | (E::CodePoint, Value::CodePoint(_)) => true,
                (
                    E::Record {
                        record: expected, ..
                    },
                    Value::Record {
                        record: Some(actual),
                        ..
                    },
                ) => expected == actual,
                (
                    E::Variant {
                        variant: expected, ..
                    },
                    Value::Variant {
                        variant: Some(actual),
                        ..
                    },
                ) => expected == actual,
                (E::Bytes, value) => value.bytes_value().is_some(),
                (E::List(_), value) => value.list_value().is_some(),
                _ => false,
            });
            if matched && binding.is_some() {
                bindings.push(value.clone());
            }
            matched
        }
        (Pattern::Record { fields: patterns }, Value::Record { fields, .. }) => {
            let checkpoint = bindings.len();
            for (name, pattern) in patterns {
                let Some(value) = fields.get(name) else {
                    bindings.truncate(checkpoint);
                    return Ok(false);
                };
                if !matches(program, pattern, value, bindings)? {
                    bindings.truncate(checkpoint);
                    return Ok(false);
                }
            }
            true
        }
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
        (Pattern::Symbol(expected), actual) => actual
            .symbol_bytes()
            .is_some_and(|value| value == expected.as_bytes()),
        (
            Pattern::Variant { variant, fields },
            Value::Variant {
                variant: _,
                type_name,
                alternative,
                payload,
            },
        ) => {
            let expected = &program.metadata.variants[variant];
            if type_name != &expected.type_name
                || alternative != &expected.alternative
                || fields.len() != payload.len()
            {
                return Ok(false);
            }
            let checkpoint = bindings.len();
            for (field, value) in fields.iter().zip(payload) {
                if !matches(program, field, value, bindings)? {
                    bindings.truncate(checkpoint);
                    return Ok(false);
                }
            }
            true
        }
        _ => false,
    })
}

fn projected(place: super::value::PlaceHandle) -> Result<Value, crate::error::RuntimeError> {
    // Reference-valued payloads already name their own storage origin.
    Ok(match place.read()? {
        value @ Value::Reference(_) => value,
        _ => Value::Reference(place),
    })
}
