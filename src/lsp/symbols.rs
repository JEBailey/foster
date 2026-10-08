//! A syntax-only disk index, refreshed by the checking worker. Queries read its
//! immutable publication and overlay open buffers without walking the filesystem.
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use lsp_types::{InitializeParams, Location, SymbolInformation, SymbolKind, Uri};

use super::workspace::{path_to_uri, uri_to_path};

pub(super) type SymbolFiles = HashMap<Uri, Arc<Vec<SymbolInformation>>>;

#[derive(Default)]
pub(super) struct SymbolIndex {
    roots: Vec<PathBuf>,
    files: HashMap<PathBuf, (SystemTime, u64, Arc<Vec<SymbolInformation>>)>,
}

impl SymbolIndex {
    pub(super) fn configure(&mut self, params: &InitializeParams) {
        self.roots = params
            .workspace_folders
            .as_ref()
            .into_iter()
            .flatten()
            .filter_map(|folder| uri_to_path(&folder.uri))
            .collect();
        #[allow(deprecated)]
        if self.roots.is_empty()
            && let Some(root) = params.root_uri.as_ref().and_then(uri_to_path)
        {
            self.roots.push(root);
        }
    }

    pub(super) fn invalidate(&mut self, paths: &[PathBuf]) {
        self.files.retain(|file, _| !affected(file, paths));
    }

    pub(super) fn refresh(&mut self) -> Option<SymbolFiles> {
        let mut seen = HashSet::new();
        for root in &self.roots {
            for entry in walkdir::WalkDir::new(root)
                .follow_links(false)
                .into_iter()
                .filter_entry(|entry| {
                    entry.depth() == 0
                        || !entry.file_type().is_dir()
                        || !matches!(
                            entry.file_name().to_str(),
                            Some(".git" | ".codex" | "node_modules" | "target" | "dist")
                        )
                })
            {
                if crate::compiler::cancellation::is_cancelled() {
                    return None;
                }
                let Ok(entry) = entry else {
                    continue;
                };
                if !entry.file_type().is_file()
                    || entry
                        .path()
                        .extension()
                        .is_none_or(|extension| extension != "fos")
                {
                    continue;
                }
                let path = entry.path().to_owned();
                if !seen.insert(path.clone()) {
                    continue;
                }
                let Some((modified, len)) = stamp(&path) else {
                    self.files.remove(&path);
                    continue;
                };
                if self
                    .files
                    .get(&path)
                    .is_some_and(|(old, size, _)| *old == modified && *size == len)
                {
                    continue;
                }
                let Some(uri) = path_to_uri(&path) else {
                    continue;
                };
                let Ok(source) = std::fs::read_to_string(&path) else {
                    self.files.remove(&path);
                    continue;
                };
                let symbols = source_symbols(&uri, &source);
                if stamp(&path) != Some((modified, len)) {
                    self.files.remove(&path);
                    continue;
                }
                self.files.insert(path, (modified, len, Arc::new(symbols)));
            }
        }
        self.files.retain(|path, _| seen.contains(path));
        Some(
            self.files
                .iter()
                .filter_map(|(path, (_, _, symbols))| {
                    Some((path_to_uri(path)?, Arc::clone(symbols)))
                })
                .collect(),
        )
    }
}

fn stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}

pub(super) fn affected(file: &Path, paths: &[PathBuf]) -> bool {
    let file = crate::package::watch_path(file);
    paths
        .iter()
        .any(|path| file.starts_with(crate::package::watch_path(path)))
}

pub(super) fn source_symbols(uri: &Uri, source: &str) -> Vec<SymbolInformation> {
    let Ok(tokens) = crate::lexer::lex(source) else {
        return Vec::new();
    };
    let program = crate::parser::parse_recovering(tokens.clone()).program;
    let mut symbols = Vec::new();
    let file = uri_to_path(uri).and_then(|path| {
        path.file_stem()
            .map(|name| name.to_string_lossy().into_owned())
    });
    let mut add =
        |name: &str, owner: Option<&str>, kind: SymbolKind, span: &std::ops::Range<usize>| {
            let leaf = name.rsplit('.').next().unwrap_or(name);
            let Some(token) = tokens.iter().find(|token| {
                span.start <= token.range.start
                    && token.range.end <= span.end
                    && matches!(&token.kind, crate::lexer::TokenKind::Ident(name) if name == leaf)
            }) else {
                return;
            };
            #[allow(deprecated)]
            symbols.push(SymbolInformation {
                name: name.into(),
                kind,
                tags: None,
                deprecated: None,
                location: Location::new(
                    uri.clone(),
                    super::byte_range_to_lsp(source, token.range.clone()),
                ),
                container_name: owner.map(str::to_owned).or_else(|| file.clone()),
            });
        };
    for value in &program.constants {
        add(&value.name, None, SymbolKind::CONSTANT, &value.span);
    }
    for value in &program.records {
        add(&value.name, None, SymbolKind::STRUCT, &value.span);
        for method in &value.methods {
            add(
                &format!("{}.{}", value.name, method.name),
                Some(&value.name),
                SymbolKind::METHOD,
                &method.span,
            );
        }
    }
    for value in &program.variants {
        add(
            &value.name,
            None,
            if value.kind == crate::ast::VariantKind::Enum {
                SymbolKind::ENUM
            } else {
                SymbolKind::INTERFACE
            },
            &value.span,
        );
        for alternative in &value.alternatives {
            if let crate::ast::VariantAlternative::EnumCase { name, span, .. } = alternative {
                add(
                    &format!("{}.{name}", value.name),
                    Some(&value.name),
                    SymbolKind::ENUM_MEMBER,
                    span,
                );
            }
        }
        for method in &value.methods {
            add(
                &format!("{}.{}", value.name, method.name),
                Some(&value.name),
                SymbolKind::METHOD,
                &method.span,
            );
        }
    }
    for value in &program.functions {
        add(
            &value.name,
            value.owner.as_deref(),
            if value.receiver {
                SymbolKind::METHOD
            } else {
                SymbolKind::FUNCTION
            },
            &value.span,
        );
    }
    symbols
}

