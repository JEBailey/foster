use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use lsp_server::{
    Connection, ErrorCode, Message, Notification, Request as ServerRequest, Response,
};
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
mod compilation;
mod hints;
mod snapshot;
mod workspace;
use workspace::Workspace;

const DIAGNOSTIC_DEBOUNCE: Duration = Duration::from_millis(150);

enum WorkspaceChange {
    Open(DidOpenTextDocumentParams),
    Change {
        uri: Uri,
        text: String,
        version: i32,
    },
    Close(DidCloseTextDocumentParams),
    Invalidate,
}

enum WorkerMessage {
    Change(WorkspaceChange),
    Request {
        tag: WorkTag,
        request: ServerRequest,
    },
    Diagnostics {
        tag: WorkTag,
    },
    Stop,
}

#[derive(Clone)]
struct WorkTag {
    generation: u64,
    documents: Vec<(Uri, i32)>,
}

struct WorkspaceWorker {
    sender: mpsc::Sender<WorkerMessage>,
    handle: thread::JoinHandle<()>,
}

impl WorkspaceWorker {
    fn start(
        initialize: InitializeParams,
        outgoing: crossbeam_channel::Sender<Message>,
        generation: Arc<AtomicU64>,
        cancelled: Arc<Mutex<HashSet<String>>>,
    ) -> Self {
        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let mut workspace = Workspace::new(&initialize);
            while let Ok(message) = receiver.recv() {
                match message {
                    WorkerMessage::Change(change) => apply_workspace_change(&mut workspace, change),
                    WorkerMessage::Request { tag, request } => {
                        let key = request_key(&request.id);
                        let response = if take_cancellation(&cancelled, &key) {
                            cancelled_response(request.id)
                        } else if !work_is_current(&workspace, &generation, &tag) {
                            content_modified_response(request.id)
                        } else {
                            let id = request.id.clone();
                            let response = handle_workspace_request(&workspace, request);
                            if take_cancellation(&cancelled, &key) {
                                cancelled_response(id)
                            } else if !work_is_current(&workspace, &generation, &tag) {
                                content_modified_response(id)
                            } else {
                                response
                            }
                        };
                        if outgoing.send(Message::Response(response)).is_err() {
                            break;
                        }
                    }
                    WorkerMessage::Diagnostics { tag } => {
                        if work_is_current(&workspace, &generation, &tag)
                            && let Err(error) = workspace.publish_diagnostics(
                                &outgoing,
                                tag.generation,
                                &generation,
                            )
                        {
                            eprintln!("Foster language server diagnostic error: {error}");
                        }
                    }
                    WorkerMessage::Stop => break,
                }
            }
        });
        Self { sender, handle }
    }

    fn send(&self, message: WorkerMessage) -> Result<(), Box<dyn Error>> {
        self.sender.send(message)?;
        Ok(())
    }

    fn stop(self) -> Result<(), Box<dyn Error>> {
        let _ = self.sender.send(WorkerMessage::Stop);
        self.handle
            .join()
            .map_err(|_| "Foster language server workspace worker panicked".into())
    }
}

fn work_is_current(workspace: &Workspace, generation: &AtomicU64, tag: &WorkTag) -> bool {
    generation.load(Ordering::Acquire) == tag.generation
        && tag
            .documents
            .iter()
            .all(|(uri, version)| workspace.version(uri) == Some(*version))
}

fn request_work_tag(
    request: &ServerRequest,
    generation: u64,
    versions: &HashMap<String, (Uri, i32)>,
) -> WorkTag {
    let documents = request
        .params
        .pointer("/textDocument/uri")
        .and_then(serde_json::Value::as_str)
        .and_then(|uri| versions.get(uri))
        .cloned()
        .into_iter()
        .collect();
    WorkTag {
        generation,
        documents,
    }
}

fn apply_workspace_change(workspace: &mut Workspace, change: WorkspaceChange) {
    match change {
        WorkspaceChange::Open(params) => workspace.open(
            params.text_document.uri,
            params.text_document.text,
            params.text_document.version,
        ),
        WorkspaceChange::Change { uri, text, version } => workspace.change(uri, text, version),
        WorkspaceChange::Close(params) => workspace.close(&params.text_document.uri),
        WorkspaceChange::Invalidate => workspace.invalidate_compilations(),
    }
}

