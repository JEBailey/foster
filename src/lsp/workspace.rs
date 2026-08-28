use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use lsp_server::Connection;
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionParams, CompletionResponse, Diagnostic,
    DiagnosticSeverity, DocumentChanges, DocumentSymbol, DocumentSymbolResponse, Documentation,
    Hover, HoverContents, InitializeParams, InlayHint, InlayHintParams, Location, MarkupContent,
    MarkupKind, NumberOrString, OneOf, OptionalVersionedTextDocumentIdentifier, Position,
    ReferenceParams, RenameParams, SignatureHelp, SignatureHelpParams, SymbolKind,
    TextDocumentEdit, TextDocumentPositionParams, TextEdit, Uri, WorkspaceEdit,
};

use super::{byte_range_to_lsp, error_diagnostic, publish};

#[derive(Debug, Clone)]
pub(super) struct OpenDocument {
    pub(super) text: String,
    version: i32,
}

pub(super) struct Workspace {
    pub(super) root: Option<PathBuf>,
    pub(super) documents: HashMap<Uri, OpenDocument>,
    published: HashSet<Uri>,
    pub(super) compilations: super::compilation::CompilationCache,
}

impl Workspace {
    pub(super) fn new(params: &InitializeParams) -> Self {
        #[allow(deprecated)]
        let root = params
            .workspace_folders
            .as_ref()
            .and_then(|folders| folders.first())
            .and_then(|folder| uri_to_path(&folder.uri))
            .or_else(|| params.root_uri.as_ref().and_then(uri_to_path));
        Self {
            root,
            documents: HashMap::new(),
            published: HashSet::new(),
            compilations: super::compilation::CompilationCache::default(),
        }
    }

    pub(super) fn open(&mut self, uri: Uri, text: String, version: i32) {
        self.compilations.invalidate(&uri);
        self.documents.insert(uri, OpenDocument { text, version });
    }

    pub(super) fn change(&mut self, uri: Uri, text: String, version: i32) {
        self.open(uri, text, version);
    }

    pub(super) fn close(&mut self, uri: &Uri) {
        self.compilations.invalidate(uri);
        self.documents.remove(uri);
    }

    pub(super) fn invalidate_compilations(&self) {
        self.compilations.clear();
    }

    pub(super) fn publish_diagnostics(
        &mut self,
        connection: &Connection,
    ) -> Result<(), Box<dyn Error>> {
        let mut next_by_uri = HashMap::<String, (Uri, Vec<Diagnostic>, Option<i32>)>::new();
        for (focus_uri, document) in &self.documents {
            match self.compile_for(focus_uri) {
                Ok(compilation) => {
                    for (_, module) in compilation.hir.modules.iter() {
                        let Some(path) = module.source_path.as_deref() else {
                            continue;
                        };
                        if !self.should_publish_diagnostics_for(path.as_std_path()) {
                            continue;
                        }
                        let Some(uri) = path_to_uri(path.as_std_path()) else {
                            continue;
                        };
                        let source = compilation
                            .package
                            .module(&module.name)
                            .and_then(|module| module.source.as_deref())
                            .unwrap_or_default();
                        let diagnostics = compilation
                            .diagnostics
                            .iter()
                            .filter(|diagnostic| {
                                diagnostic.source_module.as_deref() == Some(&module.name)
                            })
                            .map(|diagnostic| compiler_diagnostic(source, diagnostic))
                            .collect();
                        next_by_uri.insert(
                            uri.as_str().to_owned(),
                            (uri.clone(), diagnostics, self.version(&uri)),
                        );
                    }
                }
                Err(error) => {
                    next_by_uri.insert(
                        focus_uri.as_str().to_owned(),
                        (
                            focus_uri.clone(),
                            vec![error_diagnostic(&document.text, error)],
                            Some(document.version),
                        ),
                    );
                }
            }
        }
        let next = next_by_uri.into_values().collect::<Vec<_>>();

        let next_uris = next
            .iter()
            .map(|(uri, _, _)| uri.as_str().to_owned())
            .collect::<HashSet<_>>();
        for uri in self
            .published
            .iter()
            .filter(|uri| !next_uris.contains(uri.as_str()))
        {
            publish(connection, uri.clone(), Vec::new(), self.version(uri))?;
        }
        for (uri, diagnostics, version) in &next {
            publish(connection, uri.clone(), diagnostics.clone(), *version)?;
        }
        self.published = next.into_iter().map(|(uri, _, _)| uri).collect();
        Ok(())
    }

    fn should_publish_diagnostics_for(&self, path: &Path) -> bool {
        self.root
            .as_deref()
            .is_some_and(|root| path.starts_with(root))
            || self
                .documents
                .keys()
                .filter_map(uri_to_path)
                .any(|document| document == path)
    }

    pub(super) fn document_symbols(&self, uri: &Uri) -> Option<DocumentSymbolResponse> {
        let compilation = self.semantic_compilation_for(uri)?;
        let module_id = module_for_uri(&compilation, uri)?;
        let module = &compilation.hir.modules[module_id];
        let source = compilation
            .package
            .module(&module.name)?
            .source
            .as_deref()?;
        let mut symbols = Vec::new();
        for (_, constant) in compilation
            .hir
            .constants
            .iter()
            .filter(|(_, constant)| constant.module == module_id)
        {
            symbols.push(symbol(
                &constant.name,
                SymbolKind::CONSTANT,
                source,
                constant.span.clone(),
            ));
        }
        for (_, record) in compilation
            .hir
            .records
            .iter()
            .filter(|(_, record)| record.module == module_id)
        {
            symbols.push(symbol(
                &record.name,
                SymbolKind::STRUCT,
                source,
                record.span.clone(),
            ));
        }
        for (_, variant) in compilation
            .hir
            .variant_types
            .iter()
            .filter(|(_, variant)| variant.module == module_id)
        {
            symbols.push(symbol(
                &variant.name,
                if variant.kind == crate::ast::VariantKind::Enum {
                    SymbolKind::ENUM
                } else {
                    SymbolKind::INTERFACE
                },
                source,
                variant.span.clone(),
            ));
        }
        for (_, function) in
            compilation.hir.functions.iter().filter(|(_, function)| {
                function.module == module_id && !function.name.contains('$')
            })
        {
            let name = source_function_name(function);
            symbols.push(symbol(
                &name,
                SymbolKind::FUNCTION,
                source,
                function.span.clone(),
            ));
        }
        Some(DocumentSymbolResponse::Nested(symbols))
    }

