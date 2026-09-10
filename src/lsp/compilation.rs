use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::rc::Rc;

use camino::Utf8PathBuf;
use lsp_types::Uri;

use super::workspace::{Workspace, path_to_uri, uri_to_path};
use crate::compiler::Compilation;
use crate::error::FosterError;

// Navigation can request files that are not open in the editor. Keep a small working set
// for those requests, without retaining every package visited during the session.
const RECENT_DOCUMENT_LIMIT: usize = 8;

#[derive(Default)]
pub(super) struct CompilationCache {
    recent: RefCell<VecDeque<Uri>>,
    entries: RefCell<HashMap<Uri, Rc<Compilation>>>,
    errors: RefCell<HashMap<Uri, FosterError>>,
    last_good: RefCell<HashMap<Uri, Rc<Compilation>>>,
    modules: RefCell<crate::package::ModuleCache>,
    bodies: RefCell<HashMap<Utf8PathBuf, crate::typecheck::incremental::SharedBodyCache>>,
}

impl CompilationCache {
    pub(super) fn parse_document(
        &self,
        path: &Path,
        source: &str,
    ) -> Result<crate::ast::Program, FosterError> {
        let path = Utf8PathBuf::from_path_buf(path.to_owned())
            .map_err(|_| FosterError::runtime("source path is not valid UTF-8"))?;
        crate::compiler::profile::request("parse_document", path.as_str(), || {
            self.modules.borrow_mut().parse_source(&path, source)
        })
    }

    pub(super) fn clear(&self) {
        // Watched-file changes can remove packages or dependencies. No snapshot or incremental
        // cache from the old membership may remain available as a semantic fallback.
        self.entries.borrow_mut().clear();
        self.errors.borrow_mut().clear();
        self.last_good.borrow_mut().clear();
        self.bodies.borrow_mut().clear();
        self.recent.borrow_mut().clear();
        *self.modules.borrow_mut() = crate::package::ModuleCache::default();
    }

    pub(super) fn close<'a>(&self, uri: &Uri, open: impl Iterator<Item = &'a Uri>) {
        self.recent.borrow_mut().retain(|cached| cached != uri);
        self.invalidate(uri);
        // Other open files must not fall back to a snapshot containing the closed overlay.
        self.last_good
            .borrow_mut()
            .retain(|cached_uri, compilation| {
                cached_uri != uri && !contains_document(compilation, uri)
            });
        self.prune(open);
    }

    fn prune<'a>(&self, open: impl Iterator<Item = &'a Uri>) {
        let mut open = open.cloned().collect::<Vec<_>>();
        open.extend(self.recent.borrow().iter().cloned());
        let active = open
            .iter()
            .map(|uri| uri.as_str())
            .collect::<std::collections::HashSet<_>>();
        self.entries
            .borrow_mut()
            .retain(|uri, _| active.contains(uri.as_str()));
        self.last_good
            .borrow_mut()
            .retain(|uri, _| active.contains(uri.as_str()));
        self.errors
            .borrow_mut()
            .retain(|uri, _| active.contains(uri.as_str()));
        let mut paths = open
            .iter()
            .filter_map(uri_to_path)
            .filter_map(|path| Utf8PathBuf::from_path_buf(path).ok())
            .collect::<std::collections::HashSet<_>>();
        let mut roots = std::collections::HashSet::new();
        for compilation in self
            .entries
            .borrow()
            .values()
            .chain(self.last_good.borrow().values())
        {
            roots.insert(compilation.package.root.clone());
            paths.extend(
                compilation
                    .hir
                    .modules
                    .iter()
                    .filter_map(|(_, module)| module.source_path.clone()),
            );
        }
        self.bodies
            .borrow_mut()
            .retain(|root, _| roots.contains(root));
        self.modules.borrow_mut().retain_sources(&paths);
    }

    fn touch(&self, uri: &Uri) {
        let mut recent = self.recent.borrow_mut();
        recent.retain(|cached| cached != uri);
        recent.push_back(uri.clone());
        if recent.len() > RECENT_DOCUMENT_LIMIT {
            recent.pop_front();
        }
    }

    pub(super) fn invalidate(&self, uri: &Uri) {
        // A dependency can belong to several snapshots; its URI entry only names the latest.
        // Check every snapshot's module membership rather than following that single alias.
        self.entries.borrow_mut().retain(|cached_uri, compilation| {
            cached_uri != uri && !contains_document(compilation, uri)
        });
        // Failed compilations are keyed by the document that requested them rather than by a
        // resolved package, so conservatively discard these small entries on any source change.
        self.errors.borrow_mut().clear();
    }

    fn get(&self, uri: &Uri) -> Option<Rc<Compilation>> {
        self.entries.borrow().get(uri).cloned()
    }

    fn last_good(&self, uri: &Uri) -> Option<Rc<Compilation>> {
        self.last_good.borrow().get(uri).cloned()
    }

    fn error(&self, uri: &Uri) -> Option<FosterError> {
        self.errors.borrow().get(uri).cloned()
    }

    fn insert_error(&self, uri: Uri, error: FosterError) {
        self.errors.borrow_mut().insert(uri, error);
    }

    fn insert(&self, uri: Uri, compilation: Compilation) -> Rc<Compilation> {
        let compilation = Rc::new(compilation);
        let mut entries = self.entries.borrow_mut();
        let mut errors = self.errors.borrow_mut();
        let mut last_good = self.last_good.borrow_mut();
        entries.insert(uri.clone(), Rc::clone(&compilation));
        errors.remove(&uri);
        last_good.insert(uri, Rc::clone(&compilation));
        for (_, module) in compilation.hir.modules.iter() {
            let Some(path) = module.source_path.as_deref() else {
                continue;
            };
            let Some(uri) = path_to_uri(path.as_std_path()) else {
                continue;
            };
            entries.insert(uri.clone(), Rc::clone(&compilation));
            errors.remove(&uri);
            last_good.insert(uri, Rc::clone(&compilation));
        }
        compilation
    }

    #[cfg(test)]
    pub(super) fn has_cached_error(&self, uri: &Uri) -> bool {
        self.errors.borrow().contains_key(uri)
    }

    #[cfg(test)]
    pub(super) fn module_parse_count(&self, path: &Path) -> usize {
        let Ok(path) = Utf8PathBuf::from_path_buf(path.to_owned()) else {
            return 0;
        };
        self.modules.borrow().source_parse_count(&path)
    }

    pub(super) fn parse_diagnostics(&self, path: &Path) -> Vec<FosterError> {
        let Ok(path) = Utf8PathBuf::from_path_buf(path.to_owned()) else {
            return Vec::new();
        };
        self.modules.borrow().source_diagnostics(&path)
    }
}

