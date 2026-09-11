//! Cache the Rust runtime separately from program objects and entry shims.
use super::*;
use std::ffi::OsStr;
use std::hash::{DefaultHasher, Hash, Hasher};

const LIBRARY: &str = "libfoster_native_runtime.rlib";
// Keep runtime exports in the object pulled in by initialization. The Foster object is passed as
// a linker argument, which some host linkers process after Rust's archives.
const MANIFEST: &str = include_str!("../../runtime/native-build.toml");
const LOCKFILE: &str = include_str!("../../runtime/native-build.lock");

#[derive(serde::Serialize, serde::Deserialize)]
struct Dependencies {
    may: PathBuf,
    search_paths: BTreeSet<String>,
    archives: BTreeSet<PathBuf>,
}

fn dependencies(directory: &Path) -> std::io::Result<Dependencies> {
    Ok(serde_json::from_slice(&fs::read(
        directory.join("dependencies.json"),
    )?)?)
}

fn dependencies_complete(directory: &Path) -> bool {
    dependencies(directory).is_ok_and(|dependencies| {
        dependencies.may.is_file()
            && dependencies.archives.iter().all(|path| path.is_file())
            && dependencies.search_paths.iter().all(|path| {
                Path::new(path.split_once('=').map_or(path.as_str(), |(_, path)| path)).is_dir()
            })
    })
}

pub(super) fn library(
    rustc: &OsStr,
    source: &str,
    options: CompileOptions,
) -> Result<PathBuf, FosterError> {
    let version = Command::new(rustc)
        .arg("-vV")
        .output()
        .map_err(|error| native_error(format!("cannot identify native Rust toolchain: {error}")))?;
    if !version.status.success() {
        return Err(native_error(format!(
            "cannot identify native Rust toolchain: {}",
            String::from_utf8_lossy(&version.stderr).trim()
        )));
    }
    let version_text = String::from_utf8_lossy(&version.stdout);
    let host = version_text
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .ok_or_else(|| native_error("native Rust toolchain did not report a host target"))?;
    let identity = identity(rustc, &version.stdout, source, options);
    cached_library(
        &cache_directory()?,
        &identity,
        dependencies_complete,
        |directory, output| {
            let input = directory.join("runtime.rs");
            fs::write(&input, source)?;
            fs::write(directory.join("Cargo.toml"), MANIFEST)?;
            fs::write(directory.join("Cargo.lock"), LOCKFILE)?;
            let mut command =
                Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
            command
                .current_dir(directory)
                .env("RUSTC", rustc)
                // A standalone host runtime must not inherit a cross-compilation
                // target or custom flags from the invoking Cargo process.
                .env("CARGO_ENCODED_RUSTFLAGS", "")
                .args([
                    "--config",
                    "profile.dev.codegen-units=1",
                    "--config",
                    "profile.release.codegen-units=1",
                ])
                .args([
                    "build",
                    "--locked",
                    "--lib",
                    "--message-format=json-render-diagnostics",
                    "--manifest-path",
                ])
                .arg(directory.join("Cargo.toml"))
                .args(["--target", host])
                .arg("--target-dir")
                .arg(directory.join("target"));
            if options.optimize {
                command.arg("--release");
            }
            let result = command.output()?;
            if !result.status.success() {
                return Err(std::io::Error::other(format!(
                    "runtime compilation failed with {}: {}",
                    result.status,
                    String::from_utf8_lossy(&result.stderr).trim()
                )));
            }
            let mut native_paths = BTreeSet::new();
            let mut archives = BTreeSet::new();
            let mut may = None;
            let mut runtime = None;
            for line in String::from_utf8_lossy(&result.stdout).lines() {
                let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else {
                    continue;
                };
                if message["reason"] == "build-script-executed"
                    && let Some(paths) = message["linked_paths"].as_array()
                {
                    native_paths.extend(
                        paths
                            .iter()
                            .filter_map(|path| path.as_str().map(str::to_owned)),
                    );
                }
                if message["reason"] == "compiler-artifact"
                    && let Some(files) = message["filenames"].as_array()
                {
                    for file in files.iter().filter_map(|file| file.as_str()) {
                        let path = PathBuf::from(file);
                        if path.extension().is_some_and(|ext| ext == "rlib") {
                            if message["target"]["name"] == "may" {
                                may = Some(path.clone());
                            }
                            if message["target"]["name"] == "foster_native_runtime" {
                                runtime = Some(path.clone());
                            }
                            archives.insert(path);
                        }
                    }
                }
            }
            let may =
                may.ok_or_else(|| std::io::Error::other("Cargo did not produce a May archive"))?;
            let runtime = runtime.ok_or_else(|| {
                std::io::Error::other("Cargo did not produce the native runtime archive")
            })?;
            native_paths.insert(format!("dependency={}", may.parent().unwrap().display()));
            fs::write(
                directory.join("dependencies.json"),
                serde_json::to_vec(&Dependencies {
                    may,
                    search_paths: native_paths,
                    archives,
                })?,
            )?;
            fs::copy(runtime, output)?;
            Ok(())
        },
    )
    .map_err(|error| native_error(format!("cannot build cached native runtime: {error}")))
}

