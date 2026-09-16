#[cfg(test)]
use super::evidence::compare_flow;
use super::seal_function_with_evidence;
use crate::codegen::LowerError;
use crate::codegen::flow::{FunctionFacts, FunctionSchema};
use crate::codegen::shared::SharedProgram;
use crate::codegen::{ir, storage};
use crate::hir::FunctionId;
use std::collections::HashMap;
/// Retain the compiler's first SSA graph instead of immediately de-SSA lowering it to bytecode.
pub fn seal_program(mut construction: storage::Program) -> Result<SharedProgram, LowerError> {
    let layouts = crate::compiler::profile::measure("shared.layouts", || {
        crate::codegen::layout::legalize(&mut construction)
    })
    .map_err(|e| LowerError(e.to_string()))?;
    let SealedFunctions {
        functions,
        signatures,
        schemas,
        facts,
        write_bindings,
    } = seal_construction(&construction)?;
    Ok(SharedProgram {
        program: std::sync::Arc::new(crate::codegen::program::Program {
            metadata: construction.metadata,
            functions: construction
                .functions
                .iter()
                .map(|(id, body)| {
                    (
                        *id,
                        crate::codegen::program::FunctionDeclaration::from_construction(body),
                    )
                })
                .collect(),
            bodies: functions,
        }),
        layouts,
        drops_inserted: construction.drops_inserted,
        signatures,
        schemas,
        facts,
        write_bindings,
    })
}

/// Both program consumers receive the same complete, verified SSA graph and signatures.
struct SealedFunctions {
    write_bindings: HashMap<FunctionId, HashMap<ir::Value, ir::Value>>,
    schemas: HashMap<FunctionId, FunctionSchema>,
    facts: HashMap<FunctionId, std::sync::Arc<FunctionFacts>>,
    functions: HashMap<FunctionId, ir::Function>,
    signatures: HashMap<FunctionId, ir::Signature>,
}

fn seal_construction(construction: &storage::Program) -> Result<SealedFunctions, LowerError> {
    crate::compiler::profile::measure("shared.verify_construction", || {
        storage::verification::verify(construction)
    })
    .map_err(|error| LowerError(format!("invalid construction metadata: {error}")))?;
    let result_types = construction
        .functions
        .iter()
        .map(|(id, function)| (*id, function.result_type.clone()))
        .collect::<HashMap<_, _>>();
    let schemas = construction
        .functions
        .iter()
        .map(|(id, body)| {
            (
                *id,
                crate::codegen::program::FunctionDeclaration::from_construction(body).schema(),
            )
        })
        .collect();
    let mut facts = HashMap::new();
    let mut source_maps = HashMap::new();
    let mut functions = HashMap::with_capacity(construction.functions.len());
    for (id, function) in &construction.functions {
        if function.intrinsic_stub {
            continue;
        }
        let (shared, _sources) =
            seal_function_with_evidence(&construction.metadata.constants, &result_types, function)?;
        source_maps.insert(*id, _sources);
        functions.insert(*id, shared);
    }
    let signatures = functions
        .iter()
        .map(|(id, function)| (*id, function.signature.clone()))
        .collect::<HashMap<_, _>>();
    for function in functions.values() {
        function
            .verify(&signatures)
            .map_err(|error| LowerError(format!("invalid shared IR: {error}")))?;
    }
    for (id, shared) in &functions {
        let analyzed = crate::compiler::profile::measure("shared.flow", || {
            crate::codegen::flow::analyze(
                &construction.metadata,
                &schemas,
                &schemas[id],
                shared,
                &source_maps[id].write_bindings,
            )
        })
        .map_err(|e| LowerError(e.to_string()))?;
        #[cfg(test)]
        compare_flow(
            construction,
            &construction.functions[id],
            shared,
            &source_maps[id],
            &analyzed,
            &schemas,
        )?;
        facts.insert(*id, std::sync::Arc::new(analyzed));
    }
    Ok(SealedFunctions {
        write_bindings: source_maps
            .into_iter()
            .map(|(id, evidence)| (id, evidence.write_bindings))
            .collect(),
        functions,
        signatures,
        schemas,
        facts,
    })
}
