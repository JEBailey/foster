//! Logical callable schema adapter for validated bytecode signatures.
use super::Function;
use crate::codegen::flow;

pub fn logical_schema(f: &Function) -> flow::FunctionSchema {
    flow::FunctionSchema {
        name: f.name.clone(),
        parameters: f
            .parameters
            .iter()
            .map(|p| crate::types::Parameter {
                ty: p.ty.clone(),
                mode: p.mode,
            })
            .collect(),
        captures: f.capture_types.clone(),
        result_type: f.result_type.clone(),
        returns_reference: f.returns_reference,
        intrinsic_stub: f.intrinsic_stub,
    }
}
