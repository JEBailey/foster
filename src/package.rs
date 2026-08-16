use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;

use camino::{Utf8Path, Utf8PathBuf};
use walkdir::{DirEntry, WalkDir};

use crate::ast::Program;
use crate::error::FosterError;

#[derive(Debug, Clone)]
pub struct Module {
    pub name: String,
    pub source_path: Option<Utf8PathBuf>,
    pub program: Option<Program>,
    pub source: Option<String>,
}

impl Module {
    pub fn is_implicit(&self) -> bool {
        self.program.is_none()
    }
}

#[derive(Debug, Clone)]
pub struct Package {
    pub root: Utf8PathBuf,
    pub modules: BTreeMap<String, Module>,
}

impl Package {
    pub fn from_program(name: impl Into<String>, program: Program) -> Self {
        let name = name.into();
        let module = Module {
            name: name.clone(),
            source_path: None,
            program: Some(program),
            source: None,
        };
        Self {
            root: Utf8PathBuf::new(),
            modules: BTreeMap::from([(name, module)]),
        }
    }

    pub fn from_program_with_core(
        name: impl Into<String>,
        program: Program,
    ) -> Result<Self, FosterError> {
        let mut package = Self::from_program(name, program);
        package.install_core_modules_if_imported()?;
        package.validate()?;
        Ok(package)
    }

    pub fn load(root: impl AsRef<Path>) -> Result<Self, FosterError> {
        Self::load_with_overlays(root, &HashMap::new())
    }

    pub fn load_with_overlays(
        root: impl AsRef<Path>,
        overlays: &HashMap<Utf8PathBuf, String>,
    ) -> Result<Self, FosterError> {
        let root = root.as_ref();
        if !root.is_dir() {
            return Err(FosterError::runtime(format!(
                "package source root `{}` is not a directory",
                root.display()
            )));
        }
        let root = Utf8PathBuf::from_path_buf(root.to_path_buf()).map_err(|path| {
            FosterError::runtime(format!(
                "package source root is not valid UTF-8: `{}`",
                path.display()
            ))
        })?;
        let mut package = Self {
            root,
            modules: BTreeMap::new(),
        };
        package.discover_modules(overlays)?;
        package.install_core_modules_if_imported()?;
        package.validate()?;
        Ok(package)
    }

    fn install_core_modules_if_imported(&mut self) -> Result<(), FosterError> {
        let imports_core = self.modules.values().any(|module| {
            module.program.as_ref().is_some_and(|program| {
                program
                    .imports
                    .iter()
                    .any(|import| import.path.first().is_some_and(|name| name == "core"))
            })
        });
        if !imports_core {
            return Ok(());
        }
        let existing = CORE_MODULES
            .iter()
            .filter(|(name, _)| self.modules.contains_key(*name))
            .count();
        if existing == CORE_MODULES.len() {
            return Ok(());
        }
        if existing != 0 {
            return Err(FosterError::runtime(
                "the embedded `core` namespace cannot be partially redefined",
            ));
        }
        self.modules.entry("core".into()).or_insert(Module {
            name: "core".into(),
            source_path: None,
            program: None,
            source: None,
        });
        self.modules.entry("core.net".into()).or_insert(Module {
            name: "core.net".into(),
            source_path: None,
            program: None,
            source: None,
        });
        for (name, source) in CORE_MODULES {
            let program = crate::parse(source).map_err(|error| {
                FosterError::runtime(format!("embedded module `{name}` is invalid: {error}"))
            })?;
            self.modules.insert(
                (*name).into(),
                Module {
                    name: (*name).into(),
                    source_path: core_source_path(name),
                    program: Some(program),
                    source: Some((*source).to_owned()),
                },
            );
        }
        Ok(())
    }

    pub fn module(&self, name: &str) -> Option<&Module> {
        self.modules.get(name)
    }

    pub fn explicit_module_count(&self) -> usize {
        self.modules
            .values()
            .filter(|module| !module.is_implicit())
            .count()
    }