fn contains_document(compilation: &Compilation, uri: &Uri) -> bool {
    compilation.hir.modules.iter().any(|(_, module)| {
        module
            .source_path
            .as_deref()
            .and_then(|path| path_to_uri(path.as_std_path()))
            .as_ref()
            == Some(uri)
    })
}

impl Workspace {
    fn check_incremental(
        &self,
        package: crate::package::Package,
    ) -> Result<Compilation, FosterError> {
        let cache = self
            .compilations
            .bodies
            .borrow_mut()
            .entry(package.root.clone())
            .or_default()
            .clone();
        crate::compiler::profile::measure("compiler.recovering", || {
            crate::compiler::check_recovering_cached(package, cache)
        })
    }
    pub(super) fn compile_for(&self, uri: &Uri) -> Result<Rc<Compilation>, FosterError> {
        crate::compiler::profile::request("compile_for", uri.as_str(), || {
            self.compile_profiled(uri)
        })
    }

    fn compile_profiled(&self, uri: &Uri) -> Result<Rc<Compilation>, FosterError> {
        crate::compiler::cancellation::check()?;
        self.compilations.touch(uri);
        if let Some(compilation) = self.compilations.get(uri) {
            crate::compiler::profile::count("compilation.hit");
            return Ok(compilation);
        }
        if let Some(error) = self.compilations.error(uri) {
            crate::compiler::profile::count("compilation.error_hit");
            return Err(error);
        }
        crate::compiler::profile::count("compilation.miss");
        match crate::compiler::profile::measure("compilation.rebuild", || {
            self.compile_uncached(uri)
        }) {
            Ok(compilation) => {
                crate::compiler::cancellation::check()?;
                let compilation = self.compilations.insert(uri.clone(), compilation);
                self.compilations.prune(self.documents.keys());
                Ok(compilation)
            }
            Err(error) => {
                if crate::compiler::cancellation::is_cancellation(&error) {
                    return Err(error);
                }
                self.compilations.insert_error(uri.clone(), error.clone());
                self.compilations.prune(self.documents.keys());
                Err(error)
            }
        }
    }

    pub(super) fn semantic_compilation_for(&self, uri: &Uri) -> Option<Rc<Compilation>> {
        self.compile_for(uri)
            .ok()
            .or_else(|| self.compilations.last_good(uri))
    }

