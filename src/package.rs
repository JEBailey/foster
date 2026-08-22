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
        package.install_standard_modules_if_imported()?;
        package.install_bytes_bootstrap()?;
        package.install_byte_buffer_bootstrap()?;
        package.install_list_bootstrap()?;
        package.install_string_bootstrap()?;
        package.install_symbol_bootstrap()?;
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
        package.install_standard_modules_if_imported()?;
        package.install_bytes_bootstrap()?;
        package.install_byte_buffer_bootstrap()?;
        package.install_list_bootstrap()?;
        package.install_string_bootstrap()?;
        package.install_symbol_bootstrap()?;
        package.validate()?;
        Ok(package)
    }

    fn install_string_bootstrap(&mut self) -> Result<(), FosterError> {
        self.install_bootstrap(BootstrapModule::types_only(
            "core.string",
            include_str!("../library/core/string.fos"),
            &["String"],
        ))
    }

    fn install_bytes_bootstrap(&mut self) -> Result<(), FosterError> {
        self.install_bootstrap(BootstrapModule::types_only(
            "core.bytes",
            include_str!("../library/core/bytes.fos"),
            &["RawBytes", "Bytes"],
        ))
    }

    fn install_byte_buffer_bootstrap(&mut self) -> Result<(), FosterError> {
        self.install_bootstrap(BootstrapModule::types_only(
            "core.bytes.buffer",
            include_str!("../library/core/bytes/buffer.fos"),
            &["RawByteBuffer", "ByteBuffer"],
        ))
    }

    fn install_list_bootstrap(&mut self) -> Result<(), FosterError> {
        self.install_bootstrap(BootstrapModule::types_only(
            "core.list",
            include_str!("../library/core/list.fos"),
            &["RawList", "List"],
        ))
    }

    fn install_symbol_bootstrap(&mut self) -> Result<(), FosterError> {
        self.install_bootstrap(BootstrapModule::full(
            "core.symbol",
            include_str!("../library/core/symbol.fos"),
        ))
    }

    fn install_bootstrap(&mut self, bootstrap: BootstrapModule) -> Result<(), FosterError> {
        if self.modules.contains_key(bootstrap.name) {
            return Ok(());
        }
        self.modules.entry("core".into()).or_insert(Module {
            name: "core".into(),
            source_path: None,
            program: None,
            source: None,
        });
        let mut program = crate::parse(bootstrap.source).map_err(|error| {
            FosterError::runtime(format!(
                "embedded module `{}` is invalid: {error}",
                bootstrap.name
            ))
        })?;
        if let BootstrapMode::TypesOnly(types) = bootstrap.mode {
            program.imports.clear();
            program.constants.clear();
            program.variants.clear();
            program.functions.clear();
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
            },
        );
        Ok(())
    }

    fn install_standard_modules_if_imported(&mut self) -> Result<(), FosterError> {
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
        });
        for namespace in ["core.bytes", "std", "std.net"] {
            self.modules.entry(namespace.into()).or_insert(Module {
                name: namespace.into(),
                source_path: None,
                program: None,
                source: None,
            });
        }
        for (name, source) in EMBEDDED_MODULES {
            let program = crate::parse(source).map_err(|error| {
                FosterError::runtime(format!("embedded module `{name}` is invalid: {error}"))
            })?;
            self.modules.insert(
                (*name).into(),
                Module {
                    name: (*name).into(),
                    source_path: embedded_source_path(name),
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
            } else if path.extension() == Some("fos") {
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
        let mut intrinsic_keys = HashSet::new();
        for module in self.modules.values() {
            let Some(program) = &module.program else {
                continue;
            };
            let mut definitions = HashSet::new();
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
                        "std.fs" | "std.path" | "std.env" => key.starts_with("io."),
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
                if let Some((owner, _)) = function.name.split_once('.') {
                    if !program.records.iter().any(|record| record.name == owner)
                        && !matches!(owner, "Byte" | "Bytes" | "ByteBuffer" | "String")
                    {
                        return Err(FosterError::runtime(format!(
                            "module `{}` defines associated function `{}` for unknown record type `{owner}`",
                            module.name, function.name
                        )));
                    }
                    if function
                        .parameters
                        .first()
                        .is_some_and(|parameter| parameter.name == "self")
                        && function.intrinsic.is_none()
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

fn embedded_source_path(module: &str) -> Option<Utf8PathBuf> {
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
    ("core.option", include_str!("../library/core/option.fos")),
    ("core.byte", include_str!("../library/core/byte.fos")),
    ("core.bytes", include_str!("../library/core/bytes.fos")),
    ("std.io", include_str!("../library/std/io.fos")),
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
    ("std.env", include_str!("../library/std/env.fos")),
    ("std.net.tcp", include_str!("../library/std/net/tcp.fos")),
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
            | "io.list_directory"
            | "io.exists"
            | "io.is_file"
            | "io.is_directory"
            | "io.join"
            | "io.parent"
            | "io.file_name"
            | "io.extension"
            | "io.canonicalize"
            | "io.current_directory"
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
    )
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
