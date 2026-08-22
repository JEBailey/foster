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

    pub fn with_primary_label(
        mut self,
        range: Range<usize>,
        message: impl Into<String>,
    ) -> Self {
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
