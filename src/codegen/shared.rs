//! Verified shared SSA and its graph-specific analysis evidence.
use super::{
    flow::{FunctionFacts, FunctionSchema},
    ir,
    layout::Registry,
    metadata::ProgramMetadata,
    program::Program,
};
use crate::{error::FosterError, hir::FunctionId};
use std::collections::HashMap;

/// The canonical executable boundary contains no retained construction bytecode.
/// Transformations consume a boundary and publish new evidence only after verification.
#[derive(Debug, Clone)]
pub struct SharedProgram {
    pub(crate) program: Program,
    pub(crate) layouts: Registry,
    pub(crate) drops_inserted: bool,
    pub(crate) signatures: HashMap<FunctionId, ir::Signature>,
    pub(crate) schemas: HashMap<FunctionId, FunctionSchema>,
    pub(crate) facts: HashMap<FunctionId, FunctionFacts>,
    pub(crate) write_bindings: HashMap<FunctionId, HashMap<ir::Value, ir::Value>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_folding_preserves_nan_and_signed_zero_semantics() {
        for (expression, expected) in [
            ("(0.0 / 0.0) == (0.0 / 0.0)", false),
            ("(1.0 / -0.0) < 0.0", true),
        ] {
            let compilation =
                crate::compile(&format!("func main() -> Bool {{ {expression} }}")).unwrap();
            for optimize in [false, true] {
                let program = crate::vm::compile_with_options(
                    &compilation,
                    crate::vm::CompileOptions { optimize },
                )
                .unwrap();
                assert_eq!(
                    crate::vm::Machine::new(&program).run_main().unwrap(),
                    crate::vm::Value::Bool(expected)
                );
            }
        }
    }

    #[test]
    fn optimization_rebuilds_sites_and_values_after_inlining_and_branch_pruning() {
        let compilation = crate::compile("func choose(flag: Bool) -> Int { return 20 + 22 if flag\n 7 }\nfunc main() -> Int { choose(true) }").unwrap();
        let baseline = crate::vm::compile_shared(&compilation).unwrap();
        let id = baseline.metadata().main.unwrap();
        let optimized = baseline.clone().optimized().unwrap();
        let body = &optimized.functions()[&id];
        body.verify(optimized.signatures()).unwrap();
        assert!(
            !body
                .blocks
                .iter()
                .flat_map(|block| &block.instructions)
                .any(|entry| matches!(
                    entry.instruction,
                    ir::Instruction::Portable(ir::PortableInstruction::Call { .. })
                ))
        );
        for (id, body) in optimized.functions() {
            let facts = optimized.facts(*id);
            assert_eq!(facts.values.len(), body.values.len());
            assert_eq!(facts.points.len(), body.blocks.len());
            for (block, points) in body.blocks.iter().zip(&facts.points) {
                assert_eq!(points.len(), block.instructions.len() + 1);
            }
            let rebuilt = super::super::flow::analyze(
                optimized.metadata(),
                optimized.schemas(),
                &optimized.schemas()[id],
                body,
                &optimized.write_bindings[id],
            )
            .unwrap();
            assert_eq!(facts.values, rebuilt.values);
        }
        for shared in [baseline, optimized] {
            let program = crate::codegen::vm::lower_shared_program(shared).unwrap();
            crate::vm::verify(&program).unwrap();
            assert_eq!(
                crate::vm::Machine::new(&program).run_main().unwrap(),
                crate::vm::Value::Integer(42)
            );
        }
    }

    #[test]
    fn branch_pruning_removes_unreachable_traps_but_retains_reachable_traps() {
        for (flag, succeeds) in [(true, true), (false, false)] {
            let source = format!("func main() -> Int {{ branch {{ {flag} -> 42 _ -> 1 / 0 }} }}");
            let compilation = crate::compile(&source).unwrap();
            let shared = crate::vm::compile_shared(&compilation)
                .unwrap()
                .optimized()
                .unwrap();
            let body = &shared.functions()[&shared.metadata().main.unwrap()];
            assert!(
                !body
                    .blocks
                    .iter()
                    .any(|block| matches!(block.terminator, ir::Terminator::Branch { .. }))
            );
            let program = crate::codegen::vm::lower_shared_program(shared).unwrap();
            assert_eq!(
                crate::vm::Machine::new(&program).run_main().is_ok(),
                succeeds
            );
        }
    }
}

impl SharedProgram {
    pub fn metadata(&self) -> &ProgramMetadata {
        &self.program.metadata
    }
    pub fn functions(&self) -> &HashMap<FunctionId, ir::Function> {
        &self.program.bodies
    }
    pub fn signatures(&self) -> &HashMap<FunctionId, ir::Signature> {
        &self.signatures
    }
    pub fn schemas(&self) -> &HashMap<FunctionId, FunctionSchema> {
        &self.schemas
    }
    pub fn facts(&self, function: FunctionId) -> &FunctionFacts {
        &self.facts[&function]
    }
    pub(crate) fn into_parts(self) -> (Program, HashMap<FunctionId, FunctionFacts>, Registry) {
        (self.program, self.facts, self.layouts)
    }

    /// All shared semantic passes run here, before either backend lowers SSA.
    pub fn optimized(mut self) -> Result<Self, FosterError> {
        // No pass can observe evidence from the previous graph revision.
        let mut previous = std::mem::take(&mut self.facts);
        let changed = super::optimizer::run_shared(&mut self.program, &mut self.write_bindings)?;
        // Changed graphs cannot retain instruction-indexed facts. Rebuild only
        // affected functions, including callers changed by interprocedural passes.
        let mut changed = changed.into_iter().collect::<Vec<_>>();
        changed.sort();
        for id in changed {
            let function = &self.program.bodies[&id];
            function
                .verify(&self.signatures)
                .map_err(|error| FosterError::runtime(format!("invalid optimized SSA: {error}")))?;
            previous.remove(&id);
            let facts = super::flow::analyze(
                &self.program.metadata,
                &self.schemas,
                &self.schemas[&id],
                function,
                &self.write_bindings[&id],
            )?;
            self.facts.insert(id, facts);
        }
        self.facts.extend(previous);
        Ok(self)
    }
}
