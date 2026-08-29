use std::ops::Range as ByteRange;

use lsp_types::{Position, Range, Uri};

use crate::hir::Compilation;

use super::byte_range_to_lsp;
use super::workspace::{module_for_uri, position_to_offset};

#[derive(Debug, Clone)]
struct FunctionMapping {
    semantic: ByteRange<usize>,
    current: ByteRange<usize>,
}

/// Maps positions between a last-good semantic document and the current editor text. When the
/// texts differ, only complete functions whose source is unchanged participate in the mapping.
pub(super) struct SemanticSnapshot<'a> {
    semantic_source: &'a str,
    current_source: &'a str,
    exact: bool,
    functions: Vec<FunctionMapping>,
}

impl<'a> SemanticSnapshot<'a> {
    pub(super) fn new(
        compilation: &'a Compilation,
        uri: &Uri,
        current_source: &'a str,
    ) -> Option<Self> {
        let module = module_for_uri(compilation, uri)?;
        let module = &compilation.hir.modules[module];
        let source_module = compilation.package.module(&module.name)?;
        let semantic_source = source_module.source.as_deref()?;
        let exact = semantic_source == current_source;
        let functions = if exact {
            Vec::new()
        } else {
            let program = source_module.program.as_ref()?;
            program
                .functions
                .iter()
                .map(|function| &function.span)
                .chain(program.tests.iter().map(|test| &test.span))
                .filter_map(|span| {
                    unchanged_function_mapping(semantic_source, current_source, span)
                })
                .collect()
        };
        Some(Self {
            semantic_source,
            current_source,
            exact,
            functions,
        })
    }

    pub(super) fn semantic_source(&self) -> &'a str {
        self.semantic_source
    }

    pub(super) fn current_source(&self) -> &'a str {
        self.current_source
    }

    pub(super) fn current_position_to_semantic_offset(&self, position: Position) -> Option<usize> {
        let current = position_to_offset(self.current_source, position)?;
        self.current_offset_to_semantic(current)
    }

    pub(super) fn current_offset_to_semantic(&self, current: usize) -> Option<usize> {
        if self.exact {
            return Some(current);
        }
        self.functions
            .iter()
            .find(|mapping| mapping.current.start <= current && current <= mapping.current.end)
            .map(|mapping| mapping.semantic.start + current - mapping.current.start)
    }

    pub(super) fn semantic_offset_to_current(&self, semantic: usize) -> Option<usize> {
        if self.exact {
            return Some(semantic);
        }
        self.functions
            .iter()
            .find(|mapping| mapping.semantic.start <= semantic && semantic <= mapping.semantic.end)
            .map(|mapping| mapping.current.start + semantic - mapping.semantic.start)
    }

    pub(super) fn semantic_range_to_current(&self, range: Range) -> Option<Range> {
        let start = position_to_offset(self.semantic_source, range.start)?;
        let end = position_to_offset(self.semantic_source, range.end)?;
        let current_start = self.semantic_offset_to_current(start)?;
        let current_end = self.semantic_offset_to_current(end)?;
        Some(byte_range_to_lsp(
            self.current_source,
            current_start..current_end,
        ))
    }
}

fn unchanged_function_mapping(
    semantic_source: &str,
    current_source: &str,
    span: &ByteRange<usize>,
) -> Option<FunctionMapping> {
    let text = semantic_source.get(span.clone())?;
    let mut matches = current_source.match_indices(text);
    let (start, _) = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(FunctionMapping {
        semantic: span.clone(),
        current: start..start + text.len(),
    })
}
