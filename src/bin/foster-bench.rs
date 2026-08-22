use std::env;
use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use foster::vm::{CompileOptions, Machine, Program, ProgramMetrics};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let options = Options::parse()?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let foster_path = root.join("benchmarks/fibonacci.fos");
    let lua_path = root.join("benchmarks/fibonacci.lua");
    let source = fs::read_to_string(&foster_path)?;

    let front_end = measure(options.compile_iterations, || {
        black_box(foster::compile(black_box(&source))).unwrap();
    });
    let compilation = foster::compile(&source)?;
    let unoptimized_lowering = measure(options.compile_iterations, || {
        black_box(foster::vm::compile_with_options(
            black_box(&compilation),
            CompileOptions { optimize: false },
        ))
        .unwrap();
    });
    let optimized_lowering = measure(options.compile_iterations, || {
        black_box(foster::vm::compile_with_options(
            black_box(&compilation),
            CompileOptions { optimize: true },
        ))
        .unwrap();
    });

    let unoptimized =
        foster::vm::compile_with_options(&compilation, CompileOptions { optimize: false })?;
    let optimized =
        foster::vm::compile_with_options(&compilation, CompileOptions { optimize: true })?;
    foster::vm::verify(&unoptimized)?;
    foster::vm::verify(&optimized)?;

    let unoptimized_result = Machine::new(&unoptimized).run_main()?;
    let optimized_result = Machine::new(&optimized).run_main()?;
    if unoptimized_result != optimized_result {
        return Err("optimized and unoptimized VM results differ".into());
    }

    for _ in 0..options.warmup_iterations {
        black_box(Machine::new(&unoptimized).run_main()?);
        black_box(Machine::new(&optimized).run_main()?);
    }
    let unoptimized_runtime = benchmark_program(&unoptimized, options.runtime_iterations)?;
    let optimized_runtime = benchmark_program(&optimized, options.runtime_iterations)?;

    println!("Foster benchmark: fibonacci(20) = {optimized_result}");
    println!();
    println!("compiler (average per iteration)");
    println!(
        "  front end + checked HIR : {}",
        display_average(front_end, options.compile_iterations)
    );
    println!(
        "  bytecode, no optimizer : {}",
        display_average(unoptimized_lowering, options.compile_iterations)
    );
    println!(
        "  bytecode + optimizer   : {}",
        display_average(optimized_lowering, options.compile_iterations)
    );
    println!();
    print_metrics("unoptimized", unoptimized.metrics());
    print_metrics("optimized  ", optimized.metrics());
    println!();
    println!(
        "runtime (average of {} iterations)",
        options.runtime_iterations
    );
    println!(
        "  Foster, no optimizer   : {}",
        display_average(unoptimized_runtime, options.runtime_iterations)
    );
    println!(
        "  Foster, optimized      : {}",
        display_average(optimized_runtime, options.runtime_iterations)
    );

    if options.skip_lua {
        println!("  Lua                    : skipped");
    } else if let Some(lua) = options.lua.or_else(find_lua) {
        let (elapsed, result) = benchmark_lua(&lua, &lua_path, options.runtime_iterations)?;
        if result != optimized_result.to_string() {
            return Err(
                format!("Lua returned `{result}`, Foster returned `{optimized_result}`").into(),
            );
        }
        println!(
            "  Lua ({}) : {}",
            lua.display(),
            display_average(elapsed, options.runtime_iterations)
        );
    } else {
        println!("  Lua                    : unavailable (tried lua, lua54, and luajit)");
    }

    Ok(())
}

fn benchmark_program(
    program: &Program,
    iterations: u32,
) -> Result<Duration, Box<dyn std::error::Error>> {
    let machine = Machine::new(program);
    let start = Instant::now();
    for _ in 0..iterations {
        black_box(machine.run_main()?);
    }
    Ok(start.elapsed())
}

fn benchmark_lua(
    lua: &Path,
    source: &Path,
    iterations: u32,
) -> Result<(Duration, String), Box<dyn std::error::Error>> {
    let start = Instant::now();
    let output = Command::new(lua)
        .arg(source)
        .arg(iterations.to_string())
        .output()?;
    let elapsed = start.elapsed();
    if !output.status.success() {
        return Err(format!(
            "Lua benchmark failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok((elapsed, String::from_utf8(output.stdout)?.trim().to_owned()))
}

fn find_lua() -> Option<PathBuf> {
    ["lua", "lua54", "luajit"].into_iter().find_map(|name| {
        Command::new(name)
            .arg("-v")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|_| PathBuf::from(name))
    })
}

fn measure(iterations: u32, mut operation: impl FnMut()) -> Duration {
    let start = Instant::now();
    for _ in 0..iterations {
        operation();
    }
    start.elapsed()
}

fn display_average(duration: Duration, iterations: u32) -> String {
    let nanos = duration.as_nanos() / u128::from(iterations);
    if nanos >= 1_000_000 {
        format!("{:.3} ms", nanos as f64 / 1_000_000.0)
    } else if nanos >= 1_000 {
        format!("{:.3} us", nanos as f64 / 1_000.0)
    } else {
        format!("{nanos} ns")
    }
}

fn print_metrics(label: &str, metrics: ProgramMetrics) {
    println!(
        "bytecode {label}: {:4} instructions, {:3} registers, {:3} constants, {} functions",
        metrics.instructions, metrics.registers, metrics.constants, metrics.functions
    );
}

struct Options {
    compile_iterations: u32,
    runtime_iterations: u32,
    warmup_iterations: u32,
    lua: Option<PathBuf>,
    skip_lua: bool,
}

impl Options {
    fn parse() -> Result<Self, Box<dyn std::error::Error>> {
        let mut options = Self {
            compile_iterations: 25,
            runtime_iterations: 20,
            warmup_iterations: 2,
            lua: None,
            skip_lua: false,
        };
        let mut arguments = env::args().skip(1);
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--compile-iterations" => {
                    options.compile_iterations = positive(&mut arguments, &argument)?
                }
                "--runtime-iterations" => {
                    options.runtime_iterations = positive(&mut arguments, &argument)?
                }
                "--warmup-iterations" => {
                    options.warmup_iterations = positive(&mut arguments, &argument)?
                }
                "--lua" => {
                    options.lua = Some(PathBuf::from(
                        arguments.next().ok_or("`--lua` requires an executable")?,
                    ))
                }
                "--skip-lua" => options.skip_lua = true,
                _ => return Err(format!("unknown benchmark option `{argument}`").into()),
            }
        }
        Ok(options)
    }
}

fn positive(
    arguments: &mut impl Iterator<Item = String>,
    flag: &str,
) -> Result<u32, Box<dyn std::error::Error>> {
    let value = arguments
        .next()
        .ok_or_else(|| format!("`{flag}` requires a value"))?
        .parse::<u32>()?;
    if value == 0 {
        return Err(format!("`{flag}` must be greater than zero").into());
    }
    Ok(value)
}