fn handle_workspace_request(workspace: &Workspace, request: ServerRequest) -> Response {
    let id = request.id;
    match request.method.as_str() {
        DocumentSymbolRequest::METHOD => {
            respond(id, request.params, |params: DocumentSymbolParams| {
                workspace.document_symbols(&params.text_document.uri)
            })
        }
        GotoDefinition::METHOD => respond(id, request.params, |params: GotoDefinitionParams| {
            workspace.definition(&params.text_document_position_params)
        }),
        HoverRequest::METHOD => respond(id, request.params, |params: HoverParams| {
            workspace.hover(&params.text_document_position_params)
        }),
        Completion::METHOD => respond(id, request.params, |params: CompletionParams| {
            workspace.completion(&params)
        }),
        SignatureHelpRequest::METHOD => {
            respond(id, request.params, |params: SignatureHelpParams| {
                workspace.signature_help(&params)
            })
        }
        InlayHintRequest::METHOD => respond(id, request.params, |params: InlayHintParams| {
            workspace.inlay_hints(&params)
        }),
        References::METHOD => respond(id, request.params, |params: ReferenceParams| {
            workspace.references(&params)
        }),
        Rename::METHOD => respond(id, request.params, |params: RenameParams| {
            workspace.rename(&params)
        }),
        _ => Response::new_err(
            id,
            ErrorCode::MethodNotFound as i32,
            format!("unsupported request `{}`", request.method),
        ),
    }
}

fn respond<P, R>(
    id: lsp_server::RequestId,
    params: serde_json::Value,
    handler: impl FnOnce(P) -> R,
) -> Response
where
    P: serde::de::DeserializeOwned,
    R: serde::Serialize,
{
    match serde_json::from_value(params) {
        Ok(params) => Response::new_ok(id, handler(params)),
        Err(error) => Response::new_err(
            id,
            ErrorCode::InvalidParams as i32,
            format!("invalid request parameters: {error}"),
        ),
    }
}

fn request_key(id: &lsp_server::RequestId) -> String {
    serde_json::to_string(id).unwrap_or_else(|_| format!("{id:?}"))
}

fn take_cancellation(cancelled: &Mutex<HashSet<String>>, key: &str) -> bool {
    cancelled.lock().is_ok_and(|mut values| values.remove(key))
}

fn cancelled_response(id: lsp_server::RequestId) -> Response {
    Response::new_err(
        id,
        ErrorCode::RequestCanceled as i32,
        "request cancelled".into(),
    )
}

fn content_modified_response(id: lsp_server::RequestId) -> Response {
    Response::new_err(
        id,
        ErrorCode::ContentModified as i32,
        "document changed while the request was running".into(),
    )
}

#[derive(Default)]
struct DiagnosticSchedule {
    deadline: Option<Instant>,
}

impl DiagnosticSchedule {
    fn postpone(&mut self, now: Instant) {
        self.deadline = Some(now + DIAGNOSTIC_DEBOUNCE);
    }

    fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    fn clear(&mut self) {
        self.deadline = None;
    }
}

#[derive(Default)]
struct ShutdownState {
    requested: bool,
}

impl ShutdownState {
    fn handle_request(&mut self, request: &lsp_server::Request) -> Option<Response> {
        if request.method != "shutdown" {
            return None;
        }
        let response = if self.requested {
            Response::new_err(
                request.id.clone(),
                ErrorCode::InvalidRequest as i32,
                "the Foster language server is already shutting down".into(),
            )
        } else {
            self.requested = true;
            Response::new_ok(request.id.clone(), ())
        };
        Some(response)
    }

    fn is_requested(&self) -> bool {
        self.requested
    }

