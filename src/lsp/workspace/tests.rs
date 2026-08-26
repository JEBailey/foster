use super::*;

fn fixture_workspace() -> (Workspace, Uri, PathBuf) {
    let root = std::env::current_dir()
        .unwrap()
        .join("tests/fixtures/modules");
    let main = root.join("main.fos");
    let uri = path_to_uri(&main).unwrap();
    (
        Workspace {
            root: Some(root.clone()),
            documents: HashMap::new(),
            published: HashSet::new(),
            compilations: Default::default(),
        },
        uri,
        root,
    )
}

#[test]
fn diagnostics_are_limited_to_workspace_and_explicitly_opened_sources() {
    let (mut workspace, _, root) = fixture_workspace();
    assert!(workspace.should_publish_diagnostics_for(&root.join("main.fos")));

    let external = root.parent().unwrap().join("external-library.fos");
    assert!(!workspace.should_publish_diagnostics_for(&external));

    workspace.open(path_to_uri(&external).unwrap(), String::new(), 1);
    assert!(workspace.should_publish_diagnostics_for(&external));
}

#[test]
fn document_symbols_use_open_buffer_overlays() {
    let (mut workspace, uri, root) = fixture_workspace();
    let mut source = std::fs::read_to_string(root.join("main.fos")).unwrap();
    source.push_str("\nfunc from_overlay() -> Int { 7 }\n");
    workspace.open(uri.clone(), source, 2);
    let Some(DocumentSymbolResponse::Nested(symbols)) = workspace.document_symbols(&uri) else {
        panic!("expected document symbols")
    };
    assert!(symbols.iter().any(|symbol| symbol.name == "from_overlay"));
}

#[test]
fn definition_resolves_an_imported_function_to_its_file() {
    let (workspace, uri, root) = fixture_workspace();
    let location = workspace
        .definition(&TextDocumentPositionParams::new(
            lsp_types::TextDocumentIdentifier::new(uri),
            Position::new(5, 13),
        ))
        .unwrap();
    assert_eq!(
        uri_to_path(&location.uri).unwrap(),
        root.join("json/parser.fos")
    );
    assert_eq!(location.range.start.line, 0);
}

