//! Immutable shared-program boundary and transactional backend orchestration.
use super::LowerError;
use super::construction::seal_function_with_evidence;
use super::emission::{FunctionMetadata, lower_function};
#[cfg(test)]
use super::evidence::compare_flow;
use crate::codegen::flow::{FunctionFacts, FunctionSchema};
use crate::codegen::ir::{self};
use crate::codegen::metadata::ProgramMetadata;
use crate::hir::FunctionId;
use crate::vm::schema::logical_schema;
use crate::vm::{self};
use std::collections::HashMap;
/// Validated SSA plus immutable nominal/runtime metadata shared by executable backends.
/// Construct through `seal_program`; transforms consume the boundary and must validate their output.
///
/// ```compile_fail
/// use foster::codegen::vm::SharedProgram;
/// fn invalidate(program: &mut SharedProgram) {
///     program.functions().clear();
/// }
/// ```
#[derive(Debug)]
pub struct SharedProgram {
    metadata: ProgramMetadata,
    construction_functions: HashMap<FunctionId, vm::BytecodeFunction>,
    drops_inserted: bool,
    functions: HashMap<FunctionId, ir::Function>,
    signatures: HashMap<FunctionId, ir::Signature>,
    schemas: HashMap<FunctionId, FunctionSchema>,
    facts: HashMap<FunctionId, FunctionFacts>,
}

impl SharedProgram {
    pub fn schemas(&self) -> &HashMap<FunctionId, FunctionSchema> {
        &self.schemas
    }
    pub fn facts(&self, function: FunctionId) -> &FunctionFacts {
        &self.facts[&function]
    }
    pub fn metadata(&self) -> &ProgramMetadata {
        &self.metadata
    }
    pub fn functions(&self) -> &HashMap<FunctionId, ir::Function> {
        &self.functions
    }
    pub fn signatures(&self) -> &HashMap<FunctionId, ir::Signature> {
        &self.signatures
    }

    /// Consuming extraction for backend transformation; the result is no longer a sealed program.
    pub(crate) fn into_parts(
        self,
    ) -> (
        vm::Program,
        HashMap<FunctionId, ir::Function>,
        HashMap<FunctionId, FunctionFacts>,
    ) {
        (
            vm::Program {
                metadata: self.metadata,
                functions: self.construction_functions,
                drops_inserted: self.drops_inserted,
            },
            self.functions,
            self.facts,
        )
    }
}

/// Retain the compiler's first SSA graph instead of immediately de-SSA lowering it to bytecode.
pub fn seal_program(construction: vm::Program) -> Result<SharedProgram, LowerError> {
    let SealedFunctions {
        functions,
        signatures,
        schemas,
        facts,
    } = seal_construction(&construction)?;
    Ok(SharedProgram {
        metadata: construction.metadata,
        construction_functions: construction.functions,
        drops_inserted: construction.drops_inserted,
        functions,
        signatures,
        schemas,
        facts,
    })
}

/// Both program consumers receive the same complete, verified SSA graph and signatures.
struct SealedFunctions {
    schemas: HashMap<FunctionId, FunctionSchema>,
    facts: HashMap<FunctionId, FunctionFacts>,
    functions: HashMap<FunctionId, ir::Function>,
    signatures: HashMap<FunctionId, ir::Signature>,
}

fn seal_construction(construction: &vm::Program) -> Result<SealedFunctions, LowerError> {
    vm::verify(construction)
        .map_err(|error| LowerError(format!("invalid construction metadata: {error}")))?;
    let result_types = construction
        .functions
        .iter()
        .map(|(id, function)| (*id, function.result_type.clone()))
        .collect::<HashMap<_, _>>();
    let schemas = construction
        .functions
        .iter()
        .map(|(id, body)| (*id, logical_schema(body)))
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
        let analyzed = crate::codegen::flow::analyze(
            &construction.metadata,
            &schemas,
            &schemas[id],
            shared,
            &source_maps[id].write_bindings,
        )
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
        facts.insert(*id, analyzed);
    }
    Ok(SealedFunctions {
        functions,
        signatures,
        schemas,
        facts,
    })
}

/// Make shared SSA the mandatory backend boundary for every executable VM function.
pub fn lower_program_through_shared_ir(program: &mut vm::Program) -> Result<(), LowerError> {
    let SealedFunctions {
        functions,
        signatures,
        ..
    } = seal_construction(program)?;
    // Keep original bodies until every lowering succeeds, without cloning instruction payloads.
    // Lowering only appends constants, so truncation also restores metadata after an error.
    let original_constant_count = program.metadata.constants.len();
    let mut lowered = HashMap::with_capacity(program.functions.len());
    for (id, function) in functions {
        let original = &program.functions[&id];
        let metadata = FunctionMetadata::from_bytecode(original);
        match lower_function(
            &function,
            &signatures,
            &mut program.metadata.constants,
            metadata,
        ) {
            Ok(function) => {
                lowered.insert(id, function);
            }
            Err(error) => {
                program.metadata.constants.truncate(original_constant_count);
                return Err(error);
            }
        }
    }
    for (id, function) in std::mem::take(&mut program.functions) {
        if function.intrinsic_stub {
            lowered.insert(id, function);
        }
    }
    program.functions = lowered;
    Ok(())
}