    pub(super) fn definition(&self, params: &TextDocumentPositionParams) -> Option<Location> {
        let compilation = self.semantic_compilation_for(&params.text_document.uri)?;
        let module_id = module_for_uri(&compilation, &params.text_document.uri)?;
        let module = &compilation.hir.modules[module_id];
        let source = compilation
            .package
            .module(&module.name)?
            .source
            .as_deref()?;
        let offset = position_to_offset(source, params.position)?;
        if let Some(symbol) = symbol_at(&compilation, module_id, source, offset)
            && let Some(location) = symbol_declaration(&compilation, symbol)
        {
            return Some(location);
        }
        let (name, name_start) = identifier_at(source, offset)?;
        let qualifier = qualifier_before(source, name_start);

        if qualifier.is_none() && name == "CodePoint" {
            return embedded_module_location("core.code_point");
        }

        if qualifier.is_none()
            && let Some((function_id, function)) =
                compilation.hir.functions.iter().find(|(_, function)| {
                    function.module == module_id
                        && function.span.start <= offset
                        && offset <= function.span.end
                })
            && let Some((_, local)) = compilation
                .hir
                .locals
                .iter()
                .find(|(_, local)| local.function == function_id && local.name == name)
        {
            let span = find_name(source, function.span.clone(), &local.name)
                .unwrap_or_else(|| local.span.clone());
            return Some(Location::new(
                params.text_document.uri.clone(),
                byte_range_to_lsp(source, span),
            ));
        }

        let target_module = qualifier
            .as_deref()
            .and_then(|qualifier| module.imports.get(qualifier).copied())
            .unwrap_or(module_id);
        if qualifier.is_none()
            && let Some(import) = module
                .imports_with_spans
                .iter()
                .find(|import| import.name == name)
        {
            return module_location(&compilation, import.target);
        }
        definition_in_module(&compilation, target_module, name).or_else(|| {
            module
                .imports
                .values()
                .find_map(|imported| definition_in_module(&compilation, *imported, name))
        })
    }

    pub(super) fn hover(&self, params: &TextDocumentPositionParams) -> Option<Hover> {
        let compilation = self.semantic_compilation_for(&params.text_document.uri)?;
        let module_id = module_for_uri(&compilation, &params.text_document.uri)?;
        let module = &compilation.hir.modules[module_id];
        let source = compilation
            .package
            .module(&module.name)?
            .source
            .as_deref()?;
        let offset = position_to_offset(source, params.position)?;
        let (name, start) = identifier_at(source, offset)?;
        let range = byte_range_to_lsp(source, start..start + name.len());
        let qualifier = qualifier_before(source, start);

        let value = if qualifier.is_none()
            && let Some(function_id) = function_at(&compilation, module_id, offset)
            && let Some((local_id, _)) = compilation
                .hir
                .locals
                .iter()
                .find(|(_, local)| local.function == function_id && local.name == name)
        {
            let ty = compilation.types.local_type(local_id)?;
            documented_hover(format!("{}: {}", name, compilation.types.display(ty)), None)
        } else if let Some(symbol) = symbol_at(&compilation, module_id, source, offset)
            && let Some(value) = symbol_hover(&compilation, symbol)
        {
            value
        } else {
            let target = qualifier
                .as_deref()
                .and_then(|qualifier| module.imports.get(qualifier).copied())
                .unwrap_or(module_id);
            declaration_hover(&compilation, target, name).or_else(|| {
                (qualifier.is_none()).then(|| {
                    module
                        .imports
                        .values()
                        .find_map(|imported| declaration_hover(&compilation, *imported, name))
                })?
            })?
        };

        Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value,
            }),
            range: Some(range),
        })
    }

    pub(super) fn signature_help(&self, params: &SignatureHelpParams) -> Option<SignatureHelp> {
        let compilation =
            self.semantic_compilation_for(&params.text_document_position_params.text_document.uri)?;
        super::hints::signature_help(&compilation, params)
    }

    pub(super) fn inlay_hints(&self, params: &InlayHintParams) -> Option<Vec<InlayHint>> {
        match self.compile_for(&params.text_document.uri) {
            Ok(compilation) => super::hints::inlay_hints(&compilation, params),
            Err(_) => {
                let compilation = self.semantic_compilation_for(&params.text_document.uri)?;
                let current = &self.documents.get(&params.text_document.uri)?.text;
                super::hints::inlay_hints_for_unchanged_functions(&compilation, params, current)
            }
        }
    }

    pub(super) fn completion(&self, params: &CompletionParams) -> Option<CompletionResponse> {
        let position = &params.text_document_position;
        let mut items = std::collections::BTreeMap::<String, CompletionItem>::new();
        if let Some(document) = self.documents.get(&position.text_document.uri)
            && let Some(offset) = position_to_offset(&document.text, position.position)
        {
            add_arguments_auto_import_completion(&document.text, offset, &mut items);
        }
        let compilation = match self.semantic_compilation_for(&position.text_document.uri) {
            Some(compilation) => compilation,
            None => {
                return (!items.is_empty())
                    .then(|| CompletionResponse::Array(items.into_values().collect()));
            }
        };
        let module_id = module_for_uri(&compilation, &position.text_document.uri)?;
        let module = &compilation.hir.modules[module_id];
        let source = compilation
            .package
            .module(&module.name)?
            .source
            .as_deref()?;
        let offset = position_to_offset(source, position.position)?;
        let start = identifier_at(source, offset).map_or(offset, |(_, start)| start);
        let qualifier = qualifier_before(source, start);

        if let Some(qualifier) = qualifier {
            if !add_associated_completions(&compilation, module_id, &qualifier, &mut items)
                && let Some(target) = module.imports.get(&qualifier)
            {
                add_module_completions(&compilation, *target, true, &mut items);
            }
        } else {
            if let Some(function) = function_at(&compilation, module_id, offset) {
                for (local, definition) in compilation
                    .hir
                    .locals
                    .iter()
                    .filter(|(_, local)| local.function == function)
                {
                    let detail = compilation
                        .types
                        .local_type(local)
                        .map(|ty| compilation.types.display(ty));
                    insert_completion(
                        &mut items,
                        &definition.name,
                        CompletionItemKind::VARIABLE,
                        detail,
                    );
                }
            }
            add_module_completions(&compilation, module_id, false, &mut items);
            for imported in module.imports.values() {
                add_module_completions(&compilation, *imported, true, &mut items);
            }
            for import in &module.imports_with_spans {
                insert_completion(
                    &mut items,
                    &import.name,
                    CompletionItemKind::MODULE,
                    Some(compilation.hir.modules[import.target].name.clone()),
                );
            }
            for keyword in [
                "assert", "await", "branch", "break", "continue", "copy", "false", "func",
                "import", "let", "loop", "move", "pub", "ref", "remote", "return", "true", "type",
                "enum", "try",
            ] {
                insert_completion(&mut items, keyword, CompletionItemKind::KEYWORD, None);
            }
        }
        Some(CompletionResponse::Array(items.into_values().collect()))
    }

    pub(super) fn references(&self, params: &ReferenceParams) -> Option<Vec<Location>> {
        let position = &params.text_document_position;
        let compilation = self.semantic_compilation_for(&position.text_document.uri)?;
        let module = module_for_uri(&compilation, &position.text_document.uri)?;
        let source = compilation
            .package
            .module(&compilation.hir.modules[module].name)?
            .source
            .as_deref()?;
        let offset = position_to_offset(source, position.position)?;
        let symbol = symbol_at(&compilation, module, source, offset)?;
        let mut locations = symbol_locations(&compilation, symbol);
        if !params.context.include_declaration {
            let declaration = symbol_declaration(&compilation, symbol);
            locations.retain(|location| Some(location) != declaration.as_ref());
        }
        Some(locations)
    }

    pub(super) fn rename(&self, params: &RenameParams) -> Option<WorkspaceEdit> {
        if !valid_identifier(&params.new_name) {
            return None;
        }
        let position = &params.text_document_position;
        let compilation = self.semantic_compilation_for(&position.text_document.uri)?;
        let module = module_for_uri(&compilation, &position.text_document.uri)?;
        let source = compilation
            .package
            .module(&compilation.hir.modules[module].name)?
            .source
            .as_deref()?;
        let offset = position_to_offset(source, position.position)?;
        let symbol = symbol_at(&compilation, module, source, offset)?;
        if matches!(symbol, SymbolIdentity::Builtin(_)) {
            return None;
        }
        let mut grouped = std::collections::BTreeMap::<String, (Uri, Vec<TextEdit>)>::new();
        for location in symbol_locations(&compilation, symbol) {
            grouped
                .entry(location.uri.as_str().to_owned())
                .or_insert_with(|| (location.uri.clone(), Vec::new()))
                .1
                .push(TextEdit {
                    range: location.range,
                    new_text: params.new_name.clone(),
                });
        }
        let edits = grouped
            .into_values()
            .map(|(uri, edits)| TextDocumentEdit {
                text_document: OptionalVersionedTextDocumentIdentifier {
                    version: self.version(&uri),
                    uri,
                },
                edits: edits.into_iter().map(OneOf::Left).collect(),
            })
            .collect();
        Some(WorkspaceEdit {
            changes: None,
            document_changes: Some(DocumentChanges::Edits(edits)),
            change_annotations: None,
        })
    }

    fn version(&self, uri: &Uri) -> Option<i32> {
        self.documents.get(uri).map(|document| document.version)
    }
}

