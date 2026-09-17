//! Compare the same optimized workload across compiler revisions, in release builds.
//! cargo run --release --example measure_backends -- benchmarks/scalar_cse.fos target/cse-native.exe
use std::{hint::black_box, time::Instant};

fn median(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[samples.len() / 2]
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().collect::<Vec<_>>();
    let source = std::fs::read_to_string(&args[1])?;
    let output = std::path::Path::new(&args[2]);
    let compilation = foster::compile(&source)?;
    let mut vm_compile = Vec::new();
    let mut native_compile = Vec::new();
    let mut vm_run = Vec::new();
    let mut native_run = Vec::new();
    let vm_options = foster::vm::CompileOptions { optimize: true };
    let native_options = foster::native::CompileOptions { optimize: true };
    for sample in 0..10 {
        let start = Instant::now();
        let program = foster::vm::compile_with_options(&compilation, vm_options)?;
        let compile_time = start.elapsed().as_secs_f64() * 1000.0;
        let start = Instant::now();
        let result = foster::vm::Machine::new(&program).run_main()?;
        let run_time = start.elapsed().as_secs_f64() * 1000.0;
        black_box(&result);
        if sample > 0 {
            vm_compile.push(compile_time);
            vm_run.push(run_time);
        }
        let start = Instant::now();
        black_box(foster::native::compile_object(
            &compilation,
            native_options,
        )?);
        if sample > 0 {
            native_compile.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        if sample == 0 {
            println!("vm_result={result:?}");
            println!(
                "vm_instructions={}",
                program
                    .functions
                    .values()
                    .map(|f| f.instructions.len())
                    .sum::<usize>()
            );
        }
    }
    foster::native::build_executable(&compilation, output, native_options)?;
    for sample in 0..10 {
        let start = Instant::now();
        let result = std::process::Command::new(output).output()?;
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        if !result.status.success() {
            return Err("native execution failed".into());
        }
        if sample == 0 {
            println!(
                "native_result={}",
                String::from_utf8_lossy(&result.stdout).trim()
            );
        }
        if sample > 0 {
            native_run.push(elapsed);
        }
    }
    println!(
        "median_ms vm_compile={:.3} native_compile={:.3} vm_run={:.3} native_process={:.3}",
        median(vm_compile),
        median(native_compile),
        median(vm_run),
        median(native_run)
    );
    Ok(())
}
