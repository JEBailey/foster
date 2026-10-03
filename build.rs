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
    println!("cargo:rustc-check-cfg=cfg(foster_native_formatter)");
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
        if name == "format" && env::var("HOST").ok() == env::var("TARGET").ok() {
            // Compile the same formatting policy, with a consuming String entry boundary.
            let start = source
                .find("func main(arguments:")
                .expect("formatter entry");
            let end = source[start..]
                .find("\ntest ")
                .map(|end| start + end)
                .unwrap_or(source.len());
            let native_source = format!(
                "{}func main(arguments: Arguments) -> String [consume arguments] {{\n    format(arguments.values[0])\n}}\n{}",
                &source[..start],
                &source[end..]
            );
            let compilation =
                foster_compiler::compile(&native_source).expect("native formatter check");
            let object = foster_compiler::native::compile_object(&compilation, Default::default())
                .expect("native formatter compilation");
            let path = output.join("format.obj");
            fs::write(&path, &object.bytes).expect("native formatter object must be writable");
            fs::write(
                output.join("format_constants.rs"),
                format!(
                    "pub const CONSTANTS: &[&str] = &{:?};",
                    object.runtime_strings()
                ),
            )
            .expect("formatter constants must be writable");
            cc::Build::new().object(path).compile("foster_formatter");
            println!("cargo:rustc-cfg=foster_native_formatter");
        }
    }
}