fn add_arguments_auto_import_completion(
    source: &str,
    offset: usize,
    items: &mut std::collections::BTreeMap<String, CompletionItem>,
) {
    let Some((prefix, _)) = identifier_at(source, offset) else {
        return;
    };
    if !"Arguments".starts_with(prefix) {
        return;
    }
    let Ok(program) = crate::parse(source) else {
        return;
    };
    let in_parameter_type = program.functions.iter().any(|function| {
        function.parameters.iter().any(|parameter| {
            parameter
                .type_span
                .as_ref()
                .is_some_and(|span| span.start <= offset && offset <= span.end)
        })
    });
    if !in_parameter_type {
        return;
    }
    let imported = program
        .imports
        .iter()
        .any(|import| import.path == ["std", "process"]);
    let additional_text_edits = (!imported)
        .then(|| arguments_import_edit(source, &program))
        .flatten()
        .map(|edit| vec![edit]);
    items.insert(
        "Arguments".into(),
        CompletionItem {
            label: "Arguments".into(),
            kind: Some(CompletionItemKind::STRUCT),
            detail: Some("std.process.Arguments".into()),
            documentation: Some(Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: "The executable name and command-line values supplied to `main`.".into(),
            })),
            sort_text: Some("0_Arguments".into()),
            insert_text: Some("Arguments".into()),
            additional_text_edits,
            ..CompletionItem::default()
        },
    );
}

