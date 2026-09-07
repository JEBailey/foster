//! Ownership retained at a modeled failure, before a result has been produced.
use super::*;
use std::ops::{Deref, DerefMut};

#[derive(Default)]
pub(super) struct FailureCleanup {
    pub values: HashMap<(usize, usize), Vec<ir::Value>>,
}

/// Keep the active instruction's cleanup with the builder so failures inside nested
/// lowering helpers use the same ownership boundary as direct calls and assertions.
pub(super) struct NativeBuilder<'a> {
    inner: cranelift_frontend::FunctionBuilder<'a>,
    pub cleanup: Vec<(ClifValue, FuncId)>,
    /// Address-taken owners may be replaced by a failing call. Reload these
    /// homes on the failure edge instead of releasing their pre-call values.
    pub cleanup_homes: HashMap<ClifValue, StackSlot>,
}

impl<'a> NativeBuilder<'a> {
    pub fn new(
        function: &'a mut cranelift_codegen::ir::Function,
        context: &'a mut FunctionBuilderContext,
    ) -> Self {
        Self {
            inner: cranelift_frontend::FunctionBuilder::new(function, context),
            cleanup: Vec::new(),
            cleanup_homes: HashMap::new(),
        }
    }

    pub fn finalize(self, config: cranelift_codegen::isa::TargetFrontendConfig) {
        self.inner.finalize(config);
    }
}

impl<'a> Deref for NativeBuilder<'a> {
    type Target = cranelift_frontend::FunctionBuilder<'a>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for NativeBuilder<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}
