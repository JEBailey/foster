use std::fmt;
use std::ops::Range;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorLabel {
    pub range: Range<usize>,
    pub message: String,
    pub primary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FosterError {
    pub message: String,
    pub line: usize,
    pub column: usize,
    pub code: Option<String>,
    pub source_module: Option<String>,
    pub labels: Vec<ErrorLabel>,
    pub notes: Vec<String>,
    pub help: Option<String>,
}

impl FosterError {
    pub fn new(message: impl Into<String>, line: usize, column: usize) -> Self {
        Self {
            message: message.into(),
            line,
            column,
            code: None,
            source_module: None,
            labels: Vec::new(),
            notes: Vec::new(),
            help: None,
        }
    }

    pub fn runtime(message: impl Into<String>) -> Self {
        Self::new(message, 0, 0)
    }

    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }

    pub fn with_source_module(mut self, module: impl Into<String>) -> Self {
        self.source_module = Some(module.into());
        self
    }

    pub fn with_primary_label(mut self, range: Range<usize>, message: impl Into<String>) -> Self {
        self.labels.push(ErrorLabel {
            range,
            message: message.into(),
            primary: true,
        });
        self
    }

    pub fn with_label(mut self, range: Range<usize>, message: impl Into<String>) -> Self {
        self.labels.push(ErrorLabel {
            range,
            message: message.into(),
            primary: false,
        });
        self
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }
}

impl fmt::Display for FosterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line == 0 {
            write!(f, "{}", self.message)
        } else {
            write!(f, "{}:{}: {}", self.line, self.column, self.message)
        }
    }
}

impl std::error::Error for FosterError {}

/// A compact failure produced while executing verified bytecode.
///
/// Runtime code does not construct source diagnostics. Public VM entry points convert this into
/// `FosterError`, keeping diagnostic presentation at the API boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeError {
    pub(crate) message: String,
}

impl RuntimeError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub(crate) fn runtime(message: impl Into<String>) -> Self {
        Self::new(message)
    }
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for RuntimeError {}

impl From<RuntimeError> for FosterError {
    fn from(error: RuntimeError) -> Self {
        Self::runtime(error.message)
    }
}

/// Identifies the compiler phase that rejected a program while retaining its rich diagnostic.
#[derive(Debug)]
pub(crate) enum CompileError {
    Lowering(Box<FosterError>),
    Effects(Box<FosterError>),
    Types(Box<FosterError>),
    Ownership(Box<FosterError>),
    Loans(Box<FosterError>),
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

    pub(crate) fn loans(error: FosterError) -> Self {
        Self::Loans(Box::new(error))
    }
}

impl From<CompileError> for FosterError {
    fn from(error: CompileError) -> Self {
        match error {
            CompileError::Lowering(error)
            | CompileError::Effects(error)
            | CompileError::Types(error)
            | CompileError::Ownership(error)
            | CompileError::Loans(error) => *error,
        }
    }
}