    fn compile_uncached(&self, uri: &Uri) -> Result<Compilation, FosterError> {
        // Editor features intentionally use the same checked frontend as `foster check`. Executable
        // SSA sealing, bytecode optimization, and native subset validation belong to build/run and
        // do not require an editor document to declare an executable entry point.
        let path = uri_to_path(uri)
            .ok_or_else(|| FosterError::runtime("language server document is not a file URI"))?;
        let overlays = self
            .documents
            .iter()
            .filter_map(|(uri, document)| {
                let path = uri_to_path(uri)?;
                let path = Utf8PathBuf::from_path_buf(path).ok()?;
                Some((path, document.text.clone()))
            })
            .collect::<HashMap<_, _>>();

        if let Some(project) = crate::project::Project::discover(&path, self.root.as_deref())?
            && path.starts_with(&project.source_root)
        {
            let package = crate::compiler::profile::measure("package.load", || {
                crate::package::Package::load_project_with_overlays_cached(
                    &project,
                    &overlays,
                    &mut self.compilations.modules.borrow_mut(),
                )
            })?;
            if package.modules.values().any(|module| {
                module
                    .source_path
                    .as_ref()
                    .is_some_and(|source| source.as_std_path() == path)
            }) {
                return self.check_incremental(package);
            }
        }

        let standalone = self.compile_standalone(&path, &overlays);
        crate::compiler::cancellation::check()?;
        if standalone.is_ok() {
            return standalone;
        }

        let mut candidate = path.parent();
        while let Some(root) = candidate {
            if self
                .root
                .as_deref()
                .is_some_and(|workspace| !root.starts_with(workspace))
            {
                break;
            }
            if let Ok(package) = crate::compiler::profile::measure("package.load", || {
                crate::package::Package::load_with_overlays_cached(
                    root,
                    &overlays,
                    &mut self.compilations.modules.borrow_mut(),
                )
            }) && package.modules.values().any(|module| {
                module
                    .source_path
                    .as_ref()
                    .is_some_and(|source| source.as_std_path() == path)
            }) {
                return self.check_incremental(package);
            }
            if self
                .root
                .as_deref()
                .is_some_and(|workspace| root == workspace)
            {
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
    ) -> Result<Compilation, FosterError> {
        let source_path = Utf8PathBuf::from_path_buf(path.to_path_buf()).map_err(|path| {
            FosterError::runtime(format!(
                "source path is not valid UTF-8: `{}`",
                path.display()
            ))
        })?;
        let source = overlays.get(&source_path).cloned().map_or_else(
            || {
                std::fs::read_to_string(path).map_err(|error| {
                    FosterError::runtime(format!("cannot read `{}`: {error}", path.display()))
                })
            },
            Ok,
        )?;
        let mut modules = self.compilations.modules.borrow_mut();
        let program = modules.parse_source(&source_path, &source)?;
        let module_name = path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("main")
            .to_owned();
        let mut package = crate::compiler::profile::measure("package.load", || {
            crate::package::Package::from_program_with_core_cached(
                &module_name,
                program,
                &mut modules,
            )
        })?;
        let module = package
            .modules
            .get_mut(&module_name)
            .expect("standalone package contains its source module");
        module.source_path = Some(source_path.clone());
        module.source = Some(source);
        // Distinguish standalone roots so unrelated open files do not evict each other's bodies.
        package.root = source_path;
        self.check_incremental(package)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(path: &Path) -> Compilation {
        let mut compilation = crate::compile("func main() -> Int { 1 }").unwrap();
        let path = Utf8PathBuf::from_path_buf(path.to_owned()).unwrap();
        compilation.package.root = path.clone();
        let module = compilation.hir.modules.iter().next().unwrap().0;
        compilation.hir.modules[module].source_path = Some(path);
        compilation
    }

    #[test]
    fn closing_documents_releases_snapshots_bodies_and_source_parses() {
        let cache = CompilationCache::default();
        let root = std::env::current_dir().unwrap();
        let path = root.join("closed.fos");
        let other = root.join("open.fos");
        let uri = path_to_uri(&path).unwrap();
        let other_uri = path_to_uri(&other).unwrap();
        let source = "func main() -> Int { 1 }";
        cache.parse_document(&path, source).unwrap();
        cache.parse_document(&other, source).unwrap();
        let compilation = cache.insert(uri.clone(), snapshot(&path));
        let released = Rc::downgrade(&compilation);
        let body = crate::typecheck::incremental::SharedBodyCache::default();
        let released_body = Rc::downgrade(&body);
        cache
            .bodies
            .borrow_mut()
            .insert(compilation.package.root.clone(), body);
        drop(compilation);
        let retained = cache.insert(other_uri.clone(), snapshot(&other));

        cache.close(&uri, [&other_uri].into_iter());
        assert!(released.upgrade().is_none());
        assert!(released_body.upgrade().is_none());
        assert!(Rc::ptr_eq(&retained, &cache.get(&other_uri).unwrap()));
        cache.parse_document(&path, source).unwrap();
        cache.parse_document(&other, source).unwrap();
        assert_eq!(cache.module_parse_count(&path), 2);
        assert_eq!(cache.module_parse_count(&other), 1);
    }

    #[test]
    fn editing_keeps_fallback_but_closing_dependency_releases_it() {
        let cache = CompilationCache::default();
        let root = std::env::current_dir().unwrap();
        let dependency = root.join("dependency.fos");
        let uri = path_to_uri(&root.join("main.fos")).unwrap();
        let dependency_uri = path_to_uri(&dependency).unwrap();
        let compilation = cache.insert(uri.clone(), snapshot(&dependency));
        let released = Rc::downgrade(&compilation);
        drop(compilation);
        cache.prune([&uri, &dependency_uri].into_iter());
        cache.invalidate(&dependency_uri);
        assert!(cache.get(&uri).is_none());
        assert!(cache.last_good(&uri).is_some());
        cache.close(&dependency_uri, [&uri].into_iter());
        assert!(cache.last_good(&uri).is_none());
        assert!(released.upgrade().is_none());
    }

    #[test]
    fn watched_file_changes_discard_semantic_fallback_and_incremental_state() {
        let cache = CompilationCache::default();
        let path = std::env::current_dir().unwrap().join("removed.fos");
        let uri = path_to_uri(&path).unwrap();
        let compilation = cache.insert(uri.clone(), snapshot(&path));
        let released = Rc::downgrade(&compilation);
        cache
            .bodies
            .borrow_mut()
            .insert(compilation.package.root.clone(), Default::default());
        drop(compilation);
        cache.insert_error(uri.clone(), FosterError::runtime("missing dependency"));
        cache.clear();
        assert!(released.upgrade().is_none());
        assert!(cache.last_good(&uri).is_none());
        assert!(cache.error(&uri).is_none());
        assert!(cache.bodies.borrow().is_empty());
    }

    #[test]
    fn recent_navigation_evicts_least_recent_snapshot_but_keeps_open_documents() {
        let cache = CompilationCache::default();
        let root = std::env::current_dir().unwrap();
        let open_path = root.join("open.fos");
        let open_uri = path_to_uri(&open_path).unwrap();
        let retained = cache.insert(open_uri.clone(), snapshot(&open_path));
        let mut recent = Vec::new();
        for index in 0..RECENT_DOCUMENT_LIMIT {
            let path = root.join(format!("recent-{index}.fos"));
            let uri = path_to_uri(&path).unwrap();
            cache.touch(&uri);
            let compilation = cache.insert(uri.clone(), snapshot(&path));
            recent.push((uri, Rc::downgrade(&compilation)));
            cache.prune([&open_uri].into_iter());
        }
        cache.touch(&recent[0].0);
        let path = root.join("next.fos");
        let uri = path_to_uri(&path).unwrap();
        cache.touch(&uri);
        cache.insert(uri, snapshot(&path));
        cache.prune([&open_uri].into_iter());

        assert!(recent[0].1.upgrade().is_some());
        assert!(recent[1].1.upgrade().is_none());
        assert!(cache.last_good(&recent[1].0).is_none());
        assert!(Rc::ptr_eq(&retained, &cache.get(&open_uri).unwrap()));
        assert_eq!(cache.entries.borrow().len(), RECENT_DOCUMENT_LIMIT + 1);
    }

    #[test]
    fn invalidates_every_snapshot_containing_a_shared_module() {
        let cache = CompilationCache::default();
        let root = std::env::current_dir().unwrap();
        let shared = root.join("shared.fos");
        let shared_uri = path_to_uri(&shared).unwrap();
        let first_uri = path_to_uri(&root.join("first.fos")).unwrap();
        let second_uri = path_to_uri(&root.join("second.fos")).unwrap();
        let unrelated_uri = path_to_uri(&root.join("unrelated.fos")).unwrap();
        for uri in [&first_uri, &second_uri] {
            let mut compilation = crate::compile("func main() -> Int { 1 }").unwrap();
            // Model two independently checked roots containing the same dependency.
            let module = compilation.hir.modules.iter().next().unwrap().0;
            compilation.hir.modules[module].source_path =
                Some(Utf8PathBuf::from_path_buf(shared.clone()).unwrap());
            cache.insert(uri.clone(), compilation);
        }
        let unrelated = cache.insert(
            unrelated_uri.clone(),
            crate::compile("func main() -> Int { 2 }").unwrap(),
        );
        assert!(cache.get(&first_uri).is_some());
        assert!(cache.get(&second_uri).is_some());
        cache.invalidate(&shared_uri);
        assert!(cache.get(&first_uri).is_none());
        assert!(cache.get(&second_uri).is_none());
        assert!(cache.get(&shared_uri).is_none());
        assert!(Rc::ptr_eq(&unrelated, &cache.get(&unrelated_uri).unwrap()));
    }
}
