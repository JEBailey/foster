//! Current-buffer tooling. No checking or lowering is allowed on this path.
use super::*;
use crate::ast::{self, BranchTest, ClosureBody, Expr, Pattern, Stmt};
use crate::lexer::TokenKind;
use std::collections::BTreeMap;

type Items = BTreeMap<String, CompletionItem>;

fn binding(items: &mut Items, name: &str, detail: Option<String>) {
    if name == "_" || name.starts_with('$') {
        return;
    }
    items.insert(
        name.into(),
        CompletionItem {
            label: name.into(),
            kind: Some(CompletionItemKind::VARIABLE),
            detail,
            sort_text: Some(format!("0_{name}")),
            ..Default::default()
        },
    );
}

fn parameters(items: &mut Items, parameters: &[ast::Parameter]) {
    for parameter in parameters {
        binding(
            items,
            &parameter.name,
            parameter.ty.as_ref().map(display_type_expr),
        );
    }
}

fn pattern(items: &mut Items, value: &Pattern) {
    match value.unspanned() {
        Pattern::Binding(name) => binding(items, name, None),
        Pattern::Record { fields } => {
            for (_, value) in fields {
                pattern(items, value);
            }
        }
        Pattern::Variant { fields, .. } => {
            for value in fields {
                pattern(items, value);
            }
        }
        _ => {}
    }
}

// Follow only the lexical path to the cursor. Initializers are visited before
// adding their binding; branch/loop/closure locals never escape their scope.
fn block(body: &crate::block::Block<Stmt>, outer: &Items, marker: &str) -> Option<Items> {
    let mut scope = outer.clone();
    for statement in body {
        let found = match statement {
            Stmt::Bind { name, value } => {
                let found = expression(value, &scope, marker);
                binding(&mut scope, name, None);
                found
            }
            Stmt::Destructure {
                pattern: value,
                value: initializer,
            } => {
                let found = expression(initializer, &scope, marker);
                pattern(&mut scope, value);
                found
            }
            Stmt::Function(function) => {
                insert_completion(
                    &mut scope,
                    &function.name,
                    CompletionItemKind::FUNCTION,
                    None,
                );
                let mut nested = scope.clone();
                parameters(&mut nested, &function.parameters);
                block(&function.body, &nested, marker)
            }
            Stmt::Loop { body } => block(body, &scope, marker),
            Stmt::Expr(value) | Stmt::Assign { value, .. } => expression(value, &scope, marker),
            Stmt::Set { place, value } => {
                expression(place, &scope, marker).or_else(|| expression(value, &scope, marker))
            }
            Stmt::Return { value, guard } => expression(value, &scope, marker).or_else(|| {
                guard
                    .as_ref()
                    .and_then(|guard| expression(guard, &scope, marker))
            }),
            Stmt::Assert { condition, message } => {
                expression(condition, &scope, marker).or_else(|| {
                    message
                        .as_ref()
                        .and_then(|value| expression(value, &scope, marker))
                })
            }
            Stmt::Break { guard } | Stmt::Continue { guard } => guard
                .as_ref()
                .and_then(|value| expression(value, &scope, marker)),
        };
        if found.is_some() {
            return found;
        }
    }
    None
}

fn expression(value: &Expr, scope: &Items, marker: &str) -> Option<Items> {
    let visit = |value| expression(value, scope, marker);
    match value.unspanned() {
        Expr::Name(name) if name == marker => Some(scope.clone()),
        Expr::List(values) => values.iter().find_map(visit),
        Expr::Call { callee, arguments } | Expr::PartialApplication { callee, arguments } => {
            visit(callee).or_else(|| arguments.iter().find_map(visit))
        }
        Expr::Member { object, .. } => visit(object),
        Expr::Qualified { namespace, .. } => visit(namespace),
        Expr::Index { object, index } => visit(object).or_else(|| visit(index)),
        Expr::Reference(value)
        | Expr::MoveOut(value)
        | Expr::Remote(value)
        | Expr::Await(value)
        | Expr::Panic(value) => visit(value),
        Expr::Try { value, .. } => visit(value),
        Expr::Unary { operand, .. } => visit(operand),
        Expr::Binary { left, right, .. } | Expr::Logical { left, right, .. } => {
            visit(left).or_else(|| visit(right))
        }
        Expr::Record {
            constructor,
            fields,
        } => visit(constructor).or_else(|| fields.iter().find_map(|field| visit(&field.value))),
        Expr::Closure {
            parameters: args,
            body,
            ..
        } => {
            let mut nested = scope.clone();
            parameters(&mut nested, args);
            match body {
                ClosureBody::Expression(value) => expression(value, &nested, marker),
                ClosureBody::Block(body) => block(body, &nested, marker),
            }
        }
        Expr::Branch { subject, arms } => {
            subject.as_ref().and_then(|value| visit(value)).or_else(|| {
                arms.iter().find_map(|arm| {
                    let mut nested = scope.clone();
                    match &arm.test {
                        BranchTest::Pattern(value) => pattern(&mut nested, value),
                        BranchTest::Condition(value) => {
                            if let Some(found) = visit(value) {
                                return Some(found);
                            }
                        }
                        BranchTest::Wildcard => {}
                    }
                    block(&arm.body, &nested, marker)
                })
            })
        }
        _ => None,
    }
}