    fn should_exit(&self, notification: &Notification) -> bool {
        notification.method == "exit"
    }
}

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
    let generation = Arc::new(AtomicU64::new(0));
    let cancelled = Arc::new(Mutex::new(HashSet::new()));
    let worker = WorkspaceWorker::start(
        initialize_params,
        connection.sender.clone(),
        Arc::clone(&generation),
        Arc::clone(&cancelled),
    );

    let mut diagnostics = DiagnosticSchedule::default();
    let mut shutdown = ShutdownState::default();
    let mut document_versions = HashMap::<String, (Uri, i32)>::new();
    loop {
        let message = if let Some(deadline) = diagnostics.deadline() {
            match connection
                .receiver
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(message) => message,
                Err(error) if error.is_timeout() => {
                    worker.send(WorkerMessage::Diagnostics {
                        tag: WorkTag {
                            generation: generation.load(Ordering::Acquire),
                            documents: document_versions.values().cloned().collect(),
                        },
                    })?;
                    diagnostics.clear();
                    continue;
                }
                Err(_) => break,
            }
        } else {
            match connection.receiver.recv() {
                Ok(message) => message,
                Err(_) => break,
            }
        };
        match message {
            Message::Request(request) => {
                if let Some(response) = shutdown.handle_request(&request) {
                    diagnostics.clear();
                    connection.sender.send(Message::Response(response))?;
                    continue;
                }
                if shutdown.is_requested() {
                    connection.sender.send(Message::Response(Response::new_err(
                        request.id,
                        ErrorCode::InvalidRequest as i32,
                        "the Foster language server is shutting down".into(),
                    )))?;
                    continue;
                }
                let tag = request_work_tag(
                    &request,
                    generation.load(Ordering::Acquire),
                    &document_versions,
                );
                worker.send(WorkerMessage::Request { tag, request })?;
            }
            Message::Notification(notification) => {
                if shutdown.should_exit(&notification) {
                    break;
                }
                if shutdown.is_requested() {
                    continue;
                }
                match notification.method.as_str() {
                    "$/cancelRequest" => {
                        if let Some(id) = notification.params.get("id")
                            && let Ok(mut requests) = cancelled.lock()
                        {
                            requests.insert(id.to_string());
                        }
                    }
                    "textDocument/didOpen" => {
                        let params: DidOpenTextDocumentParams =
                            serde_json::from_value(notification.params)?;
                        document_versions.insert(
                            params.text_document.uri.as_str().to_owned(),
                            (
                                params.text_document.uri.clone(),
                                params.text_document.version,
                            ),
                        );
                        generation.fetch_add(1, Ordering::AcqRel);
                        worker.send(WorkerMessage::Change(WorkspaceChange::Open(params)))?;
                        diagnostics.postpone(Instant::now());
                    }
                    "textDocument/didChange" => {
                        let params: DidChangeTextDocumentParams =
                            serde_json::from_value(notification.params)?;
                        let Some(change) = params.content_changes.into_iter().last() else {
                            continue;
                        };
                        document_versions.insert(
                            params.text_document.uri.as_str().to_owned(),
                            (
                                params.text_document.uri.clone(),
                                params.text_document.version,
                            ),
                        );
                        generation.fetch_add(1, Ordering::AcqRel);
                        worker.send(WorkerMessage::Change(WorkspaceChange::Change {
                            uri: params.text_document.uri,
                            text: change.text,
                            version: params.text_document.version,
                        }))?;
                        diagnostics.postpone(Instant::now());
                    }
                    "textDocument/didClose" => {
                        let params: DidCloseTextDocumentParams =
                            serde_json::from_value(notification.params)?;
                        document_versions.remove(params.text_document.uri.as_str());
                        generation.fetch_add(1, Ordering::AcqRel);
                        worker.send(WorkerMessage::Change(WorkspaceChange::Close(params)))?;
                        diagnostics.postpone(Instant::now());
                    }
                    "workspace/didChangeWatchedFiles" => {
                        generation.fetch_add(1, Ordering::AcqRel);
                        worker.send(WorkerMessage::Change(WorkspaceChange::Invalidate))?;
                        diagnostics.postpone(Instant::now());
                    }
                    _ => {}
                }
            }
            Message::Response(_) => {}
        }
    }
    worker.stop()?;
    drop(connection);
    io_threads.join()?;
    Ok(())
}

#[cfg(test)]
mod scheduling_tests {
    use super::*;

    fn test_uri() -> Uri {
        "file:///foster-worker-test.fos".parse().unwrap()
    }

    #[test]
    fn repeated_changes_postpone_diagnostics_until_after_the_latest_change() {
        let start = Instant::now();
        let mut diagnostics = DiagnosticSchedule::default();
        diagnostics.postpone(start);
        assert_eq!(diagnostics.deadline(), Some(start + DIAGNOSTIC_DEBOUNCE));

        let later = start + Duration::from_millis(100);
        diagnostics.postpone(later);
        assert_eq!(diagnostics.deadline(), Some(later + DIAGNOSTIC_DEBOUNCE));

        diagnostics.clear();
        assert_eq!(diagnostics.deadline(), None);
    }

    #[test]
    fn shutdown_allows_cancellation_notifications_before_exit() {
        let mut shutdown = ShutdownState::default();
        let request = lsp_server::Request {
            id: 1.into(),
            method: "shutdown".into(),
            params: serde_json::Value::Null,
        };
        let response = shutdown.handle_request(&request).unwrap();
        assert!(response.response_result.is_ok());
        assert!(shutdown.is_requested());

        let cancellation =
            Notification::new("$/cancelRequest".into(), serde_json::json!({ "id": 2 }));
        assert!(!shutdown.should_exit(&cancellation));

        let exit = Notification::new("exit".into(), serde_json::Value::Null);
        assert!(shutdown.should_exit(&exit));
    }