pub(super) fn rank(symbol: &SymbolInformation, query: &str) -> Option<u8> {
    let name = symbol.name.to_lowercase();
    let query = query.trim().to_lowercase();
    if query.is_empty() || name == query {
        return Some(0);
    }
    if name.starts_with(&query) {
        return Some(1);
    }
    if name.contains(&query) {
        return Some(2);
    }
    let mut letters = name.chars();
    if query
        .chars()
        .all(|letter| letters.by_ref().any(|candidate| candidate == letter))
    {
        return Some(3);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::current_dir()
                .unwrap()
                .join("target")
                .join(format!("lsp-symbol-index-{}-{id}", std::process::id()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let root = self.0.canonicalize().unwrap();
            let target = std::env::current_dir()
                .unwrap()
                .join("target")
                .canonicalize()
                .unwrap();
            assert_eq!(root.parent(), Some(target.as_path()));
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn background_index_covers_unopened_files_multiple_roots_and_changes() {
        let fixture = Fixture::new();
        let first = fixture.0.join("one");
        let second = fixture.0.join("two");
        std::fs::create_dir_all(first.join("target")).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let path = first.join("main.fos");
        std::fs::write(&path, "func unopened() { 0 }").unwrap();
        std::fs::write(first.join("target/ignored.fos"), "func generated() { 0 }").unwrap();
        std::fs::write(second.join("other.fos"), "type Other = { value: Int }").unwrap();
        let mut index = SymbolIndex::default();
        index.configure(
            &serde_json::from_value(serde_json::json!({
                "capabilities": {},
                "workspaceFolders": [
                    {"uri": path_to_uri(&first).unwrap(), "name": "first"},
                    {"uri": path_to_uri(&second).unwrap(), "name": "second"}
                ]
            }))
            .unwrap(),
        );
        let uri = path_to_uri(&path).unwrap();
        let mut entries = index.refresh().unwrap().into_iter();
        assert_eq!(entries.len(), 2);
        let original = entries.find(|(key, _)| key == &uri).unwrap().1;
        assert_eq!(original[0].name, "unopened");
        assert!(Arc::ptr_eq(&original, &index.refresh().unwrap()[&uri]));
        index.invalidate(std::slice::from_ref(&path));
        assert!(!Arc::ptr_eq(&original, &index.refresh().unwrap()[&uri]));
        std::fs::write(&path, "func renamed() { 0 }").unwrap();
        index.invalidate(std::slice::from_ref(&path));
        assert_eq!(index.refresh().unwrap()[&uri][0].name, "renamed");
        std::fs::remove_file(&path).unwrap();
        std::fs::write(first.join("new.fos"), "const Answer = 42").unwrap();
        assert!(!index.refresh().unwrap().contains_key(&uri));
        assert!(
            index
                .refresh()
                .unwrap()
                .values()
                .any(|values| values.iter().any(|value| value.name == "Answer"))
        );
    }

    #[test]
    fn symbols_include_methods_enum_cases_and_recoverable_declarations() {
        let uri = "file:///symbols.fos".parse().unwrap();
        let source = "/// value is documented\ntype Box = { value: Int }\nimpl Box { func value(self: Self) -> Int { self.value } }\nenum Outcome = Good(Int) | Bad\nconst Answer = 42\nfunc broken() { let x = }\nfunc healthy() -> Int { 1 }\n";
        let symbols = source_symbols(&uri, source);
        for (name, kind) in [
            ("Box", SymbolKind::STRUCT),
            ("Box.value", SymbolKind::METHOD),
            ("Outcome.Good", SymbolKind::ENUM_MEMBER),
            ("Answer", SymbolKind::CONSTANT),
            ("healthy", SymbolKind::FUNCTION),
        ] {
            let symbol = symbols
                .iter()
                .find(|symbol| symbol.name == name)
                .unwrap_or_else(|| panic!("missing {name}"));
            assert_eq!(symbol.kind, kind);
            assert_eq!(symbol.location.uri, uri);
        }
        assert!(!symbols.iter().any(|symbol| symbol.name == "x"));
        let method = symbols
            .iter()
            .find(|symbol| symbol.name == "Box.value")
            .unwrap();
        assert_eq!(method.container_name.as_deref(), Some("Box"));
        assert_eq!(method.location.range.start.line, 2);
    }

    #[test]
    fn cancelled_index_does_not_publish_partial_results() {
        let fixture = Fixture::new();
        std::fs::write(fixture.0.join("a.fos"), "func value() { 1 }").unwrap();
        let mut index = SymbolIndex {
            roots: vec![fixture.0.clone()],
            ..Default::default()
        };
        assert!(crate::compiler::cancellation::scope(|| true, || index.refresh()).is_none());
        assert_eq!(index.refresh().unwrap().len(), 1);
    }
}