fn declarations(program: &ast::Program, public_only: bool) -> Items {
    let mut items = Items::new();
    for function in &program.functions {
        if function.owner.is_none() && (!public_only || function.public) {
            insert_documented_completion(
                &mut items,
                &function.name,
                CompletionItemKind::FUNCTION,
                None,
                function.documentation.as_deref(),
            );
        }
    }
    for record in &program.records {
        if !public_only || record.public {
            insert_documented_completion(
                &mut items,
                &record.name,
                CompletionItemKind::STRUCT,
                None,
                record.documentation.as_deref(),
            );
        }
    }
    for variant in &program.variants {
        if !public_only || variant.public {
            insert_documented_completion(
                &mut items,
                &variant.name,
                CompletionItemKind::ENUM,
                None,
                variant.documentation.as_deref(),
            );
        }
    }
    for constant in &program.constants {
        if !public_only || constant.public {
            insert_documented_completion(
                &mut items,
                &constant.name,
                CompletionItemKind::CONSTANT,
                None,
                constant.documentation.as_deref(),
            );
        }
    }
    items
}

fn library_exports(module: &str) -> Option<&'static Items> {
    type ExportCache = BTreeMap<&'static str, (&'static str, std::sync::OnceLock<Items>)>;
    static EXPORTS: std::sync::OnceLock<ExportCache> = std::sync::OnceLock::new();
    let (source, items) = EXPORTS
        .get_or_init(|| {
            crate::package::EMBEDDED_MODULES
                .iter()
                .map(|(module, source)| (*module, (*source, std::sync::OnceLock::new())))
                .collect()
        })
        .get(module)?;
    Some(items.get_or_init(|| {
        crate::parse_recovering(source)
            .ok()
            .map_or_else(Items::new, |parsed| declarations(&parsed.program, true))
    }))
}

// Replace the fragment being typed with a valid expression, then close only
// unmatched delimiters. The real parser supplies scope and desugars iteration.
fn cursor_program(source: &str, offset: usize) -> Option<(ast::Program, String)> {
    let start = identifier_at(source, offset).map_or(offset, |(_, start)| start);
    let mut marker = "foster_completion_cursor".to_owned();
    while source.contains(&marker) {
        marker.push('_');
    }
    let mut repaired = format!("{}{marker}", &source[..start]);
    let tokens = crate::lexer::lex(&repaired).ok()?;
    // A marker swallowed by a comment/string is not a code completion position.
    if !tokens
        .iter()
        .any(|token| token.kind == TokenKind::Ident(marker.clone()))
    {
        return None;
    }
    let mut delimiters = Vec::new();
    for token in tokens {
        match token.kind {
            TokenKind::LBrace => delimiters.push('}'),
            TokenKind::LParen => delimiters.push(')'),
            TokenKind::LBracket => delimiters.push(']'),
            TokenKind::RBrace | TokenKind::RParen | TokenKind::RBracket => {
                delimiters.pop()?;
            }
            _ => {}
        }
    }
    for delimiter in delimiters.into_iter().rev() {
        repaired.push(delimiter);
    }
    Some((crate::parse_recovering(&repaired).ok()?.program, marker))
}