// Rustc needs May and its transitive archives when linking the small entry shim,
// and when compiling the full, instrumented runtime used by cleanup tests.
pub(super) fn link_dependencies(command: &mut Command, library: &Path) -> Result<(), FosterError> {
    let dependencies = dependencies(library.parent().unwrap()).map_err(|error| {
        native_error(format!("cannot read native runtime dependencies: {error}"))
    })?;
    command
        .arg("--extern")
        .arg(format!("may={}", dependencies.may.display()));
    for path in dependencies.search_paths {
        command.arg("-L").arg(path);
    }
    Ok(())
}

fn optimization(options: CompileOptions) -> &'static str {
    if options.optimize {
        "opt-level=2"
    } else {
        "opt-level=0"
    }
}

fn cache_directory() -> Result<PathBuf, FosterError> {
    if let Some(directory) = std::env::var_os("FOSTER_NATIVE_CACHE_DIR") {
        return absolute_path(Path::new(&directory));
    }
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
    };
    base.map(|base| base.join("foster").join("native-runtime"))
        .ok_or_else(|| {
            native_error("cannot locate user cache directory; set FOSTER_NATIVE_CACHE_DIR")
        })
}

fn identity(rustc: &OsStr, version: &[u8], source: &str, options: CompileOptions) -> String {
    // Include the full source (and embedded ABI assertions), host/toolchain version, and every
    // runtime compilation option. Changing the build recipe must also change this format version.
    format!(
        "foster-native-runtime-cache-v4\nrustc={rustc:?}\n{}\nmanifest={MANIFEST}\nlock={LOCKFILE}\n{}\n{source}",
        String::from_utf8_lossy(version),
        optimization(options),
    )
}