#[test]
fn associated_function_navigation_uses_the_type_namespace() {
    let root = std::env::current_dir()
        .unwrap()
        .join("tests/fixtures/associated_function");
    let main = root.join("main.fos");
    let uri = path_to_uri(&main).unwrap();
    let workspace = Workspace {
        root: Some(root.clone()),
        documents: HashMap::new(),
        published: HashSet::new(),
        compilations: Default::default(),
    };

    let location = workspace
        .definition(&TextDocumentPositionParams::new(
            lsp_types::TextDocumentIdentifier::new(uri.clone()),
            Position::new(3, 18),
        ))
        .unwrap();
    assert_eq!(
        uri_to_path(&location.uri).unwrap(),
        root.join("collection.fos")
    );
    assert_eq!(location.range.start, Position::new(4, 13));
    assert_eq!(location.range.end, Position::new(4, 19));

    let references = workspace
        .references(&ReferenceParams {
            text_document_position: TextDocumentPositionParams::new(
                lsp_types::TextDocumentIdentifier::new(uri),
                Position::new(3, 18),
            ),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: lsp_types::ReferenceContext {
                include_declaration: true,
            },
        })
        .unwrap();
    assert_eq!(references.len(), 3);

    let completion = workspace
        .completion(&CompletionParams {
            text_document_position: TextDocumentPositionParams::new(
                lsp_types::TextDocumentIdentifier::new(
                    path_to_uri(&root.join("main.fos")).unwrap(),
                ),
                Position::new(3, 21),
            ),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .unwrap();
    let CompletionResponse::Array(items) = completion else {
        panic!("expected completion items")
    };
    assert!(items.iter().any(|item| item.label == "create"));
}

#[test]
fn hover_reports_inferred_local_and_function_types() {
    let (workspace, uri, _) = fixture_workspace();
    let local = workspace
        .hover(&TextDocumentPositionParams::new(
            lsp_types::TextDocumentIdentifier::new(uri.clone()),
            Position::new(5, 24),
        ))
        .unwrap();
    let HoverContents::Markup(local) = local.contents else {
        panic!("expected markdown hover")
    };
    assert!(local.value.contains("source: String"));

    let function = workspace
        .hover(&TextDocumentPositionParams::new(
            lsp_types::TextDocumentIdentifier::new(uri),
            Position::new(5, 13),
        ))
        .unwrap();
    let HoverContents::Markup(function) = function.contents else {
        panic!("expected markdown hover")
    };
    assert!(
        function
            .value
            .contains("func parse(input: consume String) -> String"),
        "{}",
        function.value
    );
}

#[test]
fn constants_have_symbols_hover_and_cross_module_definitions() {
    let root = std::env::current_dir()
        .unwrap()
        .join("tests/fixtures/constants");
    let main_uri = path_to_uri(&root.join("main.fos")).unwrap();
    let values_uri = path_to_uri(&root.join("values.fos")).unwrap();
    let workspace = Workspace {
        root: Some(root.clone()),
        documents: HashMap::new(),
        published: HashSet::new(),
        compilations: Default::default(),
    };

    let Some(DocumentSymbolResponse::Nested(symbols)) = workspace.document_symbols(&values_uri)
    else {
        panic!("expected document symbols")
    };
    assert!(
        symbols
            .iter()
            .any(|symbol| { symbol.name == "EXPORTED" && symbol.kind == SymbolKind::CONSTANT })
    );

    let hover = workspace
        .hover(&TextDocumentPositionParams::new(
            lsp_types::TextDocumentIdentifier::new(main_uri.clone()),
            Position::new(3, 6),
        ))
        .unwrap();
    let HoverContents::Markup(hover) = hover.contents else {
        panic!("expected markdown hover")
    };
    assert!(
        hover.value.contains("const EXPORTED: Int"),
        "{}",
        hover.value
    );
    assert!(hover.value.contains("public answer"), "{}", hover.value);

    let definition = workspace
        .definition(&TextDocumentPositionParams::new(
            lsp_types::TextDocumentIdentifier::new(main_uri),
            Position::new(3, 6),
        ))
        .unwrap();
    assert_eq!(
        uri_to_path(&definition.uri).unwrap(),
        root.join("values.fos")
    );
    assert_eq!(definition.range.start, Position::new(1, 10));
}

#[test]
fn hover_and_completion_publish_declaration_documentation() {
    let (mut workspace, uri, _) = fixture_workspace();
    let source = r#"/// Computes the documented answer.
func documented(value: Int) -> Int { value }

func main() -> Int {
    documented(42)
}
"#;
    workspace.open(uri.clone(), source.into(), 1);

    let hover = workspace
        .hover(&TextDocumentPositionParams::new(
            lsp_types::TextDocumentIdentifier::new(uri.clone()),
            Position::new(4, 8),
        ))
        .unwrap();
    let HoverContents::Markup(hover) = hover.contents else {
        panic!("expected markdown hover")
    };
    assert!(hover.value.contains("func documented(value: Int) -> Int"));
    assert!(hover.value.contains("Computes the documented answer."));
    assert_eq!(hover.value.matches("```foster").count(), 1);

    let completion = workspace
        .completion(&CompletionParams {
            text_document_position: TextDocumentPositionParams::new(
                lsp_types::TextDocumentIdentifier::new(uri),
                Position::new(4, 7),
            ),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .unwrap();
    let CompletionResponse::Array(items) = completion else {
        panic!("expected completion items")
    };
    let item = items
        .iter()
        .find(|item| item.label == "documented")
        .unwrap();
    let Some(Documentation::MarkupContent(documentation)) = &item.documentation else {
        panic!("expected markdown completion documentation")
    };
    assert_eq!(documentation.value, "Computes the documented answer.");
}

#[test]
fn completion_uses_scope_and_import_visibility() {
    let (workspace, uri, _) = fixture_workspace();
    let response = workspace
        .completion(&CompletionParams {
            text_document_position: TextDocumentPositionParams::new(
                lsp_types::TextDocumentIdentifier::new(uri.clone()),
                Position::new(5, 11),
            ),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .unwrap();
    let CompletionResponse::Array(items) = response else {
        panic!("expected completion items")
    };
    assert!(items.iter().any(|item| item.label == "parse"));

    let response = workspace
        .completion(&CompletionParams {
            text_document_position: TextDocumentPositionParams::new(
                lsp_types::TextDocumentIdentifier::new(uri),
                Position::new(5, 4),
            ),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .unwrap();
    let CompletionResponse::Array(items) = response else {
        panic!("expected completion items")
    };
    assert!(items.iter().any(|item| item.label == "source"));
    assert!(items.iter().any(|item| item.label == "branch"));
}

#[test]
fn arguments_completion_adds_the_required_import_when_compilation_fails() {
    let (mut workspace, uri, _) = fixture_workspace();
    let source = "func main(arguments: Argument) -> () {}\n";
    workspace.open(uri.clone(), source.into(), 7);

    let response = workspace
        .completion(&CompletionParams {
            text_document_position: TextDocumentPositionParams::new(
                lsp_types::TextDocumentIdentifier::new(uri),
                Position::new(0, 29),
            ),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .unwrap();
    let CompletionResponse::Array(items) = response else {
        panic!("expected completion items")
    };
    let arguments = items
        .iter()
        .find(|item| item.label == "Arguments")
        .expect("expected Arguments completion");
    assert_eq!(arguments.insert_text.as_deref(), Some("Arguments"));
    let edits = arguments
        .additional_text_edits
        .as_ref()
        .expect("expected an import edit");
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].range.start, Position::new(0, 0));
    assert_eq!(edits[0].new_text, "import std.process\n");
}

#[test]
fn references_follow_resolved_symbols_across_modules() {
    let (workspace, uri, root) = fixture_workspace();
    let references = workspace
        .references(&ReferenceParams {
            text_document_position: TextDocumentPositionParams::new(
                lsp_types::TextDocumentIdentifier::new(uri),
                Position::new(5, 13),
            ),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: lsp_types::ReferenceContext {
                include_declaration: true,
            },
        })
        .unwrap();
    assert_eq!(references.len(), 3);
    assert!(references.iter().any(|location| {
        uri_to_path(&location.uri).as_deref() == Some(root.join("json/parser.fos").as_path())
    }));
}

#[test]
fn rename_returns_edits_for_a_local_identity_only() {
    let (workspace, uri, _) = fixture_workspace();
    let edit = workspace
        .rename(&RenameParams {
            text_document_position: TextDocumentPositionParams::new(
                lsp_types::TextDocumentIdentifier::new(uri.clone()),
                Position::new(5, 24),
            ),
            new_name: "input".into(),
            work_done_progress_params: Default::default(),
        })
        .unwrap();
    let Some(DocumentChanges::Edits(documents)) = edit.document_changes else {
        panic!("expected versioned document edits")
    };
    let edits = documents
        .into_iter()
        .find(|document| document.text_document.uri == uri)
        .unwrap()
        .edits;
    assert_eq!(edits.len(), 2);
    assert!(edits.iter().all(|edit| match edit {
        OneOf::Left(edit) => edit.new_text == "input",
        OneOf::Right(edit) => edit.text_edit.new_text == "input",
    }));
}

#[test]
fn definition_uses_the_exact_nested_pattern_binding_range() {
    let (mut workspace, uri, _) = fixture_workspace();
    let source = r#"enum Choice = Some(String)
    | None

func select(value: Choice) -> String {
    branch value {
        Choice.Some(payload) -> payload
        Choice.None -> ""
    }
}
"#;
    workspace.open(uri.clone(), source.into(), 2);

    let location = workspace
        .definition(&TextDocumentPositionParams::new(
            lsp_types::TextDocumentIdentifier::new(uri.clone()),
            Position::new(5, 32),
        ))
        .unwrap();

    assert_eq!(location.uri, uri);
    assert_eq!(location.range.start, Position::new(5, 20));
    assert_eq!(location.range.end, Position::new(5, 27));
}

#[test]
fn union_hover_renders_complete_member_types() {
    let (mut workspace, uri, _) = fixture_workspace();
    let source = r#"type Value =
    String
    | List<Value>

func identity(value: Value) -> Value { value }
"#;
    workspace.open(uri.clone(), source.into(), 2);

    let hover = workspace
        .hover(&TextDocumentPositionParams::new(
            lsp_types::TextDocumentIdentifier::new(uri),
            Position::new(0, 6),
        ))
        .unwrap();
    let HoverContents::Markup(contents) = hover.contents else {
        panic!("expected markdown hover")
    };
    assert!(contents.value.contains("    String"), "{}", contents.value);
    assert!(
        contents.value.contains("| List<Value>"),
        "{}",
        contents.value
    );
    assert!(
        !contents.value.contains("List(Value)"),
        "{}",
        contents.value
    );
}

#[test]
fn enum_hover_renders_case_labels_and_payload_types() {
    let (mut workspace, uri, _) = fixture_workspace();
    let source = r#"enum Option<T> = Some(T)
    | None
"#;
    workspace.open(uri.clone(), source.into(), 2);

    let hover = workspace
        .hover(&TextDocumentPositionParams::new(
            lsp_types::TextDocumentIdentifier::new(uri),
            Position::new(0, 7),
        ))
        .unwrap();
    let HoverContents::Markup(contents) = hover.contents else {
        panic!("expected markdown hover")
    };
    assert!(
        contents.value.contains("enum Option<T>"),
        "{}",
        contents.value
    );
    assert!(contents.value.contains("Some(T)"), "{}", contents.value);
    assert!(contents.value.contains("| None"), "{}", contents.value);
}

#[test]
fn inlay_hints_report_inferred_types_and_argument_names() {
    let (mut workspace, uri, _) = fixture_workspace();
    let source = r#"/// Adds two values.
func add(left: Int, right: Int) -> Int { left + right }

func main() -> Int {
    let value = add(1, 2)
    value
}
"#;
    workspace.open(uri.clone(), source.into(), 1);
    let hints = workspace
        .inlay_hints(&InlayHintParams {
            work_done_progress_params: Default::default(),
            text_document: lsp_types::TextDocumentIdentifier::new(uri),
            range: lsp_types::Range::new(Position::new(0, 0), Position::new(6, 1)),
        })
        .unwrap();

    assert!(hints.iter().any(|hint| {
        matches!(&hint.label, lsp_types::InlayHintLabel::String(label) if label == ": Int")
            && hint.kind == Some(lsp_types::InlayHintKind::TYPE)
    }));
    for expected in ["left:", "right:"] {
        assert!(hints.iter().any(|hint| {
                matches!(&hint.label, lsp_types::InlayHintLabel::LabelParts(parts) if parts.iter().any(|part| part.value == expected && part.location.is_some()))
            }));
    }
}

#[test]
fn signature_help_selects_the_active_argument_and_shows_docs() {
    let (mut workspace, uri, _) = fixture_workspace();
    let source = r#"/// Adds two values.
func add(left: Int, right: Int) -> Int { left + right }

func main() -> Int {
    add(1, 2)
}
"#;
    workspace.open(uri.clone(), source.into(), 1);
    let help = workspace
        .signature_help(&SignatureHelpParams {
            context: None,
            text_document_position_params: TextDocumentPositionParams::new(
                lsp_types::TextDocumentIdentifier::new(uri),
                Position::new(4, 11),
            ),
            work_done_progress_params: Default::default(),
        })
        .unwrap();

    assert_eq!(help.active_parameter, Some(1));
    assert!(
        help.signatures[0]
            .label
            .contains("add(left: Int, right: Int) -> Int")
    );
    assert!(matches!(
        &help.signatures[0].documentation,
        Some(Documentation::MarkupContent(contents)) if contents.value.contains("Adds two values")
    ));
}

#[test]
fn callable_contract_members_provide_hover_signature_and_definition() {
    let (mut workspace, uri, _) = fixture_workspace();
    let source = r#"type Identified = {
    /// Adds an amount to this value.
    pub func offset(self, amount: Int) -> Int [read self]
}

type User = & Identified & { value: Int }
func offset(self: User, amount: Int) -> Int { self.value + amount }

func apply(value: Identified) -> Int {
    value.offset(2)
}
func main() -> Int { apply(User { value: 40 }) }
"#;
    workspace.open(uri.clone(), source.into(), 1);
    let position = TextDocumentPositionParams::new(
        lsp_types::TextDocumentIdentifier::new(uri.clone()),
        Position::new(9, 12),
    );

    let hover = workspace.hover(&position).unwrap();
    let HoverContents::Markup(contents) = hover.contents else {
        panic!("expected markdown hover")
    };
    assert!(
        contents
            .value
            .contains("func offset(self, amount: Int) -> Int"),
        "{}",
        contents.value
    );
    assert!(contents.value.contains("Adds an amount"));

    let location = workspace.definition(&position).unwrap();
    assert_eq!(location.range.start, Position::new(2, 13));

    let help = workspace
        .signature_help(&SignatureHelpParams {
            context: None,
            text_document_position_params: TextDocumentPositionParams::new(
                lsp_types::TextDocumentIdentifier::new(uri),
                Position::new(9, 19),
            ),
            work_done_progress_params: Default::default(),
        })
        .unwrap();
    assert!(
        help.signatures[0]
            .label
            .contains("offset(self, amount: Int)")
    );
    assert!(matches!(
        &help.signatures[0].documentation,
        Some(Documentation::MarkupContent(contents)) if contents.value.contains("Adds an amount")
    ));
}

#[test]
fn definition_resolves_instance_methods_from_receiver_types() {
    let root = std::env::current_dir()
        .unwrap()
        .join("tests/fixtures/associated_function");
    let uri = path_to_uri(&root.join("main.fos")).unwrap();
    let workspace = Workspace {
        root: Some(root.clone()),
        documents: HashMap::new(),
        published: HashSet::new(),
        compilations: Default::default(),
    };

    let location = workspace
        .definition(&TextDocumentPositionParams::new(
            lsp_types::TextDocumentIdentifier::new(uri),
            Position::new(5, 13),
        ))
        .unwrap();
    assert_eq!(
        uri_to_path(&location.uri).unwrap(),
        root.join("collection.fos")
    );
    assert_eq!(location.range.start, Position::new(8, 9));
}

#[test]
fn definition_opens_embedded_core_source_when_available() {
    let root = std::env::current_dir()
        .unwrap()
        .join("tests/fixtures/core_consumer");
    let uri = path_to_uri(&root.join("main.fos")).unwrap();
    let workspace = Workspace {
        root: Some(root),
        documents: HashMap::new(),
        published: HashSet::new(),
        compilations: Default::default(),
    };

    let location = workspace
        .definition(&TextDocumentPositionParams::new(
            lsp_types::TextDocumentIdentifier::new(uri),
            Position::new(11, 19),
        ))
        .unwrap();
    assert_eq!(
        uri_to_path(&location.uri).unwrap(),
        std::env::current_dir()
            .unwrap()
            .join("library/core/list.fos")
    );
    assert_eq!(location.range.start, Position::new(32, 9));
}

#[test]
fn examples_compile_in_their_own_document_context() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
    let example = root.join("examples/pima/repository_analyzer.fos");
    let uri = path_to_uri(&example).unwrap();
    let workspace = Workspace {
        root: Some(root.clone()),
        documents: HashMap::new(),
        published: HashSet::new(),
        compilations: Default::default(),
    };
    let position = TextDocumentPositionParams::new(
        lsp_types::TextDocumentIdentifier::new(uri),
        Position::new(42, 26),
    );

    let hover = workspace.hover(&position).unwrap();
    let HoverContents::Markup(contents) = hover.contents else {
        panic!("expected markdown hover")
    };
    assert!(contents.value.contains("func count"));
    assert!(contents.value.contains("Counts the sequence elements"));

    let location = workspace.definition(&position).unwrap();
    assert_eq!(
        uri_to_path(&location.uri).unwrap(),
        root.join("library/std/sequence.fos")
    );
    assert_eq!(location.range.start, Position::new(109, 9));
}

#[test]
fn intrinsics_provide_docs_navigation_and_parameter_hints() {
    let (mut workspace, uri, _) = fixture_workspace();
    let source = "func main() -> CodePoint {\n    from_code_point(65)\n}\n";
    workspace.open(uri.clone(), source.into(), 1);

    let hover = workspace
        .hover(&TextDocumentPositionParams::new(
            lsp_types::TextDocumentIdentifier::new(uri.clone()),
            Position::new(1, 10),
        ))
        .unwrap();
    let HoverContents::Markup(hover) = hover.contents else {
        panic!("expected markdown hover")
    };
    assert!(
        hover
            .value
            .contains("from_code_point(value: Int) -> CodePoint")
    );
    assert!(hover.value.contains("Unicode scalar value"));

    let location = workspace
        .definition(&TextDocumentPositionParams::new(
            lsp_types::TextDocumentIdentifier::new(uri.clone()),
            Position::new(1, 10),
        ))
        .unwrap();
    assert!(
        uri_to_path(&location.uri)
            .unwrap()
            .ends_with("docs/core-library.md")
    );

    let hints = workspace
        .inlay_hints(&InlayHintParams {
            work_done_progress_params: Default::default(),
            text_document: lsp_types::TextDocumentIdentifier::new(uri.clone()),
            range: lsp_types::Range::new(Position::new(0, 0), Position::new(2, 1)),
        })
        .unwrap();
    assert!(hints.iter().any(|hint| {
            matches!(&hint.label, lsp_types::InlayHintLabel::LabelParts(parts) if parts.iter().any(|part| part.value == "value:" && part.location.is_some()))
        }));

    let help = workspace
        .signature_help(&SignatureHelpParams {
            context: None,
            text_document_position_params: TextDocumentPositionParams::new(
                lsp_types::TextDocumentIdentifier::new(uri),
                Position::new(1, 21),
            ),
            work_done_progress_params: Default::default(),
        })
        .unwrap();
    assert_eq!(
        help.signatures[0].label,
        "from_code_point(value: Int) -> CodePoint"
    );
}

#[test]
fn record_signatures_show_declared_type_composition() {
    let compilation = crate::compile(
        r#"
type Named = { pub name: String }
type TextSlice = & Named & {}
func main() { 0 }
"#,
    )
    .unwrap();
    let module = compilation.hir.module_named("main").unwrap();
    let record = compilation.hir.record_named(module, "TextSlice").unwrap();
    let signature = record_signature(&compilation.hir.records[record]);
    assert!(
        signature.starts_with("type TextSlice = & Named"),
        "{signature}"
    );
}

#[test]
fn compilation_cache_reuses_snapshots_and_invalidates_on_change() {
    let (mut workspace, uri, root) = fixture_workspace();
    let first = workspace.compile_for(&uri).unwrap();
    let second = workspace.compile_for(&uri).unwrap();
    assert!(std::rc::Rc::ptr_eq(&first, &second));

    let source = std::fs::read_to_string(root.join("main.fos")).unwrap();
    workspace.change(uri.clone(), source, 2);
    let changed = workspace.compile_for(&uri).unwrap();
    assert!(!std::rc::Rc::ptr_eq(&first, &changed));
}

#[test]
fn compilation_uses_the_manifest_source_root() {
    let root = std::env::temp_dir().join(format!(
        "foster-lsp-project-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let source_root = root.join("source");
    std::fs::create_dir_all(&source_root).unwrap();
    std::fs::write(
        root.join("foster.toml"),
        "[package]\nname = \"lsp-project\"\nsource = \"source\"\n",
    )
    .unwrap();
    std::fs::write(
        source_root.join("helper.fos"),
        "pub func answer() -> Int { 42 }\n",
    )
    .unwrap();
    let main = source_root.join("main.fos");
    std::fs::write(&main, "import helper\nfunc main() -> Int { answer() }\n").unwrap();
    let uri = path_to_uri(&main).unwrap();
    let workspace = Workspace {
        root: Some(root.clone()),
        documents: HashMap::new(),
        published: HashSet::new(),
        compilations: Default::default(),
    };

    let compilation = workspace.compile_for(&uri).unwrap();
    assert_eq!(compilation.package.root.as_std_path(), source_root);
    assert!(compilation.package.module("helper").is_some());

    std::fs::remove_dir_all(root).unwrap();
}
