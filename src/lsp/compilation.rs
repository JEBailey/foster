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
}

impl CompilationCache {
    pub(super) fn clear(&self) {
        self.entries.borrow_mut().clear();
    }

    fn get(&self, uri: &Uri) -> Option<Rc<Compilation>> {
        self.entries.borrow().get(uri).cloned()
    }

    fn insert(&self, uri: Uri, compilation: Compilation) -> Rc<Compilation> {
        let compilation = Rc::new(compilation);
        let mut entries = self.entries.borrow_mut();
        entries.insert(uri, Rc::clone(&compilation));
        for (_, module) in compilation.hir.modules.iter() {
            let Some(path) = module.source_path.as_deref() else {
                continue;
            };
            let Some(uri) = path_to_uri(path.as_std_path()) else {
                continue;
            };
            entries.insert(uri, Rc::clone(&compilation));
        }
        compilation
    }
}

impl Workspace {
    pub(super) fn compile_for(&self, uri: &Uri) -> Result<Rc<Compilation>, FosterError> {
        if let Some(compilation) = self.compilations.get(uri) {
            return Ok(compilation);
        }
        let compilation = self.compile_uncached(uri)?;
        Ok(self.compilations.insert(uri.clone(), compilation))
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
            let package =
                crate::package::Package::load_with_overlays(&project.source_root, &overlays)?;
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
            if let Ok(package) = crate::package::Package::load_with_overlays(root, &overlays)
                && package.modules.values().any(|module| {
                    module
                        .source_path
                        .as_ref()
                        .is_some_and(|source| source.as_std_path() == path)
                })
            {
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
        Compilation::new(package)
    }
}
