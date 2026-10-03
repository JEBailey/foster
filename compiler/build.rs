use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rustc-check-cfg=cfg(foster_bootstrap)");
    println!("cargo:rerun-if-changed=../library");
    println!("cargo:rerun-if-changed=src");
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo supplies OUT_DIR"));
    std::thread::Builder::new()
        .name("foster-library-build".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            for (name, full) in [("bootstrap", false), ("standard", true)] {
                let library = foster_bootstrap::package::build_embedded_library(full)
                    .unwrap_or_else(|error| panic!("{name} library: {error}"));
                let bytes = foster_bootstrap::library::encode(&library)
                    .unwrap_or_else(|error| panic!("{name} library encoding: {error}"));
                fs::write(output.join(format!("{name}.flib")), bytes)
                    .expect("embedded library must be writable");
            }
        })
        .expect("cannot start Foster library compiler")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
}