    pub fn implicit_module_count(&self) -> usize {
        self.modules
            .values()
            .filter(|module| module.is_implicit())
            .count()
    }

    fn discover_modules(
        &mut self,
        overlays: &HashMap<Utf8PathBuf, String>,
    ) -> Result<(), FosterError> {
        let entries = WalkDir::new(&self.root)
            .follow_links(false)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|entry| !is_ignored_directory(entry));

        for entry in entries {
            let entry = entry.map_err(|error| {
                FosterError::runtime(format!("cannot walk `{}`: {error}", self.root))
            })?;
            let path = Utf8PathBuf::from_path_buf(entry.into_path()).map_err(|path| {
                FosterError::runtime(format!(
                    "source path is not valid UTF-8: `{}`",
                    path.display()
                ))
            })?;
            if path == self.root {
                continue;
            }
            let relative = path
                .strip_prefix(&self.root)
                .expect("walked source paths are beneath the package root");
            if path.is_dir() {
                self.ensure_implicit(&module_components(relative, false)?);
            } else if path.extension() == Some("foster") {
                self.add_explicit(module_components(relative, true)?, path, overlays)?;
            }
        }
        Ok(())
    }

    fn ensure_implicit(&mut self, path: &[String]) {
        let name = path.join(".");
        self.modules.entry(name.clone()).or_insert(Module {
            name,
            source_path: None,
            program: None,
            source: None,
        });
    }

    fn add_explicit(
        &mut self,
        path: Vec<String>,
        source_path: Utf8PathBuf,
        overlays: &HashMap<Utf8PathBuf, String>,
    ) -> Result<(), FosterError> {
        let name = path.join(".");
        let source = overlays.get(&source_path).cloned().map_or_else(
            || {
                fs::read_to_string(&source_path).map_err(|error| {
                    FosterError::runtime(format!("cannot read `{source_path}`: {error}"))
                })
            },
            Ok,
        )?;
        let program = crate::parse(&source)
            .map_err(|error| FosterError::runtime(format!("{source_path}: {error}")))?;
        let module = self.modules.entry(name.clone()).or_insert(Module {
            name,
            source_path: None,
            program: None,
            source: None,
        });
        if let Some(existing) = &module.source_path {
            return Err(FosterError::runtime(format!(
                "module `{}` has two source files: `{existing}` and `{source_path}`",
                module.name
            )));
        }
        module.source_path = Some(source_path);
        module.program = Some(program);
        module.source = Some(source);
        Ok(())
    }

    fn validate(&self) -> Result<(), FosterError> {
        self.validate_portable_names()?;
        for module in self.modules.values() {
            let Some(program) = &module.program else {
                continue;
            };
            let mut definitions = HashSet::new();
            for record in &program.records {
                if !definitions.insert(record.name.as_str()) {
                    return Err(FosterError::runtime(format!(
                        "module `{}` defines `{}` more than once",
                        module.name, record.name
                    )));
                }
            }
            for variant in &program.variants {
                if !definitions.insert(variant.name.as_str()) {
                    return Err(FosterError::runtime(format!(
                        "module `{}` defines `{}` more than once",
                        module.name, variant.name
                    )));
                }
            }
            for function in &program.functions {
                if let Some((owner, _)) = function.name.split_once('.') {
                    if !program.records.iter().any(|record| record.name == owner) {
                        return Err(FosterError::runtime(format!(
                            "module `{}` defines associated function `{}` for unknown record type `{owner}`",
                            module.name, function.name
                        )));
                    }
                    if function
                        .parameters
                        .first()
                        .is_some_and(|parameter| parameter.name == "self")
                    {
                        return Err(FosterError::runtime(format!(
                            "associated function `{}` cannot declare a `self` parameter; declare an instance method as `func {}`",
                            function.name,
                            function.name.split_once('.').unwrap().1
                        )));
                    }
                }
                if !definitions.insert(function.name.as_str()) {
                    return Err(FosterError::runtime(format!(
                        "module `{}` defines `{}` more than once",
                        module.name, function.name
                    )));
                }
            }
            let mut aliases = HashSet::new();
            for import in &program.imports {
                let target = import.path.join(".");
                if !self.modules.contains_key(&target) {
                    return Err(FosterError::runtime(format!(
                        "module `{}` imports unknown module `{target}`",
                        module.name
                    )));
                }
                let local_name = import
                    .alias
                    .as_ref()
                    .unwrap_or_else(|| import.path.last().expect("an import path is never empty"));
                if !aliases.insert(local_name.as_str()) {
                    return Err(FosterError::runtime(format!(
                        "module `{}` binds import name `{local_name}` more than once",
                        module.name
                    )));
                }
                if definitions.contains(local_name.as_str()) {
                    return Err(FosterError::runtime(format!(
                        "module `{}` uses `{local_name}` for both an import and a declaration",
                        module.name
                    )));
                }
            }
        }
        Ok(())
    }

    fn validate_portable_names(&self) -> Result<(), FosterError> {
        let mut folded: HashMap<String, &str> = HashMap::new();
        for name in self.modules.keys() {
            let key = name.to_lowercase();
            if let Some(previous) = folded.insert(key, name)
                && previous != name
            {
                return Err(FosterError::runtime(format!(
                    "module names `{previous}` and `{name}` differ only by case"
                )));
            }
        }
        Ok(())
    }
}