    #[test]
    fn work_tags_require_the_current_generation_and_document_version() {
        let mut workspace = Workspace::new(&InitializeParams::default());
        let uri = test_uri();
        workspace.open(uri.clone(), "func main() { 0 }\n".into(), 4);
        let generation = AtomicU64::new(7);
        let current = WorkTag {
            generation: 7,
            documents: vec![(uri.clone(), 4)],
        };
        assert!(work_is_current(&workspace, &generation, &current));

        let stale_generation = WorkTag {
            generation: 6,
            documents: vec![(uri.clone(), 4)],
        };
        assert!(!work_is_current(&workspace, &generation, &stale_generation));

        let stale_document = WorkTag {
            generation: 7,
            documents: vec![(uri, 3)],
        };
        assert!(!work_is_current(&workspace, &generation, &stale_document));
    }

    #[test]
    fn workspace_worker_supersedes_stale_and_cancelled_requests() {
        let (outgoing, responses) = crossbeam_channel::unbounded();
        let generation = Arc::new(AtomicU64::new(2));
        let cancelled = Arc::new(Mutex::new(HashSet::from(["2".into()])));
        let worker = WorkspaceWorker::start(
            InitializeParams::default(),
            outgoing,
            Arc::clone(&generation),
            Arc::clone(&cancelled),
        );
        let uri = test_uri();
        worker
            .send(WorkerMessage::Change(WorkspaceChange::Open(
                DidOpenTextDocumentParams {
                    text_document: lsp_types::TextDocumentItem::new(
                        uri.clone(),
                        "foster".into(),
                        1,
                        "func main() { 0 }\n".into(),
                    ),
                },
            )))
            .unwrap();
        for (id, expected_generation, expected_code) in [
            (1_i32, 1_u64, ErrorCode::ContentModified as i32),
            (2, 2, ErrorCode::RequestCanceled as i32),
        ] {
            worker
                .send(WorkerMessage::Request {
                    tag: WorkTag {
                        generation: expected_generation,
                        documents: vec![(uri.clone(), 1)],
                    },
                    request: ServerRequest {
                        id: id.into(),
                        method: HoverRequest::METHOD.into(),
                        params: serde_json::json!({
                            "textDocument": { "uri": uri },
                            "position": { "line": 0, "character": 5 }
                        }),
                    },
                })
                .unwrap();
            let Message::Response(response) =
                responses.recv_timeout(Duration::from_secs(5)).unwrap()
            else {
                panic!("expected a response")
            };
            assert_eq!(response.response_result.unwrap_err().code, expected_code);
        }

        worker
            .send(WorkerMessage::Request {
                tag: WorkTag {
                    generation: 2,
                    documents: vec![(uri.clone(), 1)],
                },
                request: ServerRequest {
                    id: 3.into(),
                    method: HoverRequest::METHOD.into(),
                    params: serde_json::json!({
                        "textDocument": { "uri": uri },
                        "position": { "line": 0, "character": 5 }
                    }),
                },
            })
            .unwrap();
        let Message::Response(response) = responses.recv_timeout(Duration::from_secs(5)).unwrap()
        else {
            panic!("expected a response")
        };
        assert!(response.response_result.is_ok());
        worker.stop().unwrap();
    }
}