impl Workspace {
    fn edit_exports(
        &self,
        uri: &Uri,
        selected: Option<&HashSet<String>>,
    ) -> BTreeMap<String, Items> {
        let wanted = |name: &str| selected.is_none_or(|names| names.contains(name));
        let mut exports = crate::package::EMBEDDED_MODULES
            .iter()
            .filter(|(name, _)| wanted(name))
            .filter_map(|(name, _)| Some(((*name).to_owned(), library_exports(name)?.clone())))
            .collect::<BTreeMap<_, _>>();
        if let Some(compilation) = self.compilations.available(uri) {
            for (name, module) in &compilation.package.modules {
                if !wanted(name) {
                    continue;
                }
                let current = module
                    .source_path
                    .as_ref()
                    .and_then(|path| path_to_uri(path.as_std_path()))
                    .and_then(|uri| self.documents.get(&uri));
                let parsed =
                    current.and_then(|document| crate::parse_recovering(&document.text).ok());
                if let Some(program) = parsed.as_ref().map(|parsed| &parsed.program).or_else(|| {
                    current
                        .is_none()
                        .then_some(module.program.as_ref())
                        .flatten()
                }) {
                    exports.insert(name.clone(), declarations(program, true));
                } else if current.is_none()
                    && let Some(module) = compilation.hir.module_named(name)
                {
                    let mut items = Items::new();
                    add_module_completions(&compilation, module, true, &mut items);
                    exports.insert(name.clone(), items);
                }
            }
        }
        exports
    }

