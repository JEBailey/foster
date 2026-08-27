use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use camino::Utf8PathBuf;
use lsp_types::Uri;

use super::workspace::{Workspace, path_to_uri, uri_to_path};
use crate::error::FosterError;
use crate::hir::Compilation;

#[derive(Default)]
pub(super) struct CompilationCache {
    entries: RefCell<HashMap<Uri, Rc<Compilation>>>,
    errors: RefCell<HashMap<Uri, FosterError>>,
    last_good: RefCell<HashMap<Uri, Rc<Compilation>>>,
    modules: RefCell<crate::package::ModuleCache>,
}

impl CompilationCache {
    pub(super) fn clear(&self) {
        // Watched-file changes can invalidate package membership, so discard semantic snapshots.
        // Parsed modules remain content-addressed in `modules` and are reused on the next build.
        self.entries.borrow_mut().clear();
        self.errors.borrow_mut().clear();
    }

    pub(super) fn invalidate(&self, uri: &Uri) {
        let invalid = self.entries.borrow().get(uri).cloned();
        let mut entries = self.entries.borrow_mut();
        if let Some(invalid) = invalid {
            entries.retain(|_, compilation| !Rc::ptr_eq(compilation, &invalid));
        } else {
            entries.remove(uri);
        }
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
}

impl Workspace {
    pub(super) fn compile_for(&self, uri: &Uri) -> Result<Rc<Compilation>, FosterError> {
        if let Some(compilation) = self.compilations.get(uri) {
            return Ok(compilation);
        }
        if let Some(error) = self.compilations.error(uri) {
            return Err(error);
        }
        match self.compile_uncached(uri) {
            Ok(compilation) => Ok(self.compilations.insert(uri.clone(), compilation)),
            Err(error) => {
                self.compilations.insert_error(uri.clone(), error.clone());
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
            let package = crate::package::Package::load_project_with_overlays_cached(
                &project,
                &overlays,
                &mut self.compilations.modules.borrow_mut(),
            )?;
            if package.modules.values().any(|module| {
                module
                    .source_path
                    .as_ref()
                    .is_some_and(|source| source.as_std_path() == path)
            }) {
                return Compilation::new(package);
            }
        }

        let standalone = self.compile_standalone(&path, &overlays);
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
            if let Ok(package) = crate::package::Package::load_with_overlays_cached(
                root,
                &overlays,
                &mut self.compilations.modules.borrow_mut(),
            ) && package.modules.values().any(|module| {
                module
                    .source_path
                    .as_ref()
                    .is_some_and(|source| source.as_std_path() == path)
            }) {
                return Compilation::new(package);
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
        let mut package = crate::package::Package::from_program_with_core_cached(
            &module_name,
            program,
            &mut modules,
        )?;
        let module = package
            .modules
            .get_mut(&module_name)
            .expect("standalone package contains its source module");
        module.source_path = Some(source_path);
        module.source = Some(source);
        Compilation::new(package)
    }
}
