use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FosterError {
    pub message: String,
    pub line: usize,
    pub column: usize,
}

impl FosterError {
    pub fn new(message: impl Into<String>, line: usize, column: usize) -> Self {
        Self {
            message: message.into(),
            line,
            column,
        }
    }

    pub fn runtime(message: impl Into<String>) -> Self {
        Self::new(message, 0, 0)
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