fn arguments_import_edit(source: &str, program: &crate::ast::Program) -> Option<TextEdit> {
    let (offset, new_text) = if let Some(import) = program.imports.last() {
        (import.span.end, "\nimport std.process")
    } else {
        let mut offset = 0;
        for token in crate::lexer::lex(source).ok()? {
            match token.kind {
                crate::lexer::TokenKind::Newline | crate::lexer::TokenKind::ModuleDocComment(_) => {
                    offset = token.range.end
                }
                _ => break,
            }
        }
        (offset, "import std.process\n")
    };
    let position = byte_range_to_lsp(source, offset..offset).start;
    Some(TextEdit {
        range: lsp_types::Range::new(position, position),
        new_text: new_text.into(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SymbolIdentity {
    Local(crate::hir::LocalId),
    Constant(crate::hir::ConstantId),
    Function(crate::hir::FunctionId),
    Record(crate::hir::RecordId),
    RequiredMethod(crate::hir::RecordId, usize),
    Variant(crate::hir::VariantTypeId),
    Builtin(crate::hir::Builtin),
}

fn expression_at(
    compilation: &crate::hir::Compilation,
    module: crate::hir::ModuleId,
    offset: usize,
) -> Option<crate::hir::ExprId> {
    compilation
        .hir
        .expression_spans
        .iter()
        .filter(|(expression, span)| {
            span.start <= offset
                && offset <= span.end
                && compilation
                    .hir
                    .expression_functions
                    .get(expression)
                    .is_some_and(|function| compilation.hir.functions[*function].module == module)
        })
        .min_by_key(|(_, span)| span.end.saturating_sub(span.start))
        .map(|(expression, _)| *expression)
}

fn symbol_at(
    compilation: &crate::hir::Compilation,
    module_id: crate::hir::ModuleId,
    source: &str,
    offset: usize,
) -> Option<SymbolIdentity> {
    let (name, start) = identifier_at(source, offset)?;
    let module = &compilation.hir.modules[module_id];
    let qualifier = qualifier_before(source, start);
    if let Some(expression) = expression_at(compilation, module_id, offset) {
        match &compilation.hir.expressions[expression] {
            crate::hir::Expr::Member {
                object,
                name: member,
            } if member == name => {
                if let Some(function) = member_function(compilation, *object, member) {
                    return Some(SymbolIdentity::Function(function));
                }
                if let Some((record, method)) = required_method(compilation, *object, member) {
                    return Some(SymbolIdentity::RequiredMethod(record, method));
                }
            }
            crate::hir::Expr::Name(resolved) => match *resolved {
                crate::hir::ResolvedName::Local(local) => {
                    return Some(SymbolIdentity::Local(local));
                }
                crate::hir::ResolvedName::Constant(constant) => {
                    return Some(SymbolIdentity::Constant(constant));
                }
                crate::hir::ResolvedName::Function(function) => {
                    return Some(SymbolIdentity::Function(function));
                }
                crate::hir::ResolvedName::Record(record) => {
                    return Some(SymbolIdentity::Record(record));
                }
                crate::hir::ResolvedName::Variant(variant) => {
                    return Some(SymbolIdentity::Variant(
                        compilation.hir.variants[variant].parent,
                    ));
                }
                crate::hir::ResolvedName::Builtin(builtin) => {
                    return Some(SymbolIdentity::Builtin(builtin));
                }
                crate::hir::ResolvedName::Module(_) => {}
            },
            _ => {}
        }
    }
    if let Some(function) = function_at(compilation, module_id, offset) {
        let definition = &compilation.hir.functions[function];
        let declared_name = definition
            .name
            .rsplit('.')
            .next()
            .unwrap_or(&definition.name);
        let source_name = source_function_name(definition);
        if declared_name == name
            && find_name(source, definition.span.clone(), &source_name)
                .is_some_and(|span| span.start <= offset && offset <= span.end)
        {
            return Some(SymbolIdentity::Function(function));
        }
    }
    if qualifier.is_none()
        && let Some(function) = function_at(compilation, module_id, offset)
        && let Some((local, _)) = compilation
            .hir
            .locals
            .iter()
            .find(|(_, local)| local.function == function && local.name == name)
    {
        return Some(SymbolIdentity::Local(local));
    }
    let target = qualifier
        .as_deref()
        .and_then(|qualifier| module.imports.get(qualifier).copied())
        .unwrap_or(module_id);
    declaration_identity(compilation, target, name).or_else(|| {
        (qualifier.is_none()).then(|| {
            let mut matches = module
                .imports
                .values()
                .filter_map(|imported| declaration_identity(compilation, *imported, name));
            let first = matches.next()?;
            matches.next().is_none().then_some(first)
        })?
    })
}

fn symbol_hover(compilation: &crate::hir::Compilation, symbol: SymbolIdentity) -> Option<String> {
    match symbol {
        SymbolIdentity::Local(local) => compilation.types.local_type(local).map(|ty| {
            documented_hover(
                format!(
                    "{}: {}",
                    compilation.hir.locals[local].name,
                    compilation.types.display(ty)
                ),
                None,
            )
        }),
        SymbolIdentity::Constant(constant) => {
            let definition = &compilation.hir.constants[constant];
            compilation.types.constants.get(&constant).map(|ty| {
                documented_hover(
                    format!(
                        "const {}: {}",
                        definition.name,
                        compilation.types.display(*ty)
                    ),
                    definition.documentation.as_deref(),
                )
            })
        }
        SymbolIdentity::Function(function) => declaration_hover(
            compilation,
            compilation.hir.functions[function].module,
            &compilation.hir.functions[function].name,
        ),
        SymbolIdentity::Record(record) => {
            let definition = &compilation.hir.records[record];
            Some(documented_hover(
                record_signature(definition),
                definition.documentation.as_deref(),
            ))
        }
        SymbolIdentity::RequiredMethod(record, method) => {
            let method = &compilation.hir.records[record].methods[method];
            Some(documented_hover(
                method_requirement_signature(method),
                method.documentation.as_deref(),
            ))
        }
        SymbolIdentity::Variant(variant) => {
            let definition = &compilation.hir.variant_types[variant];
            Some(documented_hover(
                variant_signature(compilation, variant),
                definition.documentation.as_deref(),
            ))
        }
        SymbolIdentity::Builtin(builtin) => {
            let info = super::builtins::info(builtin);
            Some(documented_hover(
                info.signature.to_owned(),
                Some(info.documentation),
            ))
        }
    }
}

fn declaration_identity(
    compilation: &crate::hir::Compilation,
    module: crate::hir::ModuleId,
    name: &str,
) -> Option<SymbolIdentity> {
    let definitions = &compilation.hir.modules[module];
    definitions
        .constants
        .get(name)
        .copied()
        .map(SymbolIdentity::Constant)
        .or_else(|| {
            definitions
                .functions
                .get(name)
                .copied()
                .map(SymbolIdentity::Function)
        })
        .or_else(|| {
            definitions
                .records
                .get(name)
                .copied()
                .map(SymbolIdentity::Record)
        })
        .or_else(|| {
            definitions
                .variant_types
                .get(name)
                .copied()
                .map(SymbolIdentity::Variant)
        })
}

fn symbol_locations(
    compilation: &crate::hir::Compilation,
    symbol: SymbolIdentity,
) -> Vec<Location> {
    let mut locations = Vec::new();
    for (module_id, module) in compilation.hir.modules.iter() {
        let Some(path) = module.source_path.as_deref() else {
            continue;
        };
        let Some(source) = compilation
            .package
            .module(&module.name)
            .and_then(|module| module.source.as_deref())
        else {
            continue;
        };
        let Some(uri) = path_to_uri(path.as_std_path()) else {
            continue;
        };
        let name = symbol_name(compilation, symbol);
        let source_name = name.to_owned();
        let ranges = if source_name.contains('.') {
            qualified_name_ranges(source, &source_name)
        } else {
            identifier_ranges(source, &source_name).collect()
        };
        for range in ranges {
            let lookup_offset = source_name
                .rsplit_once('.')
                .map_or(range.start, |(_, member)| range.end - member.len());
            if symbol_at(compilation, module_id, source, lookup_offset) == Some(symbol) {
                let location_range = source_name
                    .rsplit_once('.')
                    .map_or(range.clone(), |(_, member)| {
                        range.end - member.len()..range.end
                    });
                locations.push(Location::new(
                    uri.clone(),
                    byte_range_to_lsp(source, location_range),
                ));
            }
        }
    }
    locations
}

fn qualified_name_ranges(source: &str, expected: &str) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::new();
    let mut offset = 0;
    while let Some(relative) = source[offset..].find(expected) {
        let start = offset + relative;
        let end = start + expected.len();
        let starts_at_boundary = start == 0 || !is_ident(source.as_bytes()[start - 1]);
        let ends_at_boundary = end == source.len() || !is_ident(source.as_bytes()[end]);
        if starts_at_boundary && ends_at_boundary {
            ranges.push(start..end);
        }
        offset = end;
    }
    ranges
}

fn symbol_name(compilation: &crate::hir::Compilation, symbol: SymbolIdentity) -> &str {
    match symbol {
        SymbolIdentity::Local(local) => &compilation.hir.locals[local].name,
        SymbolIdentity::Constant(constant) => &compilation.hir.constants[constant].name,
        SymbolIdentity::Function(function) => &compilation.hir.functions[function].name,
        SymbolIdentity::Record(record) => &compilation.hir.records[record].name,
        SymbolIdentity::RequiredMethod(record, method) => {
            &compilation.hir.records[record].methods[method].name
        }
        SymbolIdentity::Variant(variant) => &compilation.hir.variant_types[variant].name,
        SymbolIdentity::Builtin(builtin) => super::builtins::info(builtin).name,
    }
}

pub(super) fn symbol_declaration(
    compilation: &crate::hir::Compilation,
    symbol: SymbolIdentity,
) -> Option<Location> {
    match symbol {
        SymbolIdentity::Local(local) => {
            let definition = &compilation.hir.locals[local];
            let function = &compilation.hir.functions[definition.function];
            location(
                compilation,
                function.module,
                definition.span.clone(),
                &definition.name,
            )
        }
        SymbolIdentity::Constant(constant) => {
            let definition = &compilation.hir.constants[constant];
            location(
                compilation,
                definition.module,
                definition.span.clone(),
                &definition.name,
            )
        }
        SymbolIdentity::Function(function) => {
            let definition = &compilation.hir.functions[function];
            let member = definition
                .name
                .rsplit_once('.')
                .map_or(definition.name.as_str(), |(_, member)| member);
            location(
                compilation,
                definition.module,
                definition.span.clone(),
                member,
            )
        }
        SymbolIdentity::Record(record) => {
            let definition = &compilation.hir.records[record];
            location(
                compilation,
                definition.module,
                definition.span.clone(),
                &definition.name,
            )
        }
        SymbolIdentity::RequiredMethod(record, method) => {
            let record = &compilation.hir.records[record];
            let method = &record.methods[method];
            location(
                compilation,
                record.module,
                method.span.clone(),
                &method.name,
            )
        }
        SymbolIdentity::Variant(variant) => {
            let definition = &compilation.hir.variant_types[variant];
            location(
                compilation,
                definition.module,
                definition.span.clone(),
                &definition.name,
            )
        }
        SymbolIdentity::Builtin(builtin) => super::builtins::definition_location(builtin),
    }
}

fn identifier_ranges<'a>(
    source: &'a str,
    expected: &'a str,
) -> impl Iterator<Item = std::ops::Range<usize>> + 'a {
    let bytes = source.as_bytes();
    let mut index = 0;
    std::iter::from_fn(move || {
        while index < bytes.len() {
            if !is_ident(bytes[index]) {
                index += 1;
                continue;
            }
            let start = index;
            while index < bytes.len() && is_ident(bytes[index]) {
                index += 1;
            }
            if &source[start..index] == expected {
                return Some(start..index);
            }
        }
        None
    })
}

fn valid_identifier(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|first| first == b'_' || first.is_ascii_alphabetic())
        && bytes.all(is_ident)
}

