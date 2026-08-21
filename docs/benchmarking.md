# Optimization testing and benchmarks

Optimization is enabled by default. It can be selected explicitly from the CLI:

```text
foster run program.fos --optimize
foster run program.fos --no-optimize
```

Library users select the same behavior with `vm::CompileOptions { optimize }` and
`vm::compile_with_options` or `vm::run_with_options`.

## Correctness gates

The VM test suite compiles a representative language corpus both ways, verifies both bytecode
programs, executes them, and requires identical results. A separate structural test requires the
optimizer to reduce instruction and register counts for a representative program. These are stable
CI gates; elapsed-time assertions are deliberately excluded because scheduler load and machine
differences make them unreliable.

## Criterion benchmarks

Criterion provides statistically sampled compiler and VM microbenchmarks, warmup, outlier
analysis, regression comparisons against previous local runs, and HTML reports:

```text
cargo bench
```

Run one suite with `cargo bench --bench compiler` or `cargo bench --bench runtime`. Criterion writes
reports beneath `target/criterion/`; open `target/criterion/report/index.html` for the complete HTML
report. The compiler suite separates front-end plus checked-HIR work from optimized and
unoptimized bytecode lowering. The runtime suite executes already compiled bytecode so compiler
time is not mixed into VM measurements. It includes recursive Fibonacci plus focused workloads
for Foster-defined `String`, `Symbol`, and `Bytes` values. Each workload reports optimized and
unoptimized VM execution separately.

## Lua comparison harness

Run the cross-language harness in release mode:

```text
cargo run --release --bin foster-bench
```

It reports:

- complete Foster front-end and checked-HIR compilation time;
- bytecode lowering time with and without optimization;
- instruction, register, constant, and function counts;
- VM execution time with and without optimization;
- execution time and result equivalence for the matching Lua program.

The harness looks for `lua`, `lua54`, then `luajit`. Use `--lua <path>` to choose an executable or
`--skip-lua` to omit the comparison. Lua is optional and is not required by normal builds or tests.
The Lua workload runs all timed iterations inside one process so process startup is paid once.

Available tuning flags are `--compile-iterations`, `--runtime-iterations`,
`--warmup-iterations`, `--lua`, and `--skip-lua`.

Criterion and the cross-language benchmark are diagnostic rather than pass/fail speed gates.
Result mismatches do fail the cross-language run.
Record results from a quiet machine, use release builds, and compare results from the same commit,
toolchains, hardware, and power settings.
