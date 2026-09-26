pub use foster_bytecode::error::*;
/// Identifies the compiler phase that rejected a program while retaining its rich diagnostic.
#[derive(Debug)]
pub(crate) enum CompileError {
    Lowering(Box<FosterError>),
    Effects(Box<FosterError>),
    Types(Box<FosterError>),
    Ownership(Box<FosterError>),
}

impl CompileError {
    pub(crate) fn lowering(error: FosterError) -> Self {
        Self::Lowering(Box::new(error))
    }

    pub(crate) fn effects(error: FosterError) -> Self {
        Self::Effects(Box::new(error))
    }

    pub(crate) fn types(error: FosterError) -> Self {
        Self::Types(Box::new(error))
    }

    pub(crate) fn ownership(error: FosterError) -> Self {
        Self::Ownership(Box::new(error))
    }
}

impl From<CompileError> for FosterError {
    fn from(error: CompileError) -> Self {
        match error {
            CompileError::Lowering(error)
            | CompileError::Effects(error)
            | CompileError::Types(error)
            | CompileError::Ownership(error) => *error,
        }
    }
}