fn function_at(
    compilation: &crate::hir::Compilation,
    module: crate::hir::ModuleId,
    offset: usize,
) -> Option<crate::hir::FunctionId> {
    compilation
        .hir
        .functions
        .iter()
        .filter(|(_, function)| {
            function.module == module
                && function.span.start <= offset
                && offset <= function.span.end
        })
        .min_by_key(|(_, function)| function.span.end.saturating_sub(function.span.start))
        .map(|(id, _)| id)
}

fn declaration_hover(
    compilation: &crate::hir::Compilation,
    module: crate::hir::ModuleId,
    name: &str,
) -> Option<String> {
    let definitions = &compilation.hir.modules[module];
    if let Some(constant) = definitions.constants.get(name) {
        let definition = &compilation.hir.constants[*constant];
        return Some(documented_hover(
            format!(
                "const {}: {}",
                definition.name,
                compilation
                    .types
                    .constants
                    .get(constant)
                    .map_or_else(|| "unknown".into(), |ty| compilation.types.display(*ty))
            ),
            definition.documentation.as_deref(),
        ));
    }
    if let Some(function) = definitions.functions.get(name) {
        let definition = &compilation.hir.functions[*function];
        return Some(documented_hover(
            function_signature(compilation, *function, false),
            definition.documentation.as_deref(),
        ));
    }
    if let Some(record) = definitions.records.get(name) {
        return Some(documented_hover(
            record_signature(&compilation.hir.records[*record]),
            compilation.hir.records[*record].documentation.as_deref(),
        ));
    }
    if let Some(variant) = definitions.variant_types.get(name) {
        return Some(documented_hover(
            variant_signature(compilation, *variant),
            compilation.hir.variant_types[*variant]
                .documentation
                .as_deref(),
        ));
    }
    None
}

fn add_module_completions(
    compilation: &crate::hir::Compilation,
    module: crate::hir::ModuleId,
    public_only: bool,
    items: &mut std::collections::BTreeMap<String, CompletionItem>,
) {
    let definitions = &compilation.hir.modules[module];
    for (name, constant) in &definitions.constants {
        let definition = &compilation.hir.constants[*constant];
        if !public_only || definition.public {
            insert_documented_completion(
                items,
                name,
                CompletionItemKind::CONSTANT,
                compilation
                    .types
                    .constants
                    .get(constant)
                    .map(|ty| compilation.types.display(*ty)),
                definition.documentation.as_deref(),
            );
        }
    }
    for (name, function) in &definitions.functions {
        let definition = &compilation.hir.functions[*function];
        if !definition.name.contains('$') && (!public_only || definition.public) {
            let detail = compilation.types.function_type(*function).map(|signature| {
                format!(
                    "func({}) -> {}",
                    signature
                        .parameters
                        .iter()
                        .map(|ty| compilation.types.display(*ty))
                        .collect::<Vec<_>>()
                        .join(", "),
                    compilation.types.display(signature.result)
                )
            });
            insert_documented_completion(
                items,
                name,
                CompletionItemKind::FUNCTION,
                detail,
                definition.documentation.as_deref(),
            );
        }
    }
    for (name, record) in &definitions.records {
        if !public_only || compilation.hir.records[*record].public {
            insert_documented_completion(
                items,
                name,
                CompletionItemKind::STRUCT,
                Some("type".into()),
                compilation.hir.records[*record].documentation.as_deref(),
            );
        }
    }
    for (name, variant) in &definitions.variant_types {
        if !public_only || compilation.hir.variant_types[*variant].public {
            insert_documented_completion(
                items,
                name,
                if compilation.hir.variant_types[*variant].kind
                    == crate::ast::VariantKind::Enum
                {
                    CompletionItemKind::ENUM
                } else {
                    CompletionItemKind::INTERFACE
                },
                Some(
                    if compilation.hir.variant_types[*variant].kind
                        == crate::ast::VariantKind::Enum
                    {
                        "enum"
                    } else {
                        "type"
                    }
                    .into(),
                ),
                compilation.hir.variant_types[*variant]
                    .documentation
                    .as_deref(),
            );
        }
    }
}

fn add_associated_completions(
    compilation: &crate::hir::Compilation,
    current_module: crate::hir::ModuleId,
    type_name: &str,
    items: &mut std::collections::BTreeMap<String, CompletionItem>,
) -> bool {
    let local = compilation.hir.record_named(current_module, type_name);
    let imported = compilation.hir.modules[current_module]
        .imports
        .values()
        .filter_map(|module| {
            compilation
                .hir
                .record_named(*module, type_name)
                .filter(|record| compilation.hir.records[*record].public)
                .map(|_| *module)
        })
        .collect::<Vec<_>>();
    let (module, public_only) = match (local, imported.as_slice()) {
        (Some(_), _) => (current_module, false),
        (None, [module]) => (*module, true),
        _ => return false,
    };
    let prefix = format!("{type_name}.");
    for (name, function) in &compilation.hir.modules[module].functions {
        let Some(member) = name.strip_prefix(&prefix) else {
            continue;
        };
        let definition = &compilation.hir.functions[*function];
        if public_only && !definition.public {
            continue;
        }
        let detail = compilation.types.function_type(*function).map(|signature| {
            format!(
                "func({}) -> {}",
                signature
                    .parameters
                    .iter()
                    .map(|ty| compilation.types.display(*ty))
                    .collect::<Vec<_>>()
                    .join(", "),
                compilation.types.display(signature.result)
            )
        });
        insert_documented_completion(
            items,
            member,
            CompletionItemKind::FUNCTION,
            detail,
            definition.documentation.as_deref(),
        );
    }
    true
}

fn insert_completion(
    items: &mut std::collections::BTreeMap<String, CompletionItem>,
    label: &str,
    kind: CompletionItemKind,
    detail: Option<String>,
) {
    items
        .entry(label.to_owned())
        .or_insert_with(|| CompletionItem {
            label: label.to_owned(),
            kind: Some(kind),
            detail,
            ..CompletionItem::default()
        });
}

fn insert_documented_completion(
    items: &mut std::collections::BTreeMap<String, CompletionItem>,
    label: &str,
    kind: CompletionItemKind,
    detail: Option<String>,
    documentation: Option<&str>,
) {
    insert_completion(items, label, kind, detail);
    if let Some(documentation) = documentation
        && let Some(item) = items.get_mut(label)
    {
        item.documentation = Some(Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: documentation.to_owned(),
        }));
    }
}

pub(super) struct CallablePresentation {
    pub signature: String,
    pub parameters: Vec<String>,
    pub documentation: Option<String>,
    pub definition: Option<Location>,
}

