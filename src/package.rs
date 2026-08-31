use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;

use camino::{Utf8Path, Utf8PathBuf};
use walkdir::{DirEntry, WalkDir};

use crate::ast::Program;
use crate::error::FosterError;

#[derive(Debug, Clone)]
struct CachedModule {
    source: String,
    parsed: Result<crate::parser::RecoveringParse, FosterError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ModuleCacheKey {
    Source(Utf8PathBuf),
    Embedded(String),
}

/// Parsed Foster modules retained across language-server package rebuilds.
///
/// Entries are replaced only when their source text changes. Cached parse errors are retained too,
/// so an unchanged invalid module does not repeatedly run through the lexer and parser.
#[derive(Debug, Default)]
pub(crate) struct ModuleCache {
    entries: HashMap<ModuleCacheKey, CachedModule>,
    #[cfg(test)]
    parse_counts: HashMap<ModuleCacheKey, usize>,
}

impl ModuleCache {
    pub(crate) fn parse_source(
        &mut self,
        path: &Utf8Path,
        source: &str,
    ) -> Result<Program, FosterError> {
        self.parse(ModuleCacheKey::Source(path.to_owned()), source)
    }

    fn parse_embedded(&mut self, name: &str, source: &str) -> Result<Program, FosterError> {
        self.parse(ModuleCacheKey::Embedded(name.to_owned()), source)
    }

    fn parse(&mut self, key: ModuleCacheKey, source: &str) -> Result<Program, FosterError> {
        if let Some(cached) = self.entries.get(&key)
            && cached.source == source
        {
            return cached_program(&key, cached.parsed.clone());
        }

        let parsed = crate::parse_recovering(source);
        self.entries.insert(
            key.clone(),
            CachedModule {
                source: source.to_owned(),
                parsed: parsed.clone(),
            },
        );
        self.record_parse(&key);
        cached_program(&key, parsed)
    }

    pub(crate) fn source_diagnostics(&self, path: &Utf8Path) -> Vec<FosterError> {
        self.entries
            .get(&ModuleCacheKey::Source(path.to_owned()))
            .and_then(|cached| cached.parsed.as_ref().ok())
            .map(|parsed| parsed.diagnostics.clone())
            .unwrap_or_default()
    }

    #[cfg(test)]
    fn record_parse(&mut self, key: &ModuleCacheKey) {
        *self.parse_counts.entry(key.clone()).or_default() += 1;
    }

    #[cfg(not(test))]
    fn record_parse(&mut self, _key: &ModuleCacheKey) {}

    #[cfg(test)]
    pub(crate) fn source_parse_count(&self, path: &Utf8Path) -> usize {
        self.parse_counts
            .get(&ModuleCacheKey::Source(path.to_owned()))
            .copied()
            .unwrap_or_default()
    }
}

fn cached_program(
    key: &ModuleCacheKey,
    parsed: Result<crate::parser::RecoveringParse, FosterError>,
) -> Result<Program, FosterError> {
    let parsed = parsed?;
    if matches!(key, ModuleCacheKey::Embedded(_))
        && let Some(error) = parsed.diagnostics.first()
    {
        return Err(error.clone());
    }
    Ok(parsed.program)
}

fn parse_source_module(
    cache: &mut Option<&mut ModuleCache>,
    path: &Utf8Path,
    source: &str,
) -> Result<Program, FosterError> {
    cache.as_deref_mut().map_or_else(
        || crate::parse(source),
        |cache| cache.parse_source(path, source),
    )
}

fn parse_embedded_module(
    cache: &mut Option<&mut ModuleCache>,
    name: &str,
    source: &str,
) -> Result<Program, FosterError> {
    cache.as_deref_mut().map_or_else(
        || crate::parse(source),
        |cache| cache.parse_embedded(name, source),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleOrigin {
    Input,
    Dependency,
    Embedded,
}

#[derive(Debug, Clone)]
pub struct Module {
    pub name: String,
    pub source_path: Option<Utf8PathBuf>,
    pub program: Option<Program>,
    pub source: Option<String>,
    pub origin: ModuleOrigin,
}

impl Module {
    pub fn is_implicit(&self) -> bool {
        self.program.is_none()
    }

    pub fn is_input(&self) -> bool {
        self.origin == ModuleOrigin::Input
    }
}

#[derive(Debug, Clone)]
pub struct Package {
    pub root: Utf8PathBuf,
    pub modules: BTreeMap<String, Module>,
}

#[derive(Clone, Copy)]
struct BootstrapModule {
    name: &'static str,
    source: &'static str,
    mode: BootstrapMode,
}

#[derive(Clone, Copy)]
enum BootstrapMode {
    Full,
    TypesOnly(&'static [&'static str]),
    TypesAndFunctions {
        types: &'static [&'static str],
        functions: &'static [&'static str],
    },
}

