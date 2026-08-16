use std::error::Error;

use lsp_server::{Connection, ErrorCode, Message, Notification, Response};
use lsp_types::{
    CompletionOptions, CompletionParams, Diagnostic, DiagnosticSeverity,
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DocumentSymbolParams, GotoDefinitionParams, HoverParams, InitializeParams, InitializeResult,
    InlayHintParams, Position, PublishDiagnosticsParams, Range, ReferenceParams, RenameParams,
    ServerCapabilities, ServerInfo, SignatureHelpOptions, SignatureHelpParams,
    TextDocumentSyncCapability, TextDocumentSyncKind, Uri,
    request::{
        Completion, DocumentSymbolRequest, GotoDefinition, HoverRequest, InlayHintRequest,
        References, Rename, Request, SignatureHelpRequest,
    },
};

mod builtins;
mod hints;
mod workspace;
use workspace::Workspace;

pub fn run() -> Result<(), Box<dyn Error>> {
    let (connection, io_threads) = Connection::stdio();
    let (initialize_id, initialize_params) = connection.initialize_start()?;
    let initialize_params: InitializeParams = serde_json::from_value(initialize_params)?;
    let capabilities = ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        document_symbol_provider: Some(lsp_types::OneOf::Left(true)),
        definition_provider: Some(lsp_types::OneOf::Left(true)),
        hover_provider: Some(lsp_types::HoverProviderCapability::Simple(true)),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![".".into()]),
            ..CompletionOptions::default()
        }),
        signature_help_provider: Some(SignatureHelpOptions {
            trigger_characters: Some(vec!["(".into(), ",".into()]),
            retrigger_characters: Some(vec![",".into()]),
            ..SignatureHelpOptions::default()
        }),
        inlay_hint_provider: Some(lsp_types::OneOf::Left(true)),
        references_provider: Some(lsp_types::OneOf::Left(true)),
        rename_provider: Some(lsp_types::OneOf::Left(true)),
        ..ServerCapabilities::default()
    };
    connection.initialize_finish(
        initialize_id,
        serde_json::to_value(InitializeResult {
            capabilities,
            server_info: Some(ServerInfo {
                name: "foster".into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
            }),
        })?,
    )?;
    let mut workspace = Workspace::new(&initialize_params);

    for message in &connection.receiver {
        match message {
            Message::Request(request) => {
                if connection.handle_shutdown(&request)? {
                    break;
                }
                let response = match request.method.as_str() {
                    DocumentSymbolRequest::METHOD => {
                        let params: DocumentSymbolParams = serde_json::from_value(request.params)?;
                        Response::new_ok(
                            request.id,
                            workspace.document_symbols(&params.text_document.uri),
                        )
                    }
                    GotoDefinition::METHOD => {
                        let params: GotoDefinitionParams = serde_json::from_value(request.params)?;
                        Response::new_ok(
                            request.id,
                            workspace.definition(&params.text_document_position_params),
                        )
                    }
                    HoverRequest::METHOD => {
                        let params: HoverParams = serde_json::from_value(request.params)?;
                        Response::new_ok(
                            request.id,
                            workspace.hover(&params.text_document_position_params),
                        )
                    }
                    Completion::METHOD => {
                        let params: CompletionParams = serde_json::from_value(request.params)?;
                        Response::new_ok(request.id, workspace.completion(&params))
                    }
                    SignatureHelpRequest::METHOD => {
                        let params: SignatureHelpParams = serde_json::from_value(request.params)?;
                        Response::new_ok(request.id, workspace.signature_help(&params))
                    }
                    InlayHintRequest::METHOD => {
                        let params: InlayHintParams = serde_json::from_value(request.params)?;
                        Response::new_ok(request.id, workspace.inlay_hints(&params))
                    }
                    References::METHOD => {
                        let params: ReferenceParams = serde_json::from_value(request.params)?;
                        Response::new_ok(request.id, workspace.references(&params))
                    }
                    Rename::METHOD => {
                        let params: RenameParams = serde_json::from_value(request.params)?;
                        Response::new_ok(request.id, workspace.rename(&params))
                    }
                    _ => Response::new_err(
                        request.id,
                        ErrorCode::MethodNotFound as i32,
                        format!("unsupported request `{}`", request.method),
                    ),
                };
                connection.sender.send(Message::Response(response))?;
            }
            Message::Notification(notification) => match notification.method.as_str() {
                "textDocument/didOpen" => {
                    let params: DidOpenTextDocumentParams =
                        serde_json::from_value(notification.params)?;
                    let uri = params.text_document.uri;
                    let text = params.text_document.text;
                    workspace.open(uri, text, params.text_document.version);
                    workspace.publish_diagnostics(&connection)?;
                }
                "textDocument/didChange" => {
                    let params: DidChangeTextDocumentParams =
                        serde_json::from_value(notification.params)?;
                    if let Some(change) = params.content_changes.into_iter().last() {
                        let uri = params.text_document.uri;
                        workspace.change(uri, change.text, params.text_document.version);
                        workspace.publish_diagnostics(&connection)?;
                    }
                }
                "textDocument/didClose" => {
                    let params: DidCloseTextDocumentParams =
                        serde_json::from_value(notification.params)?;
                    workspace.close(&params.text_document.uri);
                    workspace.publish_diagnostics(&connection)?;
                }
                "workspace/didChangeWatchedFiles" => {
                    workspace.publish_diagnostics(&connection)?;
                }
                _ => {}
            },
            Message::Response(_) => {}
        }
    }
    io_threads.join()?;
    Ok(())
}