pub(super) fn callable_presentation(
    compilation: &crate::hir::Compilation,
    callee: crate::hir::ExprId,
) -> Option<CallablePresentation> {
    let (function, receiver) = match &compilation.hir.expressions[callee] {
        crate::hir::Expr::Name(crate::hir::ResolvedName::Function(function)) => (*function, false),
        crate::hir::Expr::Member { object, name } => {
            if let Some(function) = member_function(compilation, *object, name) {
                (function, true)
            } else {
                let (record, method_index) = required_method(compilation, *object, name)?;
                let method = &compilation.hir.records[record].methods[method_index];
                return Some(CallablePresentation {
                    signature: method_requirement_signature(method),
                    parameters: method
                        .parameters
                        .iter()
                        .skip(1)
                        .map(|parameter| parameter.name.clone())
                        .collect(),
                    documentation: method.documentation.clone(),
                    definition: symbol_declaration(
                        compilation,
                        SymbolIdentity::RequiredMethod(record, method_index),
                    ),
                });
            }
        }
        crate::hir::Expr::Name(crate::hir::ResolvedName::Builtin(builtin)) => {
            let info = super::builtins::info(*builtin);
            return Some(CallablePresentation {
                signature: info.signature.to_owned(),
                parameters: info
                    .parameters
                    .iter()
                    .map(|name| (*name).to_owned())
                    .collect(),
                documentation: Some(info.documentation.to_owned()),
                definition: super::builtins::definition_location(*builtin),
            });
        }
        _ => return None,
    };
    let definition = &compilation.hir.functions[function];
    Some(CallablePresentation {
        signature: function_signature(compilation, function, receiver),
        parameters: definition
            .parameters
            .iter()
            .skip(usize::from(receiver))
            .map(|parameter| compilation.hir.locals[*parameter].name.clone())
            .collect(),
        documentation: definition.documentation.clone(),
        definition: symbol_declaration(compilation, SymbolIdentity::Function(function)),
    })
}

fn member_function(
    compilation: &crate::hir::Compilation,
    object: crate::hir::ExprId,
    member: &str,
) -> Option<crate::hir::FunctionId> {
    let ty = compilation.types.expression_type(object)?;
    let owner = nominal_owner_from_type(&compilation.types, ty)?;
    let (module, name) = match owner {
        NominalOwner::Record(record) => {
            let definition = &compilation.hir.records[record];
            (definition.module, definition.name.as_str())
        }
        NominalOwner::Variant(variant) => {
            let definition = &compilation.hir.variant_types[variant];
            (definition.module, definition.name.as_str())
        }
        NominalOwner::Bool => (compilation.hir.module_named("core.bool")?, "Bool"),
        NominalOwner::Int => (compilation.hir.module_named("core.int")?, "Int"),
        NominalOwner::Float => (compilation.hir.module_named("core.float")?, "Float"),
        NominalOwner::CodePoint => (
            compilation.hir.module_named("core.code_point")?,
            "CodePoint",
        ),
        NominalOwner::Byte => (compilation.hir.module_named("core.byte")?, "Byte"),
    };
    let qualified_name = format!("{name}.{member}");
    let function = compilation.hir.modules[module]
        .functions
        .get(&qualified_name)
        .copied()?;
    let receiver_matches = compilation
        .types
        .function_type(function)
        .and_then(|signature| signature.parameters.first())
        .is_some_and(|ty| {
            matches!(
                (owner, &compilation.types.types[*ty]),
                (
                    NominalOwner::Record(expected),
                    crate::types::Type::Record { record, .. }
                ) if expected == *record
            ) || matches!(
                (owner, &compilation.types.types[*ty]),
                (
                    NominalOwner::Variant(expected),
                    crate::types::Type::Variant { variant, .. }
                ) if expected == *variant
            ) || matches!(
                (owner, &compilation.types.types[*ty]),
                (NominalOwner::Bool, crate::types::Type::Bool)
                    | (NominalOwner::Int, crate::types::Type::Int)
                    | (NominalOwner::Float, crate::types::Type::Float)
                    | (NominalOwner::CodePoint, crate::types::Type::CodePoint)
                    | (NominalOwner::Byte, crate::types::Type::Byte)
            )
        });
    (receiver_matches && compilation.hir.functions[function].receiver.is_some()).then_some(function)
}

#[derive(Clone, Copy)]
enum NominalOwner {
    Record(crate::hir::RecordId),
    Variant(crate::hir::VariantTypeId),
    Bool,
    Int,
    Float,
    CodePoint,
    Byte,
}

fn nominal_owner_from_type(
    types: &crate::types::TypeInformation,
    ty: crate::types::TypeId,
) -> Option<NominalOwner> {
    match &types.types[ty] {
        crate::types::Type::Record { record, .. } => Some(NominalOwner::Record(*record)),
        crate::types::Type::Variant { variant, .. } => Some(NominalOwner::Variant(*variant)),
        crate::types::Type::Bool => Some(NominalOwner::Bool),
        crate::types::Type::Int => Some(NominalOwner::Int),
        crate::types::Type::Float => Some(NominalOwner::Float),
        crate::types::Type::CodePoint => Some(NominalOwner::CodePoint),
        crate::types::Type::Byte => Some(NominalOwner::Byte),
        crate::types::Type::Reference { value, .. } | crate::types::Type::Remote(value) => {
            nominal_owner_from_type(types, *value)
        }
        _ => None,
    }
}

fn required_method(
    compilation: &crate::hir::Compilation,
    object: crate::hir::ExprId,
    member: &str,
) -> Option<(crate::hir::RecordId, usize)> {
    let ty = compilation.types.expression_type(object)?;
    let record = record_from_type(&compilation.types, ty)?;
    compilation.hir.records[record]
        .methods
        .iter()
        .position(|method| method.name == member)
        .map(|method| (record, method))
}

fn record_from_type(
    types: &crate::types::TypeInformation,
    ty: crate::types::TypeId,
) -> Option<crate::hir::RecordId> {
    match &types.types[ty] {
        crate::types::Type::Record { record, .. } => Some(*record),
        crate::types::Type::Reference { value, .. } | crate::types::Type::Remote(value) => {
            record_from_type(types, *value)
        }
        _ => None,
    }
}

