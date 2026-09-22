use super::*;

impl Workspace {
    pub(in crate::lsp) fn formatting(
        &self,
        params: &lsp_types::DocumentFormattingParams,
    ) -> Result<Vec<TextEdit>, crate::error::FosterError> {
        let uri = &params.text_document.uri;
        let source = if let Some(document) = self.documents.get(uri) {
            document.text.clone()
        } else {
            let path = uri_to_path(uri).ok_or_else(|| {
                crate::error::FosterError::runtime("formatting requires a file URI")
            })?;
            std::fs::read_to_string(path).map_err(|error| {
                crate::error::FosterError::runtime(format!("cannot read document: {error}"))
            })?
        };
        // Foster has one canonical formatter and indentation policy. The editor's
        // tab settings do not override the output of `foster fmt`.
        let formatted = crate::formatter::format(&source)?;
        Ok(format_edits(&source, &formatted))
    }

    pub(in crate::lsp) fn workspace_symbols(
        &self,
        query: &str,
    ) -> Vec<lsp_types::SymbolInformation> {
        let files = std::sync::Arc::clone(&self.compilations.symbol_files.borrow());
        let mut symbols = Vec::new();
        for (uri, entries) in files.iter() {
            if crate::compiler::cancellation::is_cancelled() {
                return Vec::new();
            }
            if !self.documents.contains_key(uri) {
                symbols.extend(entries.iter().filter_map(|symbol| {
                    Some((super::super::symbols::rank(symbol, query)?, symbol.clone()))
                }));
            }
        }
        // The overlay entirely replaces its disk entry, including deleted or
        // malformed declarations. Search never resurrects stale source positions.
        for (uri, document) in &self.documents {
            if crate::compiler::cancellation::is_cancelled() {
                return Vec::new();
            }
            symbols.extend(
                super::super::symbols::source_symbols(uri, &document.text)
                    .into_iter()
                    .filter_map(|symbol| {
                        Some((super::super::symbols::rank(&symbol, query)?, symbol))
                    }),
            );
        }
        symbols.sort_by(|(left_rank, left), (right_rank, right)| {
            left_rank
                .cmp(right_rank)
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| left.location.uri.as_str().cmp(right.location.uri.as_str()))
                .then_with(|| left.location.range.start.cmp(&right.location.range.start))
        });
        symbols.dedup_by(|(_, left), (_, right)| {
            left.name == right.name && left.location == right.location
        });
        symbols
            .into_iter()
            .take(256)
            .map(|(_, symbol)| symbol)
            .collect()
    }
}

fn format_edits(source: &str, formatted: &str) -> Vec<TextEdit> {
    let before = source.split_inclusive('\n').collect::<Vec<_>>();
    let after = formatted.split_inclusive('\n').collect::<Vec<_>>();
    if before.len() == after.len() {
        let mut offset = 0;
        let mut edits = Vec::new();
        for (before, after) in before.into_iter().zip(after) {
            if let Some(edit) = changed_text(source, offset, before, after) {
                edits.push(edit);
            }
            offset += before.len();
        }
        edits
    } else {
        changed_text(source, 0, source, formatted)
            .into_iter()
            .collect()
    }
}

fn changed_text(source: &str, offset: usize, before: &str, after: &str) -> Option<TextEdit> {
    if before == after {
        return None;
    }
    let prefix = before
        .chars()
        .zip(after.chars())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .sum::<usize>();
    let suffix = before[prefix..]
        .chars()
        .rev()
        .zip(after[prefix..].chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .sum::<usize>();
    let mut end_before = before.len() - suffix;
    let mut end_after = after.len() - suffix;
    // LSP positions cannot address the middle of CRLF. Replace the complete
    // newline using the next line's column zero when normalizing line endings.
    if before.as_bytes().get(end_before) == Some(&b'\n')
        && end_before > 0
        && before.as_bytes()[end_before - 1] == b'\r'
    {
        end_before += 1;
        end_after += 1;
    }
    Some(TextEdit {
        range: byte_range_to_lsp(source, offset + prefix..offset + end_before),
        new_text: after[prefix..end_after].into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(source: &str, edits: &[TextEdit]) -> String {
        let end = byte_range_to_lsp(source, source.len()..source.len()).end;
        let offset = |position| {
            if position == end {
                source.len()
            } else {
                position_to_offset(source, position).unwrap()
            }
        };
        let mut result = source.to_owned();
        for edit in edits.iter().rev() {
            result.replace_range(
                offset(edit.range.start)..offset(edit.range.end),
                &edit.new_text,
            );
        }
        result
    }

    #[test]
    fn formatting_edits_match_cli_and_preserve_unchanged_lines() {
        let source = "func main() -> Int {\nlet text = \"😀\"   \n    // unchanged\n 42\n}\n";
        let formatted = crate::formatter::format(source).unwrap();
        let edits = format_edits(source, &formatted);
        assert_eq!(apply(source, &edits), formatted);
        assert_eq!(edits.len(), 2);
        assert!(
            edits
                .iter()
                .all(|edit| edit.range.start.line == edit.range.end.line)
        );
        assert!(format_edits(&formatted, &formatted).is_empty());
    }

    #[test]
    fn formatting_handles_crlf_empty_text_and_line_count_changes() {
        for source in [
            "",
            "\n\n",
            "func main() {\r\nlet text = \"😀\"  \r\n}\r\n",
            "\nfunc main() {\n\n\n0\n}\n\n",
            "func main() { 0 }",
        ] {
            let formatted = crate::formatter::format(source).unwrap();
            assert_eq!(
                apply(source, &format_edits(source, &formatted)),
                formatted,
                "{source:?}"
            );
        }
    }

    #[test]
    fn formatting_uses_unsaved_text_and_rejects_malformed_source() {
        let uri = "file:///format-test.fos".parse().unwrap();
        let mut workspace = Workspace::new(&InitializeParams::default());
        let source = "func main() {\n0\n}";
        workspace.open(uri, source.into(), 3);
        let params: lsp_types::DocumentFormattingParams = serde_json::from_value(serde_json::json!({
            "textDocument": { "uri": "file:///format-test.fos" }, "options": { "tabSize": 2, "insertSpaces": false }
        })).unwrap();
        assert_eq!(
            apply(source, &workspace.formatting(&params).unwrap()),
            crate::formatter::format(source).unwrap()
        );
        workspace.change(
            params.text_document.uri.clone(),
            "func main() { let value = }".into(),
            4,
        );
        assert!(workspace.formatting(&params).is_err());
    }

    #[test]
    fn workspace_search_uses_unsaved_names_and_positions_without_checking() {
        let mut workspace = Workspace::new(&InitializeParams::default());
        workspace.compilations.snapshot_only = true;
        let uri: Uri = "file:///symbols-test.fos".parse().unwrap();
        let source =
            "/// buildValue documentation\nfunc buildValue() -> Int { 0 }\nfunc unrelated() {}\n";
        workspace.open(uri.clone(), source.into(), 1);
        let results = workspace.workspace_symbols("bV");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "buildValue");
        assert_eq!(results[0].location.range.start, Position::new(1, 5));
        workspace.change(uri.clone(), "\nfunc renamed() { 0 }".into(), 2);
        assert!(workspace.workspace_symbols("buildValue").is_empty());
        assert_eq!(
            workspace.workspace_symbols("RENAMED")[0]
                .location
                .range
                .start
                .line,
            1
        );
        workspace.close(&uri);
        assert!(workspace.workspace_symbols("").is_empty());
    }
}
