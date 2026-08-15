use std::io;
use std::ops::Range;

use ariadne::{Color, Label as AriadneLabel, Report, ReportKind, Source};

use crate::error::FosterError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    pub range: Range<usize>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: Option<String>,
    pub source_module: Option<String>,
    pub message: String,
    pub labels: Vec<Label>,
    pub notes: Vec<String>,
}

impl Diagnostic {
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            code: None,
            source_module: None,
            message: message.into(),
            labels: Vec::new(),
            notes: Vec::new(),
        }
    }

    pub fn warning(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            code: Some(code.into()),
            source_module: None,
            message: message.into(),
            labels: Vec::new(),
            notes: Vec::new(),
        }
    }

    pub fn with_source_module(mut self, module: impl Into<String>) -> Self {
        self.source_module = Some(module.into());
        self
    }

    pub fn with_label(mut self, range: Range<usize>, message: impl Into<String>) -> Self {
        self.labels.push(Label {
            range,
            message: message.into(),
        });
        self
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    pub fn from_source_error(source: &str, error: &FosterError) -> Self {
        let mut diagnostic = Self::error(error.message.clone());
        if error.line > 0 {
            let offset = byte_offset(source, error.line, error.column);
            let end = source[offset..]
                .chars()
                .next()
                .map_or(offset, |character| offset + character.len_utf8());
            diagnostic = diagnostic.with_label(offset..end, error.message.clone());
        }
        diagnostic
    }
}

pub fn eprint(source_name: &str, source: &str, diagnostic: &Diagnostic) -> io::Result<()> {
    let kind = match diagnostic.severity {
        Severity::Error => ReportKind::Error,
        Severity::Warning => ReportKind::Warning,
    };
    let primary = diagnostic
        .labels
        .first()
        .map_or(0..0, |label| label.range.clone());
    let mut report = Report::build(kind, (source_name, primary)).with_message(&diagnostic.message);
    if let Some(code) = &diagnostic.code {
        report = report.with_code(code);
    }
    for label in &diagnostic.labels {
        report = report.with_label(
            AriadneLabel::new((source_name, label.range.clone()))
                .with_message(&label.message)
                .with_color(match diagnostic.severity {
                    Severity::Error => Color::Red,
                    Severity::Warning => Color::Yellow,
                }),
        );
    }
    for note in &diagnostic.notes {
        report = report.with_note(note);
    }
    report.finish().eprint((source_name, Source::from(source)))
}

fn byte_offset(source: &str, line: usize, column: usize) -> usize {
    let line_start = source
        .split_inclusive('\n')
        .take(line.saturating_sub(1))
        .map(str::len)
        .sum::<usize>();
    let line_text = source.get(line_start..).unwrap_or_default();
    let column_offset = line_text
        .char_indices()
        .nth(column.saturating_sub(1))
        .map_or_else(|| line_text.len(), |(offset, _)| offset);
    (line_start + column_offset).min(source.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_character_positions_to_byte_ranges() {
        let source = "first\nλ!\n";
        let error = FosterError::new("unexpected token", 2, 2);
        let diagnostic = Diagnostic::from_source_error(source, &error);
        assert_eq!(diagnostic.labels[0].range, 8..9);
    }
}
