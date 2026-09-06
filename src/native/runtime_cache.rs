//! Cache the Rust runtime separately from program objects and entry shims.
use super::*;
use std::ffi::OsStr;
use std::hash::{DefaultHasher, Hash, Hasher};

const LIBRARY: &str = "libfoster_native_runtime.rlib";
// Keep runtime exports in the object pulled in by initialization. The Foster object is passed as
// a linker argument, which some host linkers process after Rust's archives.
const RUNTIME_FLAGS: &[&str] = &[
    "--edition=2024",
    "--crate-name=foster_native_runtime",
    "--crate-type=rlib",
    "-Ccodegen-units=1",
];

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
    let identity = identity(rustc, &version.stdout, source, options);
    cached_library(&cache_directory()?, &identity, |directory, output| {
        let input = directory.join("runtime.rs");
        fs::write(&input, source)?;
        let result = Command::new(rustc)
            .args(RUNTIME_FLAGS)
            .args(["-C", optimization(options)])
            .arg(&input)
            .arg("-o")
            .arg(output)
            .output()?;
        if !result.status.success() {
            return Err(std::io::Error::other(format!(
                "runtime compilation failed with {}: {}",
                result.status,
                String::from_utf8_lossy(&result.stderr).trim()
            )));
        }
        Ok(())
    })
    .map_err(|error| native_error(format!("cannot build cached native runtime: {error}")))
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
        "foster-native-runtime-cache-v1\nrustc={rustc:?}\n{}\nflags={RUNTIME_FLAGS:?}\n{}\n{source}",
        String::from_utf8_lossy(version),
        optimization(options),
    )
}

fn cached_library(
    root: &Path,
    identity: &str,
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
                    let library = cached_library(&temporary.path, "shared", |_, output| {
                        builds.fetch_add(1, Ordering::SeqCst);
                        fs::write(output, b"complete runtime")
                    })
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
        let failed = cached_library(&temporary.path, "runtime", |_, output| {
            fs::write(output, b"partial archive")?;
            Err(std::io::Error::other("interrupted build"))
        });
        assert!(failed.is_err());
        let library = cached_library(&temporary.path, "runtime", |_, output| {
            fs::write(output, b"complete archive")
        })
        .unwrap();
        assert_eq!(fs::read(&library).unwrap(), b"complete archive");
        fs::remove_file(&library).unwrap();
        cached_library(&temporary.path, "runtime", |_, output| {
            fs::write(output, b"rebuilt archive")
        })
        .unwrap();
        assert_eq!(fs::read(&library).unwrap(), b"rebuilt archive");
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
            let library = cached_library(&temporary.path, &identity, |_, output| {
                fs::write(output, &identity)
            })
            .unwrap();
            assert!(libraries.insert(library));
        }
    }
}