pub(super) fn function_signature(
    compilation: &crate::hir::Compilation,
    function_id: crate::hir::FunctionId,
    omit_receiver: bool,
) -> String {
    let function = &compilation.hir.functions[function_id];
    let signature = compilation.types.function_type(function_id);
    let skip = usize::from(omit_receiver);
    let parameters = function
        .parameters
        .iter()
        .enumerate()
        .skip(skip)
        .map(|(index, local)| {
            let ty = signature
                .and_then(|signature| signature.parameters.get(index))
                .map(|ty| compilation.types.display(*ty))
                .unwrap_or_else(|| "_".into());
            let consumed = signature
                .and_then(|signature| signature.parameter_modes.get(index))
                .is_some_and(|mode| *mode == crate::ast::ParameterMode::Consume);
            format!(
                "{}: {}{ty}",
                compilation.hir.locals[*local].name,
                if consumed { "consume " } else { "" }
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let result = signature
        .map(|signature| compilation.types.display(signature.result))
        .unwrap_or_else(|| "()".into());
    let effects = signature.map_or_else(String::new, |signature| {
        display_effects(&signature.effects, signature.suspends)
    });
    let generics = angle_parameters(&function.type_parameters);
    let group_entries = function
        .groups
        .iter()
        .map(|group| {
            format!(
                "{}: group {}",
                group.name,
                display_type_expr(&group.element)
            )
        })
        .collect::<Vec<_>>();
    let groups = square_parameters(&group_entries);
    format!(
        "{}func {}{generics}{groups}({parameters}) -> {result}{effects}",
        if function.public { "pub " } else { "" },
        source_function_name(function)
    )
}

fn source_function_name(function: &crate::hir::Function) -> String {
    function.name.clone()
}

fn record_signature(record: &crate::hir::Record) -> String {
    let parameters = angle_parameters(&record.parameters);
    let compositions = if record.compositions.is_empty() {
        String::new()
    } else {
        format!(
            " & {}",
            record
                .compositions
                .iter()
                .map(display_type_expr)
                .collect::<Vec<_>>()
                .join(" & ")
        )
    };
    let mut members = record
        .fields
        .iter()
        .map(|field| {
            format!(
                "    {}{}: {}",
                if field.public { "pub " } else { "" },
                field.name,
                display_type_expr(&field.ty)
            )
        })
        .collect::<Vec<_>>();
    members.extend(
        record
            .methods
            .iter()
            .map(|method| format!("    {}", method_requirement_signature(method))),
    );
    let visibility = if record.public { "pub " } else { "" };
    if members.is_empty() && !record.compositions.is_empty() {
        return format!(
            "{visibility}type {}{parameters} ={compositions}",
            record.name
        );
    }
    let members = members.join("\n");
    let body_separator = if record.compositions.is_empty() {
        ""
    } else {
        " &"
    };
    format!(
        "{visibility}type {}{parameters} ={compositions}{body_separator} {{\n{members}\n}}",
        record.name
    )
}

fn method_requirement_signature(method: &crate::ast::MethodRequirement) -> String {
    let parameters = method
        .parameters
        .iter()
        .map(|parameter| match &parameter.ty {
            Some(ty) => format!("{}: {}", parameter.name, display_type_expr(ty)),
            None => parameter.name.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    let result = method
        .return_type
        .as_ref()
        .map(display_type_expr)
        .unwrap_or_else(|| "()".into());
    format!(
        "{}func {}{}({parameters}) -> {result}{}",
        if method.public { "pub " } else { "" },
        method.name,
        angle_parameters(&method.type_parameters),
        display_effects(&method.effects, method.suspends)
    )
}

fn variant_signature(
    compilation: &crate::hir::Compilation,
    variant_id: crate::hir::VariantTypeId,
) -> String {
    let variant = &compilation.hir.variant_types[variant_id];
    let parameters = angle_parameters(&variant.parameters);
    if variant.kind == crate::ast::VariantKind::Union
        && variant.alternatives.len() == 1
        && variant.compositions.is_empty()
        && variant.methods.is_empty()
    {
        let member = compilation.hir.variants[variant.alternatives[0]]
            .member
            .as_ref()
            .expect("a union alternative has a member type");
        return format!(
            "{}type {}{parameters} = {}",
            if variant.public { "pub " } else { "" },
            variant.name,
            display_type_expr(member)
        );
    }
    let alternatives = variant
        .alternatives
        .iter()
        .enumerate()
        .map(|(index, alternative)| {
            let alternative = &compilation.hir.variants[*alternative];
            let member = if let Some(member) = &alternative.member {
                display_type_expr(member)
            } else if alternative.payload.is_none() {
                alternative.name.clone()
            } else {
                format!(
                    "{}({})",
                    alternative.name,
                    alternative
                        .payload
                        .iter()
                        .map(display_type_expr)
                        .next()
                        .expect("a payload-bearing enum case has a payload type")
                )
            };
            if index == 0 && variant.kind == crate::ast::VariantKind::Enum {
                member
            } else if index == 0 {
                format!("    {member}")
            } else {
                format!("    | {member}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let compositions = variant
        .compositions
        .iter()
        .map(|composition| format!("    & {}", display_type_expr(composition)))
        .collect::<Vec<_>>();
    let body = if variant.methods.is_empty() {
        String::new()
    } else {
        format!(
            "\n    & {{\n{}\n    }}",
            variant
                .methods
                .iter()
                .map(|method| format!("        {}", method_requirement_signature(method)))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    let contracts = if compositions.is_empty() {
        String::new()
    } else {
        format!("\n{}", compositions.join("\n"))
    };
    let first_case_separator = if variant.kind == crate::ast::VariantKind::Enum {
        " "
    } else {
        "\n"
    };
    format!(
        "{}{} {}{parameters} ={first_case_separator}{alternatives}{contracts}{body}",
        if variant.public { "pub " } else { "" },
        if variant.kind == crate::ast::VariantKind::Enum {
            "enum"
        } else {
            "type"
        },
        variant.name
    )
}

fn angle_parameters(parameters: &[String]) -> String {
    if parameters.is_empty() {
        String::new()
    } else {
        format!("<{}>", parameters.join(", "))
    }
}

fn square_parameters(parameters: &[String]) -> String {
    if parameters.is_empty() {
        String::new()
    } else {
        format!("[{}]", parameters.join(", "))
    }
}

fn display_type_expr(ty: &crate::ast::TypeExpr) -> String {
    match ty {
        crate::ast::TypeExpr::Unit => "()".into(),
        crate::ast::TypeExpr::Named(name, arguments) => {
            let arguments = arguments.iter().map(display_type_expr).collect::<Vec<_>>();
            format!(
                "{}{}",
                name.replace('.', "::"),
                angle_parameters(&arguments)
            )
        }
        crate::ast::TypeExpr::Intersection(members) => members
            .iter()
            .map(display_type_expr)
            .collect::<Vec<_>>()
            .join(" & "),
        crate::ast::TypeExpr::Reference { group, value } => {
            format!("ref[{group}] {}", display_type_expr(value))
        }
        crate::ast::TypeExpr::Function {
            parameters,
            parameter_modes,
            result,
            effects,
            suspends,
        } => {
            let parameters = parameters
                .iter()
                .zip(parameter_modes)
                .map(|(parameter, mode)| {
                    format!(
                        "{}{}",
                        if *mode == crate::ast::ParameterMode::Consume {
                            "consume "
                        } else {
                            ""
                        },
                        display_type_expr(parameter)
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "func({parameters}) -> {}{}",
                display_type_expr(result),
                display_effects(effects, *suspends)
            )
        }
    }
}

fn display_effects(effects: &[crate::ast::Effect], suspends: bool) -> String {
    let mut values = effects
        .iter()
        .map(|effect| {
            let kind = match effect.kind {
                crate::ast::EffectKind::Read => "read",
                crate::ast::EffectKind::Mut => "mut",
                crate::ast::EffectKind::Reshape => "reshape",
                crate::ast::EffectKind::Consume => "consume",
            };
            format!("{kind} {}", effect.target)
        })
        .collect::<Vec<_>>();
    if suspends {
        values.push("suspend".into());
    }
    if values.is_empty() {
        String::new()
    } else {
        format!(" [{}]", values.join(", "))
    }
}

fn documented_hover(signature: String, documentation: Option<&str>) -> String {
    let mut hover = format!("```foster\n{signature}\n```");
    if let Some(documentation) = documentation {
        hover.push_str("\n\n");
        hover.push_str(documentation);
    }
    hover
}

fn compiler_diagnostic(source: &str, diagnostic: &crate::diagnostic::Diagnostic) -> Diagnostic {
    let range = diagnostic
        .labels
        .iter()
        .find(|label| label.primary)
        .or_else(|| diagnostic.labels.first())
        .map_or_else(
            || lsp_types::Range::new(Position::new(0, 0), Position::new(0, 1)),
            |label| byte_range_to_lsp(source, label.range.clone()),
        );
    Diagnostic {
        range,
        severity: Some(match diagnostic.severity {
            crate::diagnostic::Severity::Error => DiagnosticSeverity::ERROR,
            crate::diagnostic::Severity::Warning => DiagnosticSeverity::WARNING,
        }),
        code: diagnostic.code.clone().map(NumberOrString::String),
        code_description: None,
        source: Some("foster".into()),
        message: lsp_diagnostic_message(diagnostic),
        related_information: None,
        tags: None,
        data: None,
    }
}

fn lsp_diagnostic_message(diagnostic: &crate::diagnostic::Diagnostic) -> String {
    let mut message = diagnostic.message.clone();
    for label in diagnostic.labels.iter().filter(|label| !label.primary) {
        message.push_str("\n\n");
        message.push_str(&label.message);
    }
    for note in &diagnostic.notes {
        message.push_str("\n\nnote: ");
        message.push_str(note);
    }
    if let Some(help) = &diagnostic.help {
        message.push_str("\n\nhelp: ");
        message.push_str(help);
    }
    message
}

fn symbol(
    name: &str,
    kind: SymbolKind,
    source: &str,
    span: std::ops::Range<usize>,
) -> DocumentSymbol {
    let range = byte_range_to_lsp(source, span.clone());
    let selection = find_name(source, span, name)
        .map(|span| byte_range_to_lsp(source, span))
        .unwrap_or(range);
    #[allow(deprecated)]
    DocumentSymbol {
        name: name.to_owned(),
        detail: None,
        kind,
        tags: None,
        deprecated: None,
        range,
        selection_range: selection,
        children: None,
    }
}

fn definition_in_module(
    compilation: &crate::hir::Compilation,
    module: crate::hir::ModuleId,
    name: &str,
) -> Option<Location> {
    let definition = &compilation.hir.modules[module];
    let span = definition
        .constants
        .get(name)
        .map(|id| compilation.hir.constants[*id].span.clone())
        .or_else(|| {
            definition
                .functions
                .get(name)
                .map(|id| compilation.hir.functions[*id].span.clone())
        })
        .or_else(|| {
            definition
                .records
                .get(name)
                .map(|id| compilation.hir.records[*id].span.clone())
        })
        .or_else(|| {
            definition
                .variant_types
                .get(name)
                .map(|id| compilation.hir.variant_types[*id].span.clone())
        })?;
    location(compilation, module, span, name)
}

fn module_location(
    compilation: &crate::hir::Compilation,
    module: crate::hir::ModuleId,
) -> Option<Location> {
    location(compilation, module, 0..0, "")
}

fn embedded_module_location(module: &str) -> Option<Location> {
    let path = crate::package::embedded_source_path(module)?;
    Some(Location::new(
        path_to_uri(path.as_std_path())?,
        lsp_types::Range::default(),
    ))
}

fn location(
    compilation: &crate::hir::Compilation,
    module: crate::hir::ModuleId,
    span: std::ops::Range<usize>,
    name: &str,
) -> Option<Location> {
    let module = &compilation.hir.modules[module];
    let path = module.source_path.as_deref()?;
    let source = compilation
        .package
        .module(&module.name)?
        .source
        .as_deref()?;
    let span = find_name(source, span, name).unwrap_or(0..0);
    Some(Location::new(
        path_to_uri(path.as_std_path())?,
        byte_range_to_lsp(source, span),
    ))
}

pub(super) fn module_for_uri(
    compilation: &crate::hir::Compilation,
    uri: &Uri,
) -> Option<crate::hir::ModuleId> {
    let path = uri_to_path(uri)?;
    compilation.hir.modules.iter().find_map(|(id, module)| {
        module
            .source_path
            .as_ref()
            .is_some_and(|source| source.as_std_path() == path)
            .then_some(id)
    })
}

fn find_name(
    source: &str,
    span: std::ops::Range<usize>,
    name: &str,
) -> Option<std::ops::Range<usize>> {
    let start = span.start.min(source.len());
    let end = span.end.min(source.len());
    let relative = source.get(start..end)?.find(name)?;
    Some(start + relative..start + relative + name.len())
}

fn identifier_at(source: &str, offset: usize) -> Option<(&str, usize)> {
    let bytes = source.as_bytes();
    let mut start = offset.min(bytes.len());
    while start > 0 && is_ident(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = offset.min(bytes.len());
    while end < bytes.len() && is_ident(bytes[end]) {
        end += 1;
    }
    (start < end).then(|| (&source[start..end], start))
}

fn qualifier_before(source: &str, start: usize) -> Option<String> {
    let bytes = source.as_bytes();
    let mut end = start;
    while end > 0 && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    if end >= 2 && &bytes[end - 2..end] == b"::" {
        end -= 2;
    } else if end > 0 && bytes[end - 1] == b'.' {
        end -= 1;
    } else {
        return None;
    }
    let mut begin = end;
    while begin > 0 && is_ident(bytes[begin - 1]) {
        begin -= 1;
    }
    (begin < end).then(|| source[begin..end].to_owned())
}

fn is_ident(byte: u8) -> bool {
    byte == b'_' || byte == b'?' || byte.is_ascii_alphanumeric()
}

pub(super) fn position_to_offset(source: &str, position: Position) -> Option<usize> {
    let line = source.split_inclusive('\n').nth(position.line as usize)?;
    let line_start = source.as_ptr() as usize;
    let start = line.as_ptr() as usize - line_start;
    let mut utf16 = 0_u32;
    for (offset, character) in line.char_indices() {
        if utf16 >= position.character {
            return Some(start + offset);
        }
        utf16 += character.len_utf16() as u32;
    }
    Some(start + line.len())
}

pub(super) fn uri_to_path(uri: &Uri) -> Option<PathBuf> {
    let value = uri.as_str().strip_prefix("file:///")?;
    let decoded = percent_decode(value)?;
    #[cfg(windows)]
    let decoded = decoded.replace('/', "\\");
    Some(PathBuf::from(decoded))
}

pub(super) fn path_to_uri(path: &Path) -> Option<Uri> {
    let value = path
        .to_string_lossy()
        .replace('\\', "/")
        .replace(' ', "%20");
    Uri::from_str(&format!("file:///{value}")).ok()
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut result = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = value.get(index + 1..index + 3)?;
            result.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            result.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(result).ok()
}

#[cfg(test)]
mod tests;
