//! ZIP-based executable Foster packages.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{Read, Seek, Write};
use std::path::{Component, Path, PathBuf};

use walkdir::WalkDir;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipArchive, ZipWriter};

use crate::error::FosterError;

pub const EXTENSION: &str = "fpk";
pub const MANIFEST_PATH: &str = "META-INF/foster.json";
pub const BYTECODE_PATH: &str = "app/main.fbc";
pub const RESOURCE_PREFIX: &str = "resources/";
const FORMAT_VERSION: u64 = 1;
const MAX_ENTRIES: usize = 100_000;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_BYTECODE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_RESOURCE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_TOTAL_RESOURCE_BYTES: u64 = 512 * 1024 * 1024;

/// Contents needed to execute a validated package.
pub struct ExecutablePackage {
    pub bytecode: Vec<u8>,
    pub resources: Vec<(PathBuf, Vec<u8>)>,
}

/// Writes a deterministic executable package containing bytecode and optional resources.
pub fn write_package(
    output: impl AsRef<Path>,
    bytecode: &[u8],
    resources: Option<&Path>,
) -> Result<(), FosterError> {
    let output = output.as_ref();
    let resources = collect_resources(resources)?;
    let file = File::create(output).map_err(io_error)?;
    let mut archive = ZipWriter::new(file);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .last_modified_time(DateTime::default())
        .unix_permissions(0o644);

    let manifest = serde_json::to_vec_pretty(&serde_json::json!({
        "format": FORMAT_VERSION,
        "entrypoint": BYTECODE_PATH,
        "resources": RESOURCE_PREFIX,
    }))
    .map_err(|error| FosterError::runtime(format!("cannot encode package manifest: {error}")))?;
    write_entry(&mut archive, MANIFEST_PATH, &manifest, options)?;
    write_entry(&mut archive, BYTECODE_PATH, bytecode, options)?;

    for (name, bytes) in resources {
        write_entry(&mut archive, &name, &bytes, options)?;
    }

    archive.finish().map_err(zip_error)?;
    Ok(())
}

fn collect_resources(resources: Option<&Path>) -> Result<Vec<(String, Vec<u8>)>, FosterError> {
    let Some(root) = resources else {
        return Ok(Vec::new());
    };
    if !root.is_dir() {
        return Err(FosterError::runtime(format!(
            "resource root `{}` is not a directory",
            root.display()
        )));
    }
    let mut resources = Vec::new();
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry
            .map_err(|error| FosterError::runtime(format!("cannot walk resources: {error}")))?;
        if entry.file_type().is_symlink() {
            return Err(FosterError::runtime(format!(
                "resource tree contains a symbolic link: `{}`",
                entry.path().display()
            )));
        }
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(root)
            .map_err(|error| FosterError::runtime(format!("cannot form resource path: {error}")))?;
        let name = format!("{RESOURCE_PREFIX}{}", portable_relative_path(relative)?);
        resources.push((name, fs::read(entry.path()).map_err(io_error)?));
    }
    resources.sort_by(|left, right| left.0.cmp(&right.0));
    let mut portable_names = HashSet::new();
    for (name, _) in &resources {
        if !portable_names.insert(name.to_ascii_lowercase()) {
            return Err(FosterError::runtime(format!(
                "resource paths must not differ only by case near `{name}`"
            )));
        }
    }
    Ok(resources)
}