fn core_source_path(module: &str) -> Option<Utf8PathBuf> {
    let relative = format!("library/{}.foster", module.replace('.', "/"));
    let path = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative);
    path.is_file().then_some(path)
}

const CORE_MODULES: &[(&str, &str)] = &[
    ("core.option", include_str!("../library/core/option.foster")),
    ("core.result", include_str!("../library/core/result.foster")),
    (
        "core.ordering",
        include_str!("../library/core/ordering.foster"),
    ),
    ("core.list", include_str!("../library/core/list.foster")),
    (
        "core.sequence",
        include_str!("../library/core/sequence.foster"),
    ),
    (
        "core.character",
        include_str!("../library/core/character.foster"),
    ),
    ("core.bool", include_str!("../library/core/bool.foster")),
    ("core.int", include_str!("../library/core/int.foster")),
    ("core.float", include_str!("../library/core/float.foster")),
    ("core.string", include_str!("../library/core/string.foster")),
    ("core.map", include_str!("../library/core/map.foster")),
    ("core.io", include_str!("../library/core/io.foster")),
    (
        "core.net.tcp",
        include_str!("../library/core/net/tcp.foster"),
    ),
];

fn module_components(path: &Utf8Path, strip_extension: bool) -> Result<Vec<String>, FosterError> {
    let mut components = path
        .components()
        .map(|component| component.as_str().to_owned())
        .collect::<Vec<_>>();
    if strip_extension {
        let last = components.last_mut().expect("a source file has a filename");
        *last = last
            .strip_suffix(".foster")
            .expect("source extension was checked")
            .to_owned();
    }
    for component in &components {
        validate_component(component, path)?;
    }
    Ok(components)
}

fn validate_component(name: &str, path: &Utf8Path) -> Result<(), FosterError> {
    let mut chars = name.chars();
    let valid = chars.next().is_some_and(|c| c == '_' || c.is_alphabetic())
        && chars.all(|c| c == '_' || c.is_alphanumeric());
    if valid {
        Ok(())
    } else {
        Err(FosterError::runtime(format!(
            "`{name}` is not a valid module name (from `{path}`)"
        )))
    }
}

fn is_ignored_directory(entry: &DirEntry) -> bool {
    if entry.depth() == 0 || !entry.file_type().is_dir() {
        return false;
    }
    match entry.file_name().to_str() {
        Some("target" | ".git" | ".foster") => true,
        Some("documentation") => entry.depth() == 1,
        _ => false,
    }
}
