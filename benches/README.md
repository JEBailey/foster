# Criterion entry points

`compiler.rs` measures frontend and lowering cost. `runtime.rs` measures VM
execution of precompiled workloads. Cargo registers both as Criterion benches
with `harness = false`.

```text
cargo bench --bench compiler
cargo bench --bench runtime
```

Run from the repository root. Workload sources and other harnesses are indexed in
[benchmarks](../benchmarks/README.md). Criterion output stays in `target/criterion/`.