/// Reads and defensively validates an executable package without extracting it.
pub fn read_package(path: impl AsRef<Path>) -> Result<ExecutablePackage, FosterError> {
    let file = File::open(path.as_ref()).map_err(io_error)?;
    let mut archive = ZipArchive::new(file).map_err(zip_error)?;
    if archive.len() > MAX_ENTRIES {
        return Err(FosterError::runtime(format!(
            "package contains too many entries (maximum {MAX_ENTRIES})"
        )));
    }
    let mut names = HashSet::new();
    let mut portable_names = HashSet::new();
    let mut bytecode = None;
    let mut manifest = None;
    let mut resources = Vec::new();
    let mut total_resource_bytes = 0u64;

    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(zip_error)?;
        let name = entry.name().to_owned();
        validate_entry_name(&name)?;
        if !names.insert(name.clone()) {
            return Err(FosterError::runtime(format!(
                "package contains duplicate entry `{name}`"
            )));
        }
        if !portable_names.insert(name.to_ascii_lowercase()) {
            return Err(FosterError::runtime(format!(
                "package entries differ only by case near `{name}`"
            )));
        }
        if entry.is_dir() {
            continue;
        }
        match name.as_str() {
            MANIFEST_PATH => manifest = Some(read_entry(&mut entry, MAX_MANIFEST_BYTES)?),
            BYTECODE_PATH => bytecode = Some(read_entry(&mut entry, MAX_BYTECODE_BYTES)?),
            _ if name.starts_with(RESOURCE_PREFIX) => {
                let relative = &name[RESOURCE_PREFIX.len()..];
                if relative.is_empty() {
                    return Err(FosterError::runtime(
                        "package contains an empty resource path",
                    ));
                }
                total_resource_bytes = total_resource_bytes
                    .checked_add(entry.size())
                    .ok_or_else(|| FosterError::runtime("package resources are too large"))?;
                if total_resource_bytes > MAX_TOTAL_RESOURCE_BYTES {
                    return Err(FosterError::runtime(format!(
                        "package resources exceed {} bytes",
                        MAX_TOTAL_RESOURCE_BYTES
                    )));
                }
                let bytes = read_entry(&mut entry, MAX_RESOURCE_BYTES)?;
                resources.push((PathBuf::from(relative), bytes));
            }
            _ => {}
        }
    }

    validate_manifest(
        manifest
            .as_deref()
            .ok_or_else(|| FosterError::runtime(format!("package is missing `{MANIFEST_PATH}`")))?,
    )?;
    let bytecode = bytecode
        .ok_or_else(|| FosterError::runtime(format!("package is missing `{BYTECODE_PATH}`")))?;
    resources.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(ExecutablePackage {
        bytecode,
        resources,
    })
}

fn read_entry<R: Read>(
    entry: &mut zip::read::ZipFile<'_, R>,
    maximum: u64,
) -> Result<Vec<u8>, FosterError> {
    if entry.size() > maximum {
        return Err(FosterError::runtime(format!(
            "package entry `{}` exceeds {maximum} bytes",
            entry.name()
        )));
    }
    let capacity = usize::try_from(entry.size())
        .map_err(|_| FosterError::runtime("package entry is too large for this platform"))?;
    let mut bytes = Vec::with_capacity(capacity);
    entry
        .take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() as u64 > maximum {
        return Err(FosterError::runtime("expanded package entry is too large"));
    }
    Ok(bytes)
}

fn write_entry<W: Write + Seek>(
    archive: &mut ZipWriter<W>,
    name: &str,
    bytes: &[u8],
    options: SimpleFileOptions,
) -> Result<(), FosterError> {
    archive.start_file(name, options).map_err(zip_error)?;
    archive.write_all(bytes).map_err(io_error)
}

fn validate_manifest(bytes: &[u8]) -> Result<(), FosterError> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| FosterError::runtime(format!("invalid package manifest: {error}")))?;
    let format = value.get("format").and_then(serde_json::Value::as_u64);
    let entrypoint = value.get("entrypoint").and_then(serde_json::Value::as_str);
    let resources = value.get("resources").and_then(serde_json::Value::as_str);
    if format != Some(FORMAT_VERSION) {
        return Err(FosterError::runtime(format!(
            "unsupported Foster package format; expected {FORMAT_VERSION}"
        )));
    }
    if entrypoint != Some(BYTECODE_PATH) || resources != Some(RESOURCE_PREFIX) {
        return Err(FosterError::runtime(
            "package manifest has an unsupported layout",
        ));
    }
    Ok(())
}

fn portable_relative_path(path: &Path) -> Result<String, FosterError> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str().ok_or_else(|| {
                FosterError::runtime(format!(
                    "resource path is not valid UTF-8: `{}`",
                    path.display()
                ))
            })?),
            _ => {
                return Err(FosterError::runtime(format!(
                    "resource path is not portable: `{}`",
                    path.display()
                )));
            }
        }
    }
    Ok(parts.join("/"))
}

fn validate_entry_name(name: &str) -> Result<(), FosterError> {
    if name.contains('\\') || name.starts_with('/') {
        return Err(FosterError::runtime(format!(
            "package entry has an unsafe path: `{name}`"
        )));
    }
    let path = Path::new(name);
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(FosterError::runtime(format!(
            "package entry has an unsafe path: `{name}`"
        )));
    }
    Ok(())
}

fn io_error(error: std::io::Error) -> FosterError {
    FosterError::runtime(error.to_string())
}

fn zip_error(error: zip::result::ZipError) -> FosterError {
    FosterError::runtime(format!("invalid Foster package: {error}"))
}
