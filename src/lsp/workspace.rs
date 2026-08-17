use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use camino::Utf8PathBuf;
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
struct OpenDocument {
    text: String,
    version: i32,
}

pub(super) struct Workspace {
    root: Option<PathBuf>,
    documents: HashMap<Uri, OpenDocument>,
    published: HashSet<Uri>,
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
        }
    }

    pub(super) fn open(&mut self, uri: Uri, text: String, version: i32) {
        self.documents.insert(uri, OpenDocument { text, version });
    }

    pub(super) fn change(&mut self, uri: Uri, text: String, version: i32) {
        self.open(uri, text, version);
    }

    pub(super) fn close(&mut self, uri: &Uri) {
        self.documents.remove(uri);
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

    pub(super) fn document_symbols(&self, uri: &Uri) -> Option<DocumentSymbolResponse> {
        let compilation = self.compile_for(uri).ok()?;
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
                SymbolKind::ENUM,
                source,
                variant.span.clone(),
            ));
        }
        for (_, function) in
            compilation.hir.functions.iter().filter(|(_, function)| {
                function.module == module_id && !function.name.contains('$')
            })
        {
            symbols.push(symbol(
                &function.name,
                SymbolKind::FUNCTION,
                source,
                function.span.clone(),
            ));
        }
        Some(DocumentSymbolResponse::Nested(symbols))
    }

    pub(super) fn definition(&self, params: &TextDocumentPositionParams) -> Option<Location> {
        let compilation = self.compile_for(&params.text_document.uri).ok()?;
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
        let compilation = self.compile_for(&params.text_document.uri).ok()?;
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
        let compilation = self
            .compile_for(&params.text_document_position_params.text_document.uri)
            .ok()?;
        super::hints::signature_help(&compilation, params)
    }

    pub(super) fn inlay_hints(&self, params: &InlayHintParams) -> Option<Vec<InlayHint>> {
        let compilation = self.compile_for(&params.text_document.uri).ok()?;
        super::hints::inlay_hints(&compilation, params)
    }

    pub(super) fn completion(&self, params: &CompletionParams) -> Option<CompletionResponse> {
        let position = &params.text_document_position;
        let compilation = self.compile_for(&position.text_document.uri).ok()?;
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
        let mut items = std::collections::BTreeMap::<String, CompletionItem>::new();

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
                "await", "branch", "copy", "false", "func", "import", "move", "pub", "ref",
                "remote", "return", "true", "type",
            ] {
                insert_completion(&mut items, keyword, CompletionItemKind::KEYWORD, None);
            }
        }
        Some(CompletionResponse::Array(items.into_values().collect()))
    }

    pub(super) fn references(&self, params: &ReferenceParams) -> Option<Vec<Location>> {
        let position = &params.text_document_position;
        let compilation = self.compile_for(&position.text_document.uri).ok()?;
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
        let compilation = self.compile_for(&position.text_document.uri).ok()?;
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

    fn compile_for(&self, uri: &Uri) -> Result<crate::hir::Compilation, crate::error::FosterError> {
        let path = uri_to_path(uri).ok_or_else(|| {
            crate::error::FosterError::runtime("language server document is not a file URI")
        })?;
        let overlays = self
            .documents
            .iter()
            .filter_map(|(uri, document)| {
                let path = uri_to_path(uri)?;
                let path = Utf8PathBuf::from_path_buf(path).ok()?;
                Some((path, document.text.clone()))
            })
            .collect::<HashMap<_, _>>();

        let standalone = self.compile_standalone(&path, &overlays);
        if standalone.is_ok() {
            return standalone;
        }

        let workspace_root = self.root.as_deref();
        let mut candidate = path.parent();
        while let Some(root) = candidate {
            if workspace_root.is_some_and(|workspace| !root.starts_with(workspace)) {
                break;
            }
            if let Ok(package) = crate::package::Package::load_with_overlays(root, &overlays)
                && package.modules.values().any(|module| {
                    module
                        .source_path
                        .as_ref()
                        .is_some_and(|source| source.as_std_path() == path)
                })
            {
                return crate::hir::Compilation::new(package);
            }
            if workspace_root.is_some_and(|workspace| root == workspace) {
                break;
            }
            candidate = root.parent();
        }

        standalone
    }

    fn compile_standalone(
        &self,
        path: &Path,
        overlays: &HashMap<Utf8PathBuf, String>,
    ) -> Result<crate::hir::Compilation, crate::error::FosterError> {
        let source_path = Utf8PathBuf::from_path_buf(path.to_path_buf()).map_err(|path| {
            crate::error::FosterError::runtime(format!(
                "source path is not valid UTF-8: `{}`",
                path.display()
            ))
        })?;
        let source = overlays.get(&source_path).cloned().map_or_else(
            || {
                std::fs::read_to_string(path).map_err(|error| {
                    crate::error::FosterError::runtime(format!(
                        "cannot read `{}`: {error}",
                        path.display()
                    ))
                })
            },
            Ok,
        )?;
        let program = crate::parse(&source)?;
        let module_name = path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("main")
            .to_owned();
        let mut package = crate::package::Package::from_program_with_core(&module_name, program)?;
        let module = package
            .modules
            .get_mut(&module_name)
            .expect("standalone package contains its source module");
        module.source_path = Some(source_path);
        module.source = Some(source);
        crate::hir::Compilation::new(package)
    }

    fn version(&self, uri: &Uri) -> Option<i32> {
        self.documents.get(uri).map(|document| document.version)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SymbolIdentity {
    Local(crate::hir::LocalId),
    Constant(crate::hir::ConstantId),
    Function(crate::hir::FunctionId),
    Record(crate::hir::RecordId),
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
        if declared_name == name
            && find_name(source, definition.span.clone(), &definition.name)
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
        let ranges = if name.contains('.') {
            qualified_name_ranges(source, name)
        } else {
            identifier_ranges(source, name).collect()
        };
        for range in ranges {
            let lookup_offset = name
                .rsplit_once('.')
                .map_or(range.start, |(_, member)| range.end - member.len());
            if symbol_at(compilation, module_id, source, lookup_offset) == Some(symbol) {
                let location_range = name.rsplit_once('.').map_or(range.clone(), |(_, member)| {
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
                CompletionItemKind::ENUM,
                Some("type".into()),
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
            (member_function(compilation, *object, name)?, true)
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
    let record = record_from_type(&compilation.types, ty)?;
    let record = &compilation.hir.records[record];
    let function = compilation.hir.modules[record.module]
        .functions
        .get(member)
        .copied()?;
    compilation.hir.functions[function]
        .parameters
        .first()
        .is_some_and(|parameter| compilation.hir.locals[*parameter].name == "self")
        .then_some(function)
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
        .unwrap_or_else(|| "Unit".into());
    let effects = signature.map_or_else(String::new, |signature| {
        display_effects(&signature.effects, signature.suspends)
    });
    let mut generic_entries = function.type_parameters.clone();
    generic_entries.extend(function.groups.iter().map(|group| {
        format!(
            "{}: group {}",
            group.name,
            display_type_expr(&group.element)
        )
    }));
    let generics = type_parameters(&generic_entries);
    format!(
        "{}func {}{generics}({parameters}) -> {result}{effects}",
        if function.public { "pub " } else { "" },
        function.name
    )
}

fn record_signature(record: &crate::hir::Record) -> String {
    let parameters = type_parameters(&record.parameters);
    let fields = record
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
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{}type {}{parameters} {{\n{fields}\n}}",
        if record.public { "pub " } else { "" },
        record.name
    )
}

fn variant_signature(
    compilation: &crate::hir::Compilation,
    variant_id: crate::hir::VariantTypeId,
) -> String {
    let variant = &compilation.hir.variant_types[variant_id];
    let parameters = type_parameters(&variant.parameters);
    let alternatives = variant
        .alternatives
        .iter()
        .map(|alternative| {
            let alternative = &compilation.hir.variants[*alternative];
            if alternative.payload.is_empty() {
                alternative.name.clone()
            } else {
                format!(
                    "{}({})",
                    alternative.name,
                    alternative
                        .payload
                        .iter()
                        .map(display_type_expr)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        })
        .collect::<Vec<_>>()
        .join(" | ");
    format!(
        "{}type {}{parameters} = {alternatives}",
        if variant.public { "pub " } else { "" },
        variant.name
    )
}

fn type_parameters(parameters: &[String]) -> String {
    if parameters.is_empty() {
        String::new()
    } else {
        format!("[{}]", parameters.join(", "))
    }
}

fn display_type_expr(ty: &crate::ast::TypeExpr) -> String {
    match ty {
        crate::ast::TypeExpr::Named(name, arguments) => {
            let arguments = arguments.iter().map(display_type_expr).collect::<Vec<_>>();
            format!("{name}{}", type_parameters(&arguments))
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
            erased,
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
                "{}func({parameters}) -> {}{}",
                if *erased { "any " } else { "" },
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
    let range = diagnostic.labels.first().map_or_else(
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
        message: diagnostic.message.clone(),
        related_information: None,
        tags: None,
        data: None,
    }
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
    if end == 0 || bytes[end - 1] != b'.' {
        return None;
    }
    end -= 1;
    let mut begin = end;
    while begin > 0 && is_ident(bytes[begin - 1]) {
        begin -= 1;
    }
    (begin < end).then(|| source[begin..end].to_owned())
}

fn is_ident(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphanumeric()
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

fn uri_to_path(uri: &Uri) -> Option<PathBuf> {
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
mod tests {
    use super::*;

    fn fixture_workspace() -> (Workspace, Uri, PathBuf) {
        let root = std::env::current_dir()
            .unwrap()
            .join("tests/fixtures/modules");
        let main = root.join("main.foster");
        let uri = path_to_uri(&main).unwrap();
        (
            Workspace {
                root: Some(root.clone()),
                documents: HashMap::new(),
                published: HashSet::new(),
            },
            uri,
            root,
        )
    }

    #[test]
    fn document_symbols_use_open_buffer_overlays() {
        let (mut workspace, uri, root) = fixture_workspace();
        let mut source = std::fs::read_to_string(root.join("main.foster")).unwrap();
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
            root.join("json/parser.foster")
        );
        assert_eq!(location.range.start.line, 0);
    }

    #[test]
    fn associated_function_navigation_uses_the_type_namespace() {
        let root = std::env::current_dir()
            .unwrap()
            .join("tests/fixtures/associated_function");
        let main = root.join("main.foster");
        let uri = path_to_uri(&main).unwrap();
        let workspace = Workspace {
            root: Some(root.clone()),
            documents: HashMap::new(),
            published: HashSet::new(),
        };

        let location = workspace
            .definition(&TextDocumentPositionParams::new(
                lsp_types::TextDocumentIdentifier::new(uri.clone()),
                Position::new(3, 18),
            ))
            .unwrap();
        assert_eq!(
            uri_to_path(&location.uri).unwrap(),
            root.join("collection.foster")
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
                        path_to_uri(&root.join("main.foster")).unwrap(),
                    ),
                    Position::new(3, 17),
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
        let main_uri = path_to_uri(&root.join("main.foster")).unwrap();
        let values_uri = path_to_uri(&root.join("values.foster")).unwrap();
        let workspace = Workspace {
            root: Some(root.clone()),
            documents: HashMap::new(),
            published: HashSet::new(),
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
            root.join("values.foster")
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
            uri_to_path(&location.uri).as_deref() == Some(root.join("json/parser.foster").as_path())
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
        let source = r#"type Choice =
    | Some(String)
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
                Position::new(6, 32),
            ))
            .unwrap();

        assert_eq!(location.uri, uri);
        assert_eq!(location.range.start, Position::new(6, 20));
        assert_eq!(location.range.end, Position::new(6, 27));
    }

    #[test]
    fn inlay_hints_report_inferred_types_and_argument_names() {
        let (mut workspace, uri, _) = fixture_workspace();
        let source = r#"/// Adds two values.
func add(left: Int, right: Int) -> Int { left + right }

func main() -> Int {
    value = add(1, 2)
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
    fn definition_resolves_instance_methods_from_receiver_types() {
        let root = std::env::current_dir()
            .unwrap()
            .join("tests/fixtures/associated_function");
        let uri = path_to_uri(&root.join("main.foster")).unwrap();
        let workspace = Workspace {
            root: Some(root.clone()),
            documents: HashMap::new(),
            published: HashSet::new(),
        };

        let location = workspace
            .definition(&TextDocumentPositionParams::new(
                lsp_types::TextDocumentIdentifier::new(uri),
                Position::new(5, 13),
            ))
            .unwrap();
        assert_eq!(
            uri_to_path(&location.uri).unwrap(),
            root.join("collection.foster")
        );
        assert_eq!(location.range.start, Position::new(8, 9));
    }

    #[test]
    fn definition_opens_embedded_core_source_when_available() {
        let root = std::env::current_dir()
            .unwrap()
            .join("tests/fixtures/core_consumer");
        let uri = path_to_uri(&root.join("main.foster")).unwrap();
        let workspace = Workspace {
            root: Some(root),
            documents: HashMap::new(),
            published: HashSet::new(),
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
                .join("library/core/list.foster")
        );
        assert_eq!(location.range.start, Position::new(21, 9));
    }

    #[test]
    fn examples_compile_in_their_own_document_context() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
        let example = root.join("examples/pima/repository_analyzer.foster");
        let uri = path_to_uri(&example).unwrap();
        let workspace = Workspace {
            root: Some(root.clone()),
            documents: HashMap::new(),
            published: HashSet::new(),
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
            root.join("library/core/sequence.foster")
        );
        assert_eq!(location.range.start, Position::new(106, 9));
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
}
