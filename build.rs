use std::{env, fs, path::PathBuf};

fn main() {
    // Match the compiler command's stack budget on Windows and other hosts.
    std::thread::Builder::new()
        .name("foster-tools-build".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(compile_tools)
        .expect("cannot start Foster tool compiler")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
}

fn compile_tools() {
    println!("cargo:rerun-if-changed=tools/driver");
    println!("cargo:rerun-if-changed=library");
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo supplies OUT_DIR"));
    for name in ["init", "project", "format", "documentation"] {
        let path = format!("tools/driver/{name}.fos");
        let source = fs::read_to_string(&path).expect("embedded tool source must exist");
        let compilation =
            foster_compiler::compile(&source).unwrap_or_else(|error| panic!("{path}: {error}"));
        let program = foster_compiler::vm::compile(&compilation)
            .unwrap_or_else(|error| panic!("{path}: {error}"));
        let bytes = foster_compiler::vm::encode_program(&program)
            .unwrap_or_else(|error| panic!("{path}: {error}"));
        fs::write(output.join(format!("{name}.fbc")), bytes)
            .expect("embedded bytecode must be writable");
    }
}