    pub(super) fn editing_completions(
        &self,
        uri: &Uri,
        source: &str,
        offset: usize,
    ) -> Option<Items> {
        let (prefix, marker) = cursor_program(source, offset)?;
        let full = crate::parse_recovering(source).ok();
        let program = full.as_ref().map_or(&prefix, |parsed| &parsed.program);
        let selected = program
            .imports
            .iter()
            .map(|import| import.path.join("."))
            .collect();
        let exports = self.edit_exports(uri, Some(&selected));
        let start = identifier_at(source, offset).map_or(offset, |(_, start)| start);
        let qualifier = qualifier_before(source, start);
        let mut items = Items::new();
        for import in &program.imports {
            let name = import.alias.as_ref().or_else(|| import.path.last())?;
            if name.starts_with('$') {
                continue;
            }
            if qualifier.as_ref().is_none_or(|qualifier| qualifier == name) {
                if let Some(exported) = exports.get(&import.path.join(".")) {
                    items.extend(exported.clone());
                }
            }
            if qualifier.is_none() {
                insert_completion(&mut items, name, CompletionItemKind::MODULE, None);
            }
        }
        if qualifier.is_some() {
            return Some(items);
        }
        for name in [
            "Never",
            "Bool",
            "Int",
            "Float",
            "String",
            "CodePoint",
            "Byte",
            "Bytes",
            "ByteBuffer",
            "Symbol",
            "List",
            "Sequence",
            "Remote",
            "Future",
        ] {
            insert_completion(&mut items, name, CompletionItemKind::CLASS, None);
        }
        items.extend(declarations(program, false));
        for function in &prefix.functions {
            let mut scope = items.clone();
            parameters(&mut scope, &function.parameters);
            for name in &function.type_parameters {
                insert_completion(&mut scope, name, CompletionItemKind::TYPE_PARAMETER, None);
            }
            if let Some(mut scope) = block(&function.body, &scope, &marker) {
                scope.remove(&marker);
                return Some(scope);
            }
        }
        for test in &prefix.tests {
            if let Some(scope) = block(&test.body, &items, &marker) {
                return Some(scope);
            }
        }
        // Recovery may discard a malformed body but preserve its parameter list.
        for function in &program.functions {
            if function.span.contains(&offset) {
                parameters(&mut items, &function.parameters);
            }
        }
        items.remove(&marker);
        Some(items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{
        CodeActionContext, CodeActionOrCommand, CodeActionParams, TextDocumentIdentifier,
    };

    fn workspace(source: &str) -> (Workspace, Uri) {
        let uri = path_to_uri(
            &std::env::current_dir()
                .unwrap()
                .join("target/editing-tools.fos"),
        )
        .unwrap();
        let mut workspace = Workspace::new(&InitializeParams::default());
        workspace.open(uri.clone(), source.into(), 7);
        workspace.compilations.snapshot_only = true;
        (workspace, uri)
    }

    fn complete(marked: &str) -> Items {
        let offset = marked.find('|').unwrap();
        let source = marked.replacen('|', "", 1);
        let (workspace, uri) = workspace(&source);
        let response = workspace
            .completion(&CompletionParams {
                text_document_position: TextDocumentPositionParams::new(
                    TextDocumentIdentifier::new(uri),
                    byte_range_to_lsp(&source, offset..offset).start,
                ),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
                context: None,
            })
            .unwrap();
        let CompletionResponse::Array(items) = response else {
            panic!("expected array");
        };
        items
            .into_iter()
            .map(|item| (item.label.clone(), item))
            .collect()
    }

    #[test]
    fn completion_uses_current_locals_in_unfinished_functions() {
        let items = complete(
            "func other(secret: Int) -> Int { secret }\nfunc edit(input: Int) -> Int {\nlet current = input\ncur|",
        );
        assert!(items.contains_key("current"));
        assert_eq!(items["input"].detail.as_deref(), Some("Int"));
        assert!(!items.contains_key("secret"));
        assert!(items.contains_key("other"));
        assert!(!items.keys().any(|name| name.contains("completion_cursor")));
        let items = complete("func edit() { let café = 1\ncafè|");
        assert!(items.contains_key("café"));
    }

    #[test]
    fn completion_respects_initializer_and_nested_scopes() {
        let items = complete(
            "func edit(input: Int) {\nloop { let hidden = 1\nbreak }\nlet outer = 1\nlet future = out|\nlet later = 2\n}",
        );
        for name in ["input", "outer"] {
            assert!(items.contains_key(name), "{name}");
        }
        for name in ["hidden", "future", "later"] {
            assert!(!items.contains_key(name), "{name}");
        }
        let items = complete(
            "func edit(input: Int) {\nlet action = (input: String) -> {\nlet nested = 1\nin|\n}\n}",
        );
        assert_eq!(items["input"].detail.as_deref(), Some("String"));
        assert!(items.contains_key("nested"));
        assert!(!items.contains_key("action"));
    }

    #[test]
    fn completion_handles_destructuring_branches_and_iteration() {
        let items = complete(
            "func edit(input: Int) {\nlet { value: renamed, nested: { child } } = input\nren|\n}",
        );
        assert!(items.contains_key("renamed"));
        assert!(items.contains_key("child"));
        assert!(!items.contains_key("value"));
        let items = complete(
            "func edit(input: Int) {\nbranch input {\nResult.Ok(found) -> { fou| }\n_ -> 0\n}\n}",
        );
        assert!(items.contains_key("found"));
        let items = complete("func edit() {\nfor item in [1, 2] {\nite|\n}\n}");
        assert!(items.contains_key("item"));
        assert!(!items.keys().any(|name| name.starts_with('$')));
    }

    #[test]
    fn completion_uses_current_declarations_and_imports_without_a_snapshot() {
        let items =
            complete("import core.option as options\nfunc edit() { Op| }\nfunc later() { 0 }");
        assert!(items.contains_key("Option"));
        assert!(items.contains_key("options"));
        assert!(items.contains_key("later"));
        let items = complete("import core.option as options\nfunc edit() { options::Op| }");
        assert!(items.contains_key("Option"));
        assert!(!items.contains_key("edit"));
        assert!(!items.contains_key("let"));
    }

    #[test]
    fn completion_does_not_suggest_code_inside_comments_or_strings() {
        for source in [
            "func edit() { // inp|\n}",
            "func edit() { /* inp| */ }",
            "func edit() { \"inp|\" }",
        ] {
            assert!(complete(source).is_empty(), "{source}");
        }
    }

    #[test]
    fn completion_replaces_old_snapshot_names_with_current_names() {
        let original = "func old_helper() -> Int { 0 }\nfunc main() -> Int {\nlet old_local = 1\nold_local\n}\n";
        let (mut workspace, uri) = workspace(original);
        workspace.compilations.snapshot_only = false;
        workspace.compile_for(&uri).unwrap();
        workspace.compilations.snapshot_only = true;
        let changed = original
            .replace("old_local", "fresh_local")
            .replace("old_helper", "new_helper");
        workspace.change(uri.clone(), changed.clone(), 8);
        let offset = changed.rfind("fresh_local").unwrap() + 3;
        let params = CompletionParams {
            text_document_position: TextDocumentPositionParams::new(
                TextDocumentIdentifier::new(uri.clone()),
                byte_range_to_lsp(&changed, offset..offset).start,
            ),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        };
        let CompletionResponse::Array(items) = workspace.completion(&params).unwrap() else {
            panic!("expected items");
        };
        assert!(items.iter().any(|item| item.label == "fresh_local"));
        assert!(items.iter().any(|item| item.label == "new_helper"));
        assert!(!items.iter().any(|item| item.label.starts_with("old_")));
        assert!(
            workspace.compile_for(&uri).is_err(),
            "completion must not check the edited source"
        );
    }

    fn actions(source: &str) -> (Workspace, CodeActionParams) {
        let (mut workspace, uri) = workspace(source);
        workspace.compilations.snapshot_only = false;
        let (sender, receiver) = crossbeam_channel::unbounded();
        workspace
            .publish_diagnostics(&sender, 0, &std::sync::atomic::AtomicU64::new(0), None)
            .unwrap();
        let diagnostics = receiver
            .try_iter()
            .find_map(|message| {
                let Message::Notification(notification) = message else {
                    return None;
                };
                let params: lsp_types::PublishDiagnosticsParams =
                    serde_json::from_value(notification.params).ok()?;
                (params.uri == uri).then_some(params.diagnostics)
            })
            .unwrap();
        workspace.compilations.snapshot_only = true;
        let range = diagnostics.first().expect("expected a diagnostic").range;
        let params = CodeActionParams {
            text_document: TextDocumentIdentifier::new(uri),
            range,
            context: CodeActionContext {
                diagnostics,
                only: None,
                trigger_kind: None,
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        (workspace, params)
    }

    fn apply(source: &str, action: &CodeActionOrCommand) -> String {
        let CodeActionOrCommand::CodeAction(action) = action else {
            panic!("expected code action");
        };
        let Some(DocumentChanges::Edits(documents)) =
            &action.edit.as_ref().unwrap().document_changes
        else {
            panic!("versioned edits required");
        };
        assert_eq!(documents[0].text_document.version, Some(7));
        let OneOf::Left(edit) = &documents[0].edits[0] else {
            panic!("expected text edit");
        };
        let mut fixed = source.to_owned();
        fixed.replace_range(
            position_to_offset(source, edit.range.start).unwrap()
                ..position_to_offset(source, edit.range.end).unwrap(),
            &edit.new_text,
        );
        fixed
    }

    #[test]
    fn spelling_fix_uses_real_diagnostics_and_utf16_ranges() {
        let source = "func main() -> Int {\nlet total = 42\nchoose(\"😀\", toatl)\n}\nfunc choose(text: String, value: Int) -> Int { value }\n";
        let (workspace, params) = actions(source);
        let actions = workspace.code_actions(&params).unwrap();
        let action = actions.iter().find(|action| matches!(action, CodeActionOrCommand::CodeAction(action) if action.title == "Change `toatl` to `total`")).unwrap_or_else(|| panic!("spelling fix: {:?}; actions: {:?}", params.context.diagnostics, actions));
        crate::compile(&apply(source, action)).unwrap();
        let mut stale = params.clone();
        stale.context.diagnostics[0].data = Some(serde_json::json!({ "fosterVersion": 6 }));
        assert!(workspace.code_actions(&stale).unwrap().is_empty());
        stale.context.only = Some(vec![lsp_types::CodeActionKind::REFACTOR]);
        assert!(workspace.code_actions(&stale).unwrap().is_empty());
    }

    #[test]
    fn missing_import_fix_preserves_documentation_and_line_endings() {
        let source = "//! Module documentation\r\n\r\n/// Entry documentation\r\nfunc main(arguments: Arguments) -> Int { 0 }\r\n";
        let (workspace, params) = actions(source);
        let fixes = workspace.code_actions(&params).unwrap();
        let action = fixes.iter().find(|action| matches!(action, CodeActionOrCommand::CodeAction(action) if action.title == "Import `Arguments` from `std.process`")).expect("import fix");
        let fixed = apply(source, action);
        assert!(fixed.contains("import std.process\r\n/// Entry documentation"));
        crate::compile(&fixed).unwrap();
    }

    #[test]
    fn type_spelling_fixes_exclude_value_names_and_compile() {
        let source = "type Parcel = { value: Int }\nfunc take(value: Pacrel) -> Int { value.value }\nfunc main() -> Int { 0 }\n";
        let (workspace, params) = actions(source);
        let fixes = workspace.code_actions(&params).unwrap();
        let action = fixes.iter().find(|action| matches!(action, CodeActionOrCommand::CodeAction(action) if action.title == "Change `Pacrel` to `Parcel`")).expect("type spelling fix");
        crate::compile(&apply(source, action)).unwrap();
    }

    #[test]
    fn quick_fixes_reject_changed_buffers_unrelated_ranges_and_other_action_kinds() {
        let source = "func main() -> Int { let total = 1\ntoatl }";
        let (mut workspace, params) = actions(source);
        assert!(!workspace.code_actions(&params).unwrap().is_empty());
        let mut other = params.clone();
        other.context.only = Some(vec![lsp_types::CodeActionKind::REFACTOR]);
        assert!(workspace.code_actions(&other).unwrap().is_empty());
        other = params.clone();
        other.range = lsp_types::Range::default();
        assert!(workspace.code_actions(&other).unwrap().is_empty());
        workspace.change(
            params.text_document.uri.clone(),
            source.replace("total", "amount"),
            8,
        );
        assert!(workspace.code_actions(&params).unwrap().is_empty());
    }

    #[test]
    fn import_insertion_keeps_trailing_comments_and_ignores_synthetic_imports() {
        let source = "import core.option // keep this comment\r\n/// Main documentation\r\nfunc main() {}\r\n";
        let program = crate::parse(source).unwrap();
        let edit = import_edit(source, &program, "std.process").unwrap();
        assert_eq!(edit.range.start, Position::new(1, 0));
        assert_eq!(edit.new_text, "import std.process\r\n");
        let source = "func main() { for item in [1] { item } }";
        let program = crate::parse(source).unwrap();
        assert_eq!(
            import_edit(source, &program, "std.process")
                .unwrap()
                .range
                .start,
            Position::new(0, 0)
        );
    }
}

fn type_candidate(item: &CompletionItem) -> bool {
    matches!(
        item.kind,
        Some(
            CompletionItemKind::STRUCT
                | CompletionItemKind::ENUM
                | CompletionItemKind::CLASS
                | CompletionItemKind::INTERFACE
                | CompletionItemKind::TYPE_PARAMETER
        )
    )
}

// A conservative spelling suggestion: one insertion/deletion/substitution or
// adjacent transposition. Short names require a transposition to avoid noisy fixes.
fn similar(left: &str, right: &str) -> bool {
    let left = left.chars().collect::<Vec<_>>();
    let right = right.chars().collect::<Vec<_>>();
    if left == right || left.len().abs_diff(right.len()) > 1 {
        return false;
    }
    if left.len() == right.len() {
        let changed = (0..left.len())
            .filter(|index| left[*index] != right[*index])
            .collect::<Vec<_>>();
        return (left.len() >= 3 && changed.len() == 1)
            || (changed.len() == 2
                && changed[1] == changed[0] + 1
                && left[changed[0]] == right[changed[1]]
                && left[changed[1]] == right[changed[0]]);
    }
    let (short, long) = if left.len() < right.len() {
        (&left, &right)
    } else {
        (&right, &left)
    };
    if short.len() < 3 {
        return false;
    }
    let split = short
        .iter()
        .zip(long)
        .position(|(a, b)| a != b)
        .unwrap_or(short.len());
    short[split..] == long[split + 1..]
}

fn import_edit(source: &str, program: &ast::Program, module: &str) -> Option<TextEdit> {
    let newline = if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let (offset, prefix) = if let Some(import) = program
        .imports
        .iter()
        .filter(|import| {
            !import
                .alias
                .as_ref()
                .is_some_and(|name| name.starts_with('$'))
        })
        .last()
    {
        match source[import.span.end..].find('\n') {
            Some(end) => (import.span.end + end + 1, ""),
            None => (source.len(), newline),
        }
    } else {
        // Keep module documentation and ordinary leading comments in place, but
        // insert before declaration documentation so it stays with its declaration.
        let tokens = crate::lexer::lex(source).ok()?;
        let token = tokens.iter().find(|token| {
            !matches!(
                token.kind,
                TokenKind::Newline | TokenKind::ModuleDocComment(_)
            )
        })?;
        let line_start = source[..token.range.start]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        (line_start, "")
    };
    Some(TextEdit {
        range: byte_range_to_lsp(source, offset..offset),
        new_text: format!("{prefix}import {module}{newline}"),
    })
}

impl Workspace {
    pub(in crate::lsp) fn code_actions(
        &self,
        params: &lsp_types::CodeActionParams,
    ) -> Option<lsp_types::CodeActionResponse> {
        use lsp_types::{CodeAction, CodeActionKind, CodeActionOrCommand};
        if params.context.only.as_ref().is_some_and(|kinds| {
            !kinds
                .iter()
                .any(|kind| kind.as_str().is_empty() || kind == &CodeActionKind::QUICKFIX)
        }) {
            return Some(Vec::new());
        }
        let uri = &params.text_document.uri;
        let document = self.documents.get(uri)?;
        let source = &document.text;
        let parsed = crate::parse_recovering(source).ok()?;
        let tokens = crate::lexer::lex(source).ok()?;
        let mut actions = Vec::new();
        let mut seen = HashSet::new();
        let mut exports = None;
        for diagnostic in &params.context.diagnostics {
            if diagnostic.source.as_deref() != Some("foster")
                || diagnostic
                    .data
                    .as_ref()
                    .and_then(|data| data.get("fosterVersion"))
                    .and_then(serde_json::Value::as_i64)
                    != Some(i64::from(document.version))
                || diagnostic.range.end < params.range.start
                || params.range.end < diagnostic.range.start
            {
                continue;
            }
            let message = diagnostic.message.lines().next().unwrap_or_default();
            let message = if message.starts_with("in `") {
                message
                    .split_once("`: ")
                    .map_or(message, |(_, message)| message)
            } else {
                message
            };
            let (name, is_type) = if let Some(name) = message
                .strip_prefix("unknown name `")
                .and_then(|name| name.strip_suffix('`'))
            {
                (name, false)
            } else if let Some(name) = message
                .strip_prefix("unknown type `")
                .and_then(|name| name.strip_suffix('`'))
            {
                (name, true)
            } else {
                continue;
            };
            let matches = tokens
                .iter()
                .filter(|token| {
                    token.kind == TokenKind::Ident(name.into()) && {
                        let range = byte_range_to_lsp(source, token.range.clone());
                        range.start <= diagnostic.range.end && diagnostic.range.start < range.end
                    }
                })
                .collect::<Vec<_>>();
            let [token] = matches.as_slice() else {
                continue;
            };
            let range = byte_range_to_lsp(source, token.range.clone());
            let mut offer = |title: String, edit: TextEdit| {
                if !seen.insert((title.clone(), range.start.line, range.start.character)) {
                    return;
                }
                actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                    title,
                    kind: Some(CodeActionKind::QUICKFIX),
                    diagnostics: Some(vec![diagnostic.clone()]),
                    edit: Some(WorkspaceEdit {
                        document_changes: Some(DocumentChanges::Edits(vec![TextDocumentEdit {
                            text_document: OptionalVersionedTextDocumentIdentifier {
                                uri: uri.clone(),
                                version: Some(document.version),
                            },
                            edits: vec![OneOf::Left(edit)],
                        }])),
                        ..Default::default()
                    }),
                    ..Default::default()
                }));
            };
            if let Some(candidates) = self.editing_completions(uri, source, token.range.end) {
                for item in candidates
                    .values()
                    .filter(|item| (!is_type || type_candidate(item)) && similar(name, &item.label))
                    .take(5)
                {
                    offer(
                        format!("Change `{name}` to `{}`", item.label),
                        TextEdit {
                            range,
                            new_text: item.label.clone(),
                        },
                    );
                }
            }
            for (module, members) in exports
                .get_or_insert_with(|| self.edit_exports(uri, None))
                .iter()
            {
                if !members
                    .get(name)
                    .is_some_and(|item| !is_type || type_candidate(item))
                {
                    continue;
                }
                let alias = module.rsplit('.').next().unwrap_or(module);
                if parsed.program.imports.iter().any(|import| {
                    import.path.join(".") == *module
                        || import
                            .alias
                            .as_ref()
                            .or_else(|| import.path.last())
                            .is_some_and(|name| name == alias)
                }) {
                    continue;
                }
                if let Some(edit) = import_edit(source, &parsed.program, module) {
                    offer(format!("Import `{name}` from `{module}`"), edit);
                }
            }
        }
        Some(actions)
    }
}