fn cached_library(
    root: &Path,
    identity: &str,
    valid: impl FnOnce(&Path) -> bool,
    build: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> std::io::Result<PathBuf> {
    let mut hash = DefaultHasher::new();
    identity.hash(&mut hash);
    let directory = root.join(format!("{:016x}", hash.finish()));
    fs::create_dir_all(&directory)?;
    // The OS releases this lock if a compiler crashes. Separate test executables and concurrent
    // CLI builds wait for the first builder rather than compiling or reading a partial archive.
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join("build.lock"))?;
    lock.lock()?;
    let library = directory.join(LIBRARY);
    let manifest = directory.join("identity");
    if fs::read_to_string(&manifest).is_ok_and(|saved| saved == identity)
        && fs::metadata(&library).is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
        && valid(&directory)
    {
        return Ok(library);
    }

    // Publish only successful builds. A failed/interrupted build is retried on the next request.
    let pending = directory.join("building.rlib");
    if manifest.exists() {
        fs::remove_file(&manifest)?;
    }
    build(&directory, &pending)?;
    if library.exists() {
        fs::remove_file(&library)?;
    }
    fs::rename(&pending, &library)?;
    fs::write(manifest, identity)?;
    Ok(library)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn concurrent_builders_reuse_one_complete_runtime() {
        let temporary = TemporaryDirectory::create().unwrap();
        let builds = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    let library = cached_library(
                        &temporary.path,
                        "shared",
                        |_| true,
                        |_, output| {
                            builds.fetch_add(1, Ordering::SeqCst);
                            fs::write(output, b"complete runtime")
                        },
                    )
                    .unwrap();
                    assert_eq!(fs::read(library).unwrap(), b"complete runtime");
                });
            }
        });
        assert_eq!(builds.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn failed_or_missing_archives_are_rebuilt() {
        let temporary = TemporaryDirectory::create().unwrap();
        let failed = cached_library(
            &temporary.path,
            "runtime",
            |_| true,
            |_, output| {
                fs::write(output, b"partial archive")?;
                Err(std::io::Error::other("interrupted build"))
            },
        );
        assert!(failed.is_err());
        let library = cached_library(
            &temporary.path,
            "runtime",
            |_| true,
            |_, output| fs::write(output, b"complete archive"),
        )
        .unwrap();
        assert_eq!(fs::read(&library).unwrap(), b"complete archive");
        fs::remove_file(&library).unwrap();
        cached_library(
            &temporary.path,
            "runtime",
            |_| true,
            |_, output| fs::write(output, b"rebuilt archive"),
        )
        .unwrap();
        assert_eq!(fs::read(&library).unwrap(), b"rebuilt archive");
    }

    #[test]
    fn missing_dependency_archives_invalidate_the_cached_runtime() {
        let temporary = TemporaryDirectory::create().unwrap();
        let builds = AtomicUsize::new(0);
        let build = |directory: &Path, output: &Path| {
            builds.fetch_add(1, Ordering::SeqCst);
            let may = directory.join("libmay.rlib");
            fs::write(&may, b"dependency")?;
            let metadata = Dependencies {
                may: may.clone(),
                search_paths: BTreeSet::from([format!("dependency={}", directory.display())]),
                archives: BTreeSet::from([may]),
            };
            fs::write(
                directory.join("dependencies.json"),
                serde_json::to_vec(&metadata)?,
            )?;
            fs::write(output, b"runtime")
        };
        let library = cached_library(
            &temporary.path,
            "dependencies",
            dependencies_complete,
            build,
        )
        .unwrap();
        cached_library(
            &temporary.path,
            "dependencies",
            dependencies_complete,
            build,
        )
        .unwrap();
        assert_eq!(builds.load(Ordering::SeqCst), 1);
        fs::remove_file(library.parent().unwrap().join("libmay.rlib")).unwrap();
        cached_library(
            &temporary.path,
            "dependencies",
            dependencies_complete,
            build,
        )
        .unwrap();
        assert_eq!(builds.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn source_toolchain_and_optimization_changes_invalidate_the_cache() {
        let temporary = TemporaryDirectory::create().unwrap();
        let mut libraries = HashSet::new();
        for (rustc, version, source, optimize) in [
            ("rustc", "host-a release-a", "runtime-v1", false),
            ("rustc", "host-a release-a", "runtime-v1", true),
            ("rustc", "host-a release-a", "runtime-v2", false),
            ("rustc", "host-a release-b", "runtime-v1", false),
            ("rustc", "host-b release-a", "runtime-v1", false),
            ("other-rustc", "host-a release-a", "runtime-v1", false),
        ] {
            let identity = identity(
                OsStr::new(rustc),
                version.as_bytes(),
                source,
                CompileOptions { optimize },
            );
            let library = cached_library(
                &temporary.path,
                &identity,
                |_| true,
                |_, output| fs::write(output, &identity),
            )
            .unwrap();
            assert!(libraries.insert(library));
        }
    }
}