pub(super) fn publish(
    connection: &Connection,
    uri: Uri,
    diagnostics: Vec<Diagnostic>,
    version: Option<i32>,
) -> Result<(), Box<dyn Error>> {
    let params = PublishDiagnosticsParams {
        uri,
        diagnostics,
        version,
    };
    connection
        .sender
        .send(Message::Notification(Notification::new(
            "textDocument/publishDiagnostics".into(),
            params,
        )))?;
    Ok(())
}

#[cfg(test)]
fn diagnostics(source: &str) -> Vec<Diagnostic> {
    match crate::compile(source) {
        Ok(compilation) => compilation
            .diagnostics
            .into_iter()
            .filter(|diagnostic| {
                diagnostic
                    .source_module
                    .as_deref()
                    .is_none_or(|module| module == "main")
            })
            .map(|diagnostic| {
                let range = diagnostic.labels.first().map_or_else(
                    || Range::new(Position::new(0, 0), Position::new(0, 1)),
                    |label| byte_range_to_lsp(source, label.range.clone()),
                );
                Diagnostic {
                    range,
                    severity: Some(match diagnostic.severity {
                        crate::diagnostic::Severity::Error => DiagnosticSeverity::ERROR,
                        crate::diagnostic::Severity::Warning => DiagnosticSeverity::WARNING,
                    }),
                    code: diagnostic.code.map(lsp_types::NumberOrString::String),
                    code_description: None,
                    source: Some("foster".into()),
                    message: diagnostic.message,
                    related_information: None,
                    tags: None,
                    data: None,
                }
            })
            .collect(),
        Err(error) => vec![error_diagnostic(source, error)],
    }
}

pub(super) fn error_diagnostic(source: &str, error: crate::error::FosterError) -> Diagnostic {
    let position = if error.line == 0 {
        Position::new(0, 0)
    } else {
        Position::new(
            u32::try_from(error.line.saturating_sub(1)).unwrap_or(u32::MAX),
            utf16_column(source, error.line, error.column),
        )
    };
    Diagnostic {
        range: Range::new(
            position,
            Position::new(position.line, position.character + 1),
        ),
        severity: Some(DiagnosticSeverity::ERROR),
        code: None,
        code_description: None,
        source: Some("foster".into()),
        message: error.message,
        related_information: None,
        tags: None,
        data: None,
    }
}

pub(super) fn byte_range_to_lsp(source: &str, range: std::ops::Range<usize>) -> Range {
    fn position(source: &str, offset: usize) -> Position {
        let prefix = &source[..offset.min(source.len())];
        let line = prefix.bytes().filter(|byte| *byte == b'\n').count();
        let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
        let character = prefix[line_start..].encode_utf16().count();
        Position::new(
            u32::try_from(line).unwrap_or(u32::MAX),
            u32::try_from(character).unwrap_or(u32::MAX),
        )
    }
    Range::new(position(source, range.start), position(source, range.end))
}

fn utf16_column(source: &str, line: usize, column: usize) -> u32 {
    let line = source
        .lines()
        .nth(line.saturating_sub(1))
        .unwrap_or_default();
    let units = line
        .chars()
        .take(column.saturating_sub(1))
        .map(char::len_utf16)
        .sum::<usize>();
    u32::try_from(units).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_zero_based_utf16_diagnostics() {
        let diagnostics = diagnostics("func main() {\n    \"😀\" @\n}\n");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(diagnostics[0].range.start, Position::new(1, 9));
    }

    #[test]
    fn valid_documents_have_no_diagnostics() {
        assert!(diagnostics("func main() -> Int { 42 }").is_empty());
    }

    #[test]
    fn publishes_compiler_warnings() {
        let diagnostics = diagnostics(
            "type Box { value: Int }\nfunc inspect(self: Box) -> Int [mut self] { self.value }",
        );
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].severity, Some(DiagnosticSeverity::WARNING));
        assert_eq!(
            diagnostics[0].code,
            Some(lsp_types::NumberOrString::String("unused-effect".into()))
        );
        assert_eq!(diagnostics[0].range.start, Position::new(1, 32));
        assert_eq!(diagnostics[0].range.end, Position::new(1, 40));
    }
}
