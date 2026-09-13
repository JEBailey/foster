//! Logical callable schema adapter for validated bytecode signatures.
use super::BytecodeFunction;
use crate::codegen::flow;

pub(crate) fn logical_schema(f: &BytecodeFunction) -> flow::FunctionSchema {
    flow::FunctionSchema {
        name: f.name.clone(),
        parameters: crate::types::Parameter::from_parts(
            f.parameter_types.clone(),
            f.parameter_modes.clone(),
        ),
        captures: f.capture_types.clone(),
        result_type: f.result_type.clone(),
        returns_reference: f.returns_reference,
        intrinsic_stub: f.intrinsic_stub,
    }
}