impl BootstrapModule {
    const fn full(name: &'static str, source: &'static str) -> Self {
        Self {
            name,
            source,
            mode: BootstrapMode::Full,
        }
    }

    const fn types_only(
        name: &'static str,
        source: &'static str,
        types: &'static [&'static str],
    ) -> Self {
        Self {
            name,
            source,
            mode: BootstrapMode::TypesOnly(types),
        }
    }

    const fn types_and_functions(
        name: &'static str,
        source: &'static str,
        types: &'static [&'static str],
        functions: &'static [&'static str],
    ) -> Self {
        Self {
            name,
            source,
            mode: BootstrapMode::TypesAndFunctions { types, functions },
        }
    }
}

impl Package {
    pub(crate) fn locate_compiler_error(&self, mut error: FosterError) -> FosterError {
        if error.has_source_location() {
            return error;
        }

        let module = error
            .source_module
            .as_deref()
            .and_then(|name| self.modules.get(name))
            .filter(|module| module.program.is_some())
            .or_else(|| {
                self.modules
                    .values()
                    .filter(|module| module.program.is_some())
                    .find(|module| {
                        error.message.contains(&format!("`{}.", module.name))
                            || error.message.contains(&format!("module `{}`", module.name))
                            || error.message.contains(&format!("module '{}'", module.name))
                    })
            })
            .or_else(|| {
                self.modules
                    .get("main")
                    .filter(|module| module.program.is_some())
            })
            .or_else(|| {
                self.modules
                    .values()
                    .find(|module| module.program.is_some())
            });
        let Some(module) = module else {
            return error;
        };
        let Some(program) = module.program.as_ref() else {
            return error;
        };

        let qualified_function = |name: &str| {
            error.message.contains(&format!("`{}.{name}`", module.name))
                || error.message.contains(&format!("function `{name}`"))
                || error.message.contains(&format!("of `{name}`"))
        };
        let named_declaration = |name: &str| {
            error.message.contains(&format!("`{name}`"))
                || error.message.contains(&format!("`{}.{name}`", module.name))
        };
        let range = program
            .functions
            .iter()
            .find(|function| qualified_function(&function.name))
            .map(|function| function.span.clone())
            .or_else(|| {
                program
                    .records
                    .iter()
                    .find(|record| named_declaration(&record.name))
                    .map(|record| record.span.clone())
            })
            .or_else(|| {
                program
                    .variants
                    .iter()
                    .find(|variant| named_declaration(&variant.name))
                    .map(|variant| variant.span.clone())
            })
            .or_else(|| {
                program
                    .constants
                    .iter()
                    .find(|constant| named_declaration(&constant.name))
                    .map(|constant| constant.span.clone())
            })
            .or_else(|| {
                program
                    .imports
                    .iter()
                    .find(|import| error.message.contains(&import.path.join(".")))
                    .map(|import| import.span.clone())
            })
            .or_else(|| program.functions.first().map(|item| item.span.clone()))
            .or_else(|| program.records.first().map(|item| item.span.clone()))
            .or_else(|| program.variants.first().map(|item| item.span.clone()))
            .or_else(|| program.constants.first().map(|item| item.span.clone()))
            .or_else(|| program.imports.first().map(|item| item.span.clone()))
            .unwrap_or_else(|| {
                let source = module.source.as_deref().unwrap_or_default();
                let start = source
                    .char_indices()
                    .find_map(|(offset, character)| (!character.is_whitespace()).then_some(offset))
                    .unwrap_or(0);
                let end = source[start..]
                    .chars()
                    .next()
                    .map_or(start, |character| start + character.len_utf8());
                start..end
            });

        error = error.with_fallback_location(
            module.name.clone(),
            range,
            "the compiler detected this error in this declaration",
        );
        error
    }

