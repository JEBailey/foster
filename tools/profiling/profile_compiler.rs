//! Run with FOSTER_COMPILER_PROFILE=1 and --features compiler-profile.
//! Measures compilation only: no Foster execution or native runtime linking.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "benchmarks/fibonacci.fos".into());
    let source = std::fs::read_to_string(path)?;
    for _ in 0..5 {
        let compilation = foster::compile(&source)?;
        for optimize in [false, true] {
            std::hint::black_box(foster::vm::compile_with_options(
                &compilation,
                foster::vm::CompileOptions { optimize },
            )?);
            let native = foster::native::prepare_with_options(
                &compilation,
                foster::native::CompileOptions { optimize },
            )?;
            std::hint::black_box(
                native.compile_object(foster::native::CompileOptions { optimize })?,
            );
        }
    }
    Ok(())
}