pub(super) fn publish(
    sender: &crossbeam_channel::Sender<Message>,
    uri: Uri,
    diagnostics: Vec<Diagnostic>,
    version: Option<i32>,
) -> Result<(), Box<dyn Error>> {
    let params = PublishDiagnosticsParams {
        uri,
        diagnostics,
        version,
    };
    sender.send(Message::Notification(Notification::new(
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
    let compiler = crate::diagnostic::Diagnostic::from_source_error(source, &error);
    if !compiler.labels.is_empty() || compiler.code.is_some() {
        let range = compiler
            .labels
            .iter()
            .find(|label| label.primary)
            .or_else(|| compiler.labels.first())
            .map_or_else(
                || Range::new(Position::new(0, 0), Position::new(0, 1)),
                |label| byte_range_to_lsp(source, label.range.clone()),
            );
        let mut message = compiler.message;
        for label in compiler.labels.iter().filter(|label| !label.primary) {
            message.push_str("\n\n");
            message.push_str(&label.message);
        }
        for note in compiler.notes {
            message.push_str("\n\nnote: ");
            message.push_str(&note);
        }
        if let Some(help) = compiler.help {
            message.push_str("\n\nhelp: ");
            message.push_str(&help);
        }
        return Diagnostic {
            range,
            severity: Some(DiagnosticSeverity::ERROR),
            code: compiler.code.map(lsp_types::NumberOrString::String),
            code_description: None,
            source: Some("foster".into()),
            message,
            related_information: None,
            tags: None,
            data: None,
        };
    }
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
    fn call_argument_type_errors_point_to_the_argument() {
        let source = "func takes(value: Float) -> Float { value }\nfunc main() -> Float {\n    takes(12)\n}\n";
        let diagnostics = diagnostics(source);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(diagnostics[0].range.start, Position::new(2, 10));
        assert_eq!(diagnostics[0].range.end, Position::new(2, 12));
    }

    #[test]
    fn consuming_call_errors_point_to_the_argument() {
        let source = "type Box = { value: String }\nfunc take(value: Box) -> () [consume value] {}\nfunc main() {\n    let value = Box { value: \"kept\" }\n    take(value)\n}\n";
        let diagnostics = diagnostics(source);
        assert_eq!(diagnostics.len(), 1);
        assert!(
            diagnostics[0]
                .message
                .contains("pass this argument with `move`")
        );
        assert_eq!(diagnostics[0].range.start, Position::new(4, 9));
        assert_eq!(diagnostics[0].range.end, Position::new(4, 14));
    }

    #[test]
    fn unknown_parameter_types_point_to_the_annotation() {
        let source = "func main(arguments: Argument) -> () {}\n";
        let diagnostics = diagnostics(source);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(diagnostics[0].range.start, Position::new(0, 21));
        assert_eq!(diagnostics[0].range.end, Position::new(0, 29));
    }

    #[test]
    fn missing_module_members_point_to_the_member_expression() {
        let source = "import core.float\nfunc main() -> String { float::missing(1.0) }\n";
        let diagnostics = diagnostics(source);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].severity, Some(DiagnosticSeverity::ERROR));
        assert!(
            diagnostics[0]
                .message
                .contains("module `core.float` has no member `missing`")
        );
        assert_eq!(diagnostics[0].range.start, Position::new(1, 24));
        assert_eq!(diagnostics[0].range.end, Position::new(1, 38));
    }

    #[test]
    fn duplicate_enum_case_names_point_to_the_conflicting_case() {
        let source = "enum Value =\nItem(Int)\n| Item(String)\n";
        let diagnostics = diagnostics(source);
        assert_eq!(diagnostics.len(), 1);
        assert!(
            diagnostics[0]
                .message
                .contains("enum `Value` declares `Item` more than once")
        );
        assert_eq!(diagnostics[0].range.start, Position::new(2, 2));
        assert_eq!(diagnostics[0].range.end, Position::new(2, 14));
    }

    #[test]
    fn duplicate_payloadless_enum_cases_point_to_the_second_case() {
        let source = "enum Value =\nReady\n| Ready\n";
        let diagnostics = diagnostics(source);
        assert_eq!(diagnostics.len(), 1);
        assert!(
            diagnostics[0]
                .message
                .contains("enum `Value` declares `Ready` more than once")
        );
        assert_eq!(diagnostics[0].range.start, Position::new(2, 2));
        assert_eq!(diagnostics[0].range.end, Position::new(2, 7));
    }

    #[test]
    fn every_compile_error_has_a_source_location() {
        let sources = [
            "func main() { missing() }",
            "func main(value: Missing) {}",
            "func main() -> Float { 12 }",
            "func main(value: Int) {}",
            "func broken<T, T>(value: T) { value }",
            "type Pair = { left: Int, left: Int }",
            "func main[g: group Int](value: ref[missing] Int) { value }",
        ];

        for source in sources {
            let error = crate::compile(source).expect_err(source);
            assert!(
                error.has_source_location(),
                "unlocated error for `{source}`: {}",
                error.message
            );
            assert!(
                error.labels.iter().any(|label| label.primary) || error.line > 0,
                "error has no primary location for `{source}`: {}",
                error.message
            );
        }
    }

    #[test]
    fn publishes_compiler_warnings() {
        let diagnostics = diagnostics(
            "type Box = { value: Int }\nfunc Box.inspect(self: Box) -> Int [mut self] { self.value }",
        );
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].severity, Some(DiagnosticSeverity::WARNING));
        assert_eq!(
            diagnostics[0].code,
            Some(lsp_types::NumberOrString::String("unused-effect".into()))
        );
        assert_eq!(diagnostics[0].range.start, Position::new(1, 36));
        assert_eq!(diagnostics[0].range.end, Position::new(1, 44));
    }
}