    pub fn from_program(name: impl Into<String>, program: Program) -> Self {
        let name = name.into();
        let module = Module {
            name: name.clone(),
            source_path: None,
            program: Some(program),
            source: None,
            origin: ModuleOrigin::Input,
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
        package.finish_loading(&mut None)?;
        Ok(package)
    }

    pub(crate) fn from_program_with_core_cached(
        name: impl Into<String>,
        program: Program,
        cache: &mut ModuleCache,
    ) -> Result<Self, FosterError> {
        let mut package = Self::from_program(name, program);
        package.finish_loading(&mut Some(cache))?;
        Ok(package)
    }

    pub fn load(root: impl AsRef<Path>) -> Result<Self, FosterError> {
        Self::load_with_overlays(root, &HashMap::new())
    }

    pub fn load_with_overlays(
        root: impl AsRef<Path>,
        overlays: &HashMap<Utf8PathBuf, String>,
    ) -> Result<Self, FosterError> {
        Self::load_with_overlays_and_cache(root.as_ref(), overlays, &mut None)
    }

    pub(crate) fn load_with_overlays_cached(
        root: impl AsRef<Path>,
        overlays: &HashMap<Utf8PathBuf, String>,
        cache: &mut ModuleCache,
    ) -> Result<Self, FosterError> {
        Self::load_with_overlays_and_cache(root.as_ref(), overlays, &mut Some(cache))
    }

    fn load_with_overlays_and_cache(
        root: &Path,
        overlays: &HashMap<Utf8PathBuf, String>,
        cache: &mut Option<&mut ModuleCache>,
    ) -> Result<Self, FosterError> {
        let root = utf8_source_root(root)?;
        let mut package = Self {
            root: root.clone(),
            modules: BTreeMap::new(),
        };
        package.discover_modules_from(&root, ModuleOrigin::Input, None, overlays, cache)?;
        package.finish_loading(cache)?;
        Ok(package)
    }

    pub fn load_project(project: &crate::project::Project) -> Result<Self, FosterError> {
        Self::load_project_with_overlays(project, &HashMap::new())
    }

    pub fn load_project_with_overlays(
        project: &crate::project::Project,
        overlays: &HashMap<Utf8PathBuf, String>,
    ) -> Result<Self, FosterError> {
        Self::load_project_with_overlays_and_cache(project, overlays, &mut None)
    }

    pub(crate) fn load_project_with_overlays_cached(
        project: &crate::project::Project,
        overlays: &HashMap<Utf8PathBuf, String>,
        cache: &mut ModuleCache,
    ) -> Result<Self, FosterError> {
        Self::load_project_with_overlays_and_cache(project, overlays, &mut Some(cache))
    }

    fn load_project_with_overlays_and_cache(
        project: &crate::project::Project,
        overlays: &HashMap<Utf8PathBuf, String>,
        cache: &mut Option<&mut ModuleCache>,
    ) -> Result<Self, FosterError> {
        let root = utf8_source_root(&project.source_root)?;
        let mut package = Self {
            root: root.clone(),
            modules: BTreeMap::new(),
        };
        package.discover_modules_from(&root, ModuleOrigin::Input, None, overlays, cache)?;
        for dependency in project.resolve_dependencies()? {
            let dependency_root = utf8_source_root(&dependency.project.source_root)?;
            package.discover_modules_from(
                &dependency_root,
                ModuleOrigin::Dependency,
                Some(&dependency.name),
                overlays,
                cache,
            )?;
        }
        package.finish_loading(cache)?;
        Ok(package)
    }

    fn finish_loading(&mut self, cache: &mut Option<&mut ModuleCache>) -> Result<(), FosterError> {
        self.install_standard_modules_if_imported(cache)?;
        self.install_bytes_bootstrap(cache)?;
        self.install_byte_buffer_bootstrap(cache)?;
        self.install_list_bootstrap(cache)?;
        self.install_string_bootstrap(cache)?;
        self.install_symbol_bootstrap(cache)?;
        self.validate()
            .map_err(|error| self.locate_compiler_error(error))
    }

    fn install_string_bootstrap(
        &mut self,
        cache: &mut Option<&mut ModuleCache>,
    ) -> Result<(), FosterError> {
        self.install_bootstrap(
            BootstrapModule::types_only(
                "core.string",
                include_str!("../library/core/string.fos"),
                &["String"],
            ),
            cache,
        )
    }

    fn install_bytes_bootstrap(
        &mut self,
        cache: &mut Option<&mut ModuleCache>,
    ) -> Result<(), FosterError> {
        self.install_bootstrap(
            BootstrapModule::types_only(
                "core.bytes",
                include_str!("../library/core/bytes.fos"),
                &["RawBytes", "Bytes"],
            ),
            cache,
        )
    }

    fn install_byte_buffer_bootstrap(
        &mut self,
        cache: &mut Option<&mut ModuleCache>,
    ) -> Result<(), FosterError> {
        self.install_bootstrap(
            BootstrapModule::types_only(
                "core.bytes.buffer",
                include_str!("../library/core/bytes/buffer.fos"),
                &["RawByteBuffer", "ByteBuffer"],
            ),
            cache,
        )
    }

    fn install_list_bootstrap(
        &mut self,
        cache: &mut Option<&mut ModuleCache>,
    ) -> Result<(), FosterError> {
        self.install_bootstrap(
            BootstrapModule::types_and_functions(
                "core.list",
                include_str!("../library/core/list.fos"),
                &["RawList", "List"],
                &["List.push", "List.append"],
            ),
            cache,
        )
    }

    fn install_symbol_bootstrap(
        &mut self,
        cache: &mut Option<&mut ModuleCache>,
    ) -> Result<(), FosterError> {
        self.install_bootstrap(
            BootstrapModule::full("core.symbol", include_str!("../library/core/symbol.fos")),
            cache,
        )
    }

    fn install_bootstrap(
        &mut self,
        bootstrap: BootstrapModule,
        cache: &mut Option<&mut ModuleCache>,
    ) -> Result<(), FosterError> {
        if self.modules.contains_key(bootstrap.name) {
            return Ok(());
        }
        self.modules.entry("core".into()).or_insert(Module {
            name: "core".into(),
            source_path: None,
            program: None,
            source: None,
            origin: ModuleOrigin::Embedded,
        });
        let mut program =
            parse_embedded_module(cache, bootstrap.name, bootstrap.source).map_err(|error| {
                FosterError::runtime(format!(
                    "embedded module `{}` is invalid: {error}",
                    bootstrap.name
                ))
            })?;
        if let BootstrapMode::TypesOnly(types) | BootstrapMode::TypesAndFunctions { types, .. } =
            bootstrap.mode
        {
            program.imports.clear();
            program.constants.clear();
            program.variants.clear();
            match bootstrap.mode {
                BootstrapMode::TypesAndFunctions { functions, .. } => program
                    .functions
                    .retain(|function| functions.contains(&function.name.as_str())),
                _ => program.functions.clear(),
            }
            program.tests.clear();
            program
                .records
                .retain(|record| types.contains(&record.name.as_str()));
            if program.records.len() != types.len() {
                return Err(FosterError::runtime(format!(
                    "embedded `{}` must define bootstrap types {}",
                    bootstrap.name,
                    types.join(", ")
                )));
            }
        }
        self.modules.insert(
            bootstrap.name.into(),
            Module {
                name: bootstrap.name.into(),
                source_path: embedded_source_path(bootstrap.name),
                program: Some(program),
                source: Some(bootstrap.source.to_owned()),
                origin: ModuleOrigin::Embedded,
            },
        );
        Ok(())
    }

    fn install_standard_modules_if_imported(
        &mut self,
        cache: &mut Option<&mut ModuleCache>,
    ) -> Result<(), FosterError> {
        let imports_embedded = self.modules.values().any(|module| {
            module.program.as_ref().is_some_and(|program| {
                program.imports.iter().any(|import| {
                    import
                        .path
                        .first()
                        .is_some_and(|name| matches!(name.as_str(), "core" | "std"))
                })
            })
        });
        if !imports_embedded {
            return Ok(());
        }
        let existing = EMBEDDED_MODULES
            .iter()
            .filter(|(name, _)| self.modules.contains_key(*name))
            .count();
        if existing == EMBEDDED_MODULES.len() {
            return Ok(());
        }
        if existing != 0 {
            return Err(FosterError::runtime(
                "the embedded `core` and `std` namespaces cannot be partially redefined",
            ));
        }
        self.modules.entry("core".into()).or_insert(Module {
            name: "core".into(),
            source_path: None,
            program: None,
            source: None,
            origin: ModuleOrigin::Embedded,
        });
        for namespace in ["core.bytes", "std", "std.net"] {
            self.modules.entry(namespace.into()).or_insert(Module {
                name: namespace.into(),
                source_path: None,
                program: None,
                source: None,
                origin: ModuleOrigin::Embedded,
            });
        }
        for (name, source) in EMBEDDED_MODULES {
            let program = parse_embedded_module(cache, name, source).map_err(|error| {
                FosterError::runtime(format!("embedded module `{name}` is invalid: {error}"))
            })?;
            self.modules.insert(
                (*name).into(),
                Module {
                    name: (*name).into(),
                    source_path: embedded_source_path(name),
                    program: Some(program),
                    source: Some((*source).to_owned()),
                    origin: ModuleOrigin::Embedded,
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

    pub fn is_input_module(&self, name: &str) -> bool {
        self.module(name).is_some_and(Module::is_input)
    }

    pub fn input_module_count(&self) -> usize {
        self.modules
            .values()
            .filter(|module| module.is_input())
            .count()
    }

    pub fn input_explicit_module_count(&self) -> usize {
        self.modules
            .values()
            .filter(|module| module.is_input() && !module.is_implicit())
            .count()
    }

    pub fn input_implicit_module_count(&self) -> usize {
        self.modules
            .values()
            .filter(|module| module.is_input() && module.is_implicit())
            .count()
    }

    fn discover_modules_from(
        &mut self,
        root: &Utf8Path,
        origin: ModuleOrigin,
        prefix: Option<&str>,
        overlays: &HashMap<Utf8PathBuf, String>,
        cache: &mut Option<&mut ModuleCache>,
    ) -> Result<(), FosterError> {
        let entries = WalkDir::new(root)
            .follow_links(false)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|entry| !is_ignored_directory(entry))
            .map(|entry| {
                let entry = entry.map_err(|error| {
                    FosterError::runtime(format!("cannot walk `{root}`: {error}"))
                })?;
                let is_directory = entry.file_type().is_dir();
                let path = Utf8PathBuf::from_path_buf(entry.into_path()).map_err(|path| {
                    FosterError::runtime(format!(
                        "source path is not valid UTF-8: `{}`",
                        path.display()
                    ))
                })?;
                Ok((path, is_directory))
            })
            .collect::<Result<Vec<_>, FosterError>>()?;

        let mut rewrites = HashMap::new();
        if let Some(prefix) = prefix {
            self.ensure_implicit(&[prefix.to_owned()], origin);
            rewrites.insert("main".to_owned(), vec![prefix.to_owned()]);
            for (path, is_directory) in &entries {
                if path == root {
                    continue;
                }
                let relative = path
                    .strip_prefix(root)
                    .expect("walked source paths are beneath the package root");
                if *is_directory || path.extension() == Some("fos") {
                    let local = module_components(relative, !is_directory)?;
                    rewrites.insert(local.join("."), mounted_components(prefix, &local));
                }
            }
        }

        for (path, is_directory) in entries {
            if path == root {
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .expect("walked source paths are beneath the package root");
            if is_directory {
                let local = module_components(relative, false)?;
                let mounted =
                    prefix.map_or_else(|| local.clone(), |name| mounted_components(name, &local));
                self.ensure_implicit(&mounted, origin);
            } else if path.extension() == Some("fos") {
                let local = module_components(relative, true)?;
                let mounted =
                    prefix.map_or_else(|| local.clone(), |name| mounted_components(name, &local));
                self.add_explicit(
                    mounted,
                    path,
                    origin,
                    prefix.map(|_| &rewrites),
                    overlays,
                    cache,
                )?;
            }
        }
        Ok(())
    }

    fn ensure_implicit(&mut self, path: &[String], origin: ModuleOrigin) {
        let name = path.join(".");
        self.modules.entry(name.clone()).or_insert(Module {
            name,
            source_path: None,
            program: None,
            source: None,
            origin,
        });
    }

    fn add_explicit(
        &mut self,
        path: Vec<String>,
        source_path: Utf8PathBuf,
        origin: ModuleOrigin,
        import_rewrites: Option<&HashMap<String, Vec<String>>>,
        overlays: &HashMap<Utf8PathBuf, String>,
        cache: &mut Option<&mut ModuleCache>,
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
        let mut program =
            parse_source_module(cache, &source_path, &source).map_err(|mut error| {
                error.message = format!("{source_path}: {}", error.message);
                if error.source_module.is_none() {
                    error.source_module = Some(name.clone());
                }
                error
            })?;
        if let Some(import_rewrites) = import_rewrites {
            for import in &mut program.imports {
                if let Some(rewritten) = import_rewrites.get(&import.path.join(".")) {
                    import.path.clone_from(rewritten);
                }
            }
        }
        let module = self.modules.entry(name.clone()).or_insert(Module {
            name,
            source_path: None,
            program: None,
            source: None,
            origin,
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
        module.origin = origin;
        Ok(())
    }

    fn validate(&self) -> Result<(), FosterError> {
        self.validate_portable_names()?;
        let mut intrinsic_keys = HashSet::new();
        for module in self.modules.values() {
            let Some(program) = &module.program else {
                continue;
            };
            let mut definitions = HashSet::new();
            let mut function_names = HashSet::new();
            for record in &program.records {
                if record.intrinsic
                    && !matches!(
                        (module.name.as_str(), record.name.as_str()),
                        ("core.bytes", "RawBytes")
                            | ("core.bytes.buffer", "RawByteBuffer")
                            | ("core.list", "RawList")
                    )
                {
                    return Err(FosterError::runtime(format!(
                        "intrinsic type `{}.{}` has no registered runtime representation",
                        module.name, record.name
                    )));
                }
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
                if let Some(key) = &function.intrinsic {
                    let registered = match module.name.as_str() {
                        "core.byte" => matches!(key.as_str(), "byte.valid" | "byte.unchecked"),
                        "core.bytes" => key.starts_with("bytes."),
                        "core.bytes.buffer" => key.starts_with("byte_buffer."),
                        "core.list" => key.starts_with("list."),
                        "core.float" => key.starts_with("float."),
                        "std.fs" | "std.path" | "std.env" => key.starts_with("io."),
                        "std.time" => key.starts_with("time."),
                        "std.net.tcp" => key.starts_with("tcp."),
                        _ => false,
                    } && intrinsic_key_registered(key);
                    if !registered {
                        return Err(FosterError::runtime(format!(
                            "intrinsic key `{key}` has no registered runtime implementation"
                        )));
                    }
                    if !intrinsic_keys.insert(key.as_str()) {
                        return Err(FosterError::runtime(format!(
                            "intrinsic key `{key}` is declared more than once"
                        )));
                    }
                }
                if let Some(owner) = function.owner.as_deref() {
                    if !function.receiver
                        && !program.records.iter().any(|record| record.name == owner)
                        && !matches!(
                            owner,
                            "Byte" | "Bytes" | "ByteBuffer" | "CodePoint" | "String"
                        )
                    {
                        return Err(FosterError::runtime(format!(
                            "module `{}` defines associated function `{}` for unknown record type `{owner}`",
                            module.name, function.name
                        )));
                    }
                } else if function.receiver {
                    return Err(FosterError::runtime(format!(
                        "instance method `{}` must qualify its name with its receiver type",
                        function.name
                    )));
                }
                if definitions.contains(function.name.as_str())
                    && !function_names.contains(function.name.as_str())
                {
                    return Err(FosterError::runtime(format!(
                        "module `{}` defines `{}` more than once",
                        module.name, function.name
                    )));
                }
                definitions.insert(function.name.as_str());
                function_names.insert(function.name.as_str());
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

pub(crate) fn embedded_source_path(module: &str) -> Option<Utf8PathBuf> {
    let relative = format!("library/{}.fos", module.replace('.', "/"));
    let source_tree = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(&relative);
    if source_tree.is_file() {
        return Some(source_tree);
    }

    let executable = std::env::current_exe().ok()?;
    let extension_root = executable.parent()?.parent()?;
    let bundled = extension_root.join(relative);
    let bundled = Utf8PathBuf::from_path_buf(bundled).ok()?;
    bundled.is_file().then_some(bundled)
}

const EMBEDDED_MODULES: &[(&str, &str)] = &[
    (
        "core.functions",
        include_str!("../library/core/functions.fos"),
    ),
    ("core.option", include_str!("../library/core/option.fos")),
    ("core.byte", include_str!("../library/core/byte.fos")),
    ("core.bytes", include_str!("../library/core/bytes.fos")),
    ("std.io", include_str!("../library/std/io.fos")),
    ("std.resource", include_str!("../library/std/resource.fos")),
    (
        "core.bytes.buffer",
        include_str!("../library/core/bytes/buffer.fos"),
    ),
    ("std.iter", include_str!("../library/std/iter.fos")),
    ("std.iter.map", include_str!("../library/std/iter/map.fos")),
    (
        "std.iter.filter",
        include_str!("../library/std/iter/filter.fos"),
    ),
    (
        "std.iter.take",
        include_str!("../library/std/iter/take.fos"),
    ),
    (
        "std.iter.skip",
        include_str!("../library/std/iter/skip.fos"),
    ),
    (
        "std.collections",
        include_str!("../library/std/collections.fos"),
    ),
    ("core.result", include_str!("../library/core/result.fos")),
    (
        "core.ordering",
        include_str!("../library/core/ordering.fos"),
    ),
    ("core.list", include_str!("../library/core/list.fos")),
    ("std.sequence", include_str!("../library/std/sequence.fos")),
    (
        "core.code_point",
        include_str!("../library/core/code_point.fos"),
    ),
    ("core.bool", include_str!("../library/core/bool.fos")),
    ("core.int", include_str!("../library/core/int.fos")),
    ("core.float", include_str!("../library/core/float.fos")),
    ("core.string", include_str!("../library/core/string.fos")),
    ("core.symbol", include_str!("../library/core/symbol.fos")),
    (
        "std.collections.map",
        include_str!("../library/std/collections/map.fos"),
    ),
    (
        "std.collections.set",
        include_str!("../library/std/collections/set.fos"),
    ),
    (
        "std.collections.queue",
        include_str!("../library/std/collections/queue.fos"),
    ),
    (
        "std.collections.deque",
        include_str!("../library/std/collections/deque.fos"),
    ),
    (
        "std.collections.stack",
        include_str!("../library/std/collections/stack.fos"),
    ),
    ("core.range", include_str!("../library/core/range.fos")),
    ("std.fs", include_str!("../library/std/fs.fos")),
    ("std.path", include_str!("../library/std/path.fos")),
    ("std.uri", include_str!("../library/std/uri.fos")),
    ("std.env", include_str!("../library/std/env.fos")),
    ("std.process", include_str!("../library/std/process.fos")),
    ("std.toml", include_str!("../library/std/toml.fos")),
    ("std.net.tcp", include_str!("../library/std/net/tcp.fos")),
    ("std.time", include_str!("../library/std/time.fos")),
    (
        "std.time.civil",
        include_str!("../library/std/time/civil.fos"),
    ),
    (
        "std.time.zone",
        include_str!("../library/std/time/zone.fos"),
    ),
    (
        "std.time.format",
        include_str!("../library/std/time/format.fos"),
    ),
];

fn intrinsic_key_registered(key: &str) -> bool {
    matches!(
        key,
        "byte.valid"
            | "byte.unchecked"
            | "bytes.empty"
            | "bytes.from_list"
            | "bytes.from_hex"
            | "bytes.concat"
            | "bytes.slice"
            | "bytes.to_list"
            | "bytes.hex"
            | "bytes.encode_utf8"
            | "bytes.utf8_valid"
            | "bytes.decode_utf8"
            | "list.push"
            | "list.append"
            | "byte_buffer.empty"
            | "byte_buffer.with_capacity"
            | "byte_buffer.push"
            | "byte_buffer.extend"
            | "byte_buffer.clear"
            | "byte_buffer.truncate"
            | "byte_buffer.reserve"
            | "byte_buffer.freeze"
            | "byte_buffer.snapshot"
            | "io.read_text"
            | "io.write_text"
            | "io.read_bytes"
            | "io.write_bytes"
            | "io.read_range"
            | "io.append_bytes"
            | "io.file_length"
            | "io.list_directory"
            | "io.exists"
            | "io.is_file"
            | "io.is_directory"
            | "io.create_directory"
            | "io.create_directory_all"
            | "io.remove_file"
            | "io.remove_directory"
            | "io.rename"
            | "io.copy_file"
            | "io.join"
            | "io.parent"
            | "io.file_name"
            | "io.extension"
            | "io.canonicalize"
            | "io.current_directory"
            | "time.wall_now"
            | "time.monotonic_now"
            | "tcp.listen"
            | "tcp.connect"
            | "tcp.accept"
            | "tcp.read"
            | "tcp.write"
            | "tcp.read_bytes"
            | "tcp.write_bytes"
            | "tcp.set_timeout"
            | "tcp.close_listener"
            | "tcp.close_connection"
            | "float.format"
    )
}

fn utf8_source_root(root: &Path) -> Result<Utf8PathBuf, FosterError> {
    if !root.is_dir() {
        return Err(FosterError::runtime(format!(
            "package source root `{}` is not a directory",
            root.display()
        )));
    }
    Utf8PathBuf::from_path_buf(root.to_path_buf()).map_err(|path| {
        FosterError::runtime(format!(
            "package source root is not valid UTF-8: `{}`",
            path.display()
        ))
    })
}

fn mounted_components(prefix: &str, local: &[String]) -> Vec<String> {
    let mut mounted = vec![prefix.to_owned()];
    let local = if local.first().is_some_and(|name| name == "main") {
        &local[1..]
    } else {
        local
    };
    mounted.extend_from_slice(local);
    mounted
}

fn module_components(path: &Utf8Path, strip_extension: bool) -> Result<Vec<String>, FosterError> {
    let mut components = path
        .components()
        .map(|component| component.as_str().to_owned())
        .collect::<Vec<_>>();
    if strip_extension {
        let last = components.last_mut().expect("a source file has a filename");
        *last = last
            .strip_suffix(".fos")
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
        Some(name) if name.starts_with('.') => true,
        Some("target") => true,
        Some("documentation") => entry.depth() == 1,
        _ => false,
    }
}
