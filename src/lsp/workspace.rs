use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use camino::Utf8PathBuf;
use lsp_server::Connection;
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionParams, CompletionResponse, Diagnostic,
    DiagnosticSeverity, DocumentChanges, DocumentSymbol, DocumentSymbolResponse, Documentation,
    Hover, HoverContents, InitializeParams, Location, MarkupContent, MarkupKind, NumberOrString,
    OneOf, OptionalVersionedTextDocumentIdentifier, Position, ReferenceParams, RenameParams,
    SymbolKind, TextDocumentEdit, TextDocumentPositionParams, TextEdit, Uri, WorkspaceEdit,
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
        let mut next = Vec::<(Uri, Vec<Diagnostic>, Option<i32>)>::new();
        match self.compile() {
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
                    next.push((uri.clone(), diagnostics, self.version(&uri)));
                }
            }
            Err(error) => {
                let target = self.documents.iter().find(|(uri, _)| {
                    uri_to_path(uri).is_some_and(|path| {
                        let native = path.to_string_lossy();
                        let portable = native.replace('\\', "/");
                        error.message.contains(native.as_ref()) || error.message.contains(&portable)
                    })
                });
                if let Some((uri, document)) = target.or_else(|| self.documents.iter().next()) {
                    next.push((
                        uri.clone(),
                        vec![error_diagnostic(&document.text, error)],
                        Some(document.version),
                    ));
                }
            }
        }

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
        let compilation = self.compile().ok()?;
        let module_id = module_for_uri(&compilation, uri)?;
        let module = &compilation.hir.modules[module_id];
        let source = compilation
            .package
            .module(&module.name)?
            .source
            .as_deref()?;
        let mut symbols = Vec::new();
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
        let compilation = self.compile().ok()?;
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
        let compilation = self.compile().ok()?;
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
            format!("{}: {}", name, compilation.types.display(ty))
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
                value: format!("```foster\n{value}\n```"),
            }),
            range: Some(range),
        })
    }

    pub(super) fn completion(&self, params: &CompletionParams) -> Option<CompletionResponse> {
        let compilation = self.compile().ok()?;
        let position = &params.text_document_position;
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
        let compilation = self.compile().ok()?;
        let position = &params.text_document_position;
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
        let compilation = self.compile().ok()?;
        let position = &params.text_document_position;
        let module = module_for_uri(&compilation, &position.text_document.uri)?;
        let source = compilation
            .package
            .module(&compilation.hir.modules[module].name)?
            .source
            .as_deref()?;
        let offset = position_to_offset(source, position.position)?;
        let symbol = symbol_at(&compilation, module, source, offset)?;
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

    fn compile(&self) -> Result<crate::hir::Compilation, crate::error::FosterError> {
        let root = self.root.as_deref().ok_or_else(|| {
            crate::error::FosterError::runtime("language server has no workspace root")
        })?;
        let overlays = self
            .documents
            .iter()
            .filter_map(|(uri, document)| {
                let path = uri_to_path(uri)?;
                let path = Utf8PathBuf::from_path_buf(path).ok()?;
                Some((path, document.text.clone()))
            })
            .collect();
        let package = crate::package::Package::load_with_overlays(root, &overlays)?;
        crate::hir::Compilation::new(package)
    }

    fn version(&self, uri: &Uri) -> Option<i32> {
        self.documents.get(uri).map(|document| document.version)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SymbolIdentity {
    Local(crate::hir::LocalId),
    Function(crate::hir::FunctionId),
    Record(crate::hir::RecordId),
    Variant(crate::hir::VariantTypeId),
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
    if let Some(expression) = expression_at(compilation, module_id, offset)
        && let crate::hir::Expr::Name(resolved) = compilation.hir.expressions[expression]
    {
        match resolved {
            crate::hir::ResolvedName::Local(local) => return Some(SymbolIdentity::Local(local)),
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
            crate::hir::ResolvedName::Module(_) | crate::hir::ResolvedName::Builtin(_) => {}
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
            format!(
                "{}: {}",
                compilation.hir.locals[local].name,
                compilation.types.display(ty)
            )
        }),
        SymbolIdentity::Function(function) => declaration_hover(
            compilation,
            compilation.hir.functions[function].module,
            &compilation.hir.functions[function].name,
        ),
        SymbolIdentity::Record(record) => {
            let definition = &compilation.hir.records[record];
            Some(documented_hover(
                format!("type {}", definition.name),
                definition.documentation.as_deref(),
            ))
        }
        SymbolIdentity::Variant(variant) => {
            let definition = &compilation.hir.variant_types[variant];
            Some(documented_hover(
                format!("type {}", definition.name),
                definition.documentation.as_deref(),
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
        .functions
        .get(name)
        .copied()
        .map(SymbolIdentity::Function)
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
        SymbolIdentity::Function(function) => &compilation.hir.functions[function].name,
        SymbolIdentity::Record(record) => &compilation.hir.records[record].name,
        SymbolIdentity::Variant(variant) => &compilation.hir.variant_types[variant].name,
    }
}

fn symbol_declaration(
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
    if let Some(function) = definitions.functions.get(name) {
        let signature = compilation.types.function_type(*function)?;
        let parameters = signature
            .parameters
            .iter()
            .map(|ty| compilation.types.display(*ty))
            .collect::<Vec<_>>()
            .join(", ");
        let definition = &compilation.hir.functions[*function];
        return Some(documented_hover(
            format!(
                "func {name}({parameters}) -> {}",
                compilation.types.display(signature.result)
            ),
            definition.documentation.as_deref(),
        ));
    }
    if let Some(record) = definitions.records.get(name) {
        return Some(documented_hover(
            format!("type {name}"),
            compilation.hir.records[*record].documentation.as_deref(),
        ));
    }
    if let Some(variant) = definitions.variant_types.get(name) {
        return Some(documented_hover(
            format!("type {name}"),
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

fn documented_hover(signature: String, documentation: Option<&str>) -> String {
    documentation.map_or(signature.clone(), |documentation| {
        format!("```foster\n{signature}\n```\n\n{documentation}")
    })
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
        .functions
        .get(name)
        .map(|id| compilation.hir.functions[*id].span.clone())
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

fn module_for_uri(
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

fn position_to_offset(source: &str, position: Position) -> Option<usize> {
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

fn path_to_uri(path: &Path) -> Option<Uri> {
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
        assert!(function.value.contains("func parse(String) -> String"));
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
        assert!(hover.value.contains("func documented(Int) -> Int"));
        assert!(hover.value.contains("Computes the documented answer."));

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
}
