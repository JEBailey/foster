# Rust profiling examples

These development tools are registered as explicit Cargo examples in the root
manifest. They are separate from the reader-facing Foster programs in `examples/`.

```text
cargo run --release --example profile_compiler --features compiler-profile -- benchmarks/fibonacci.fos
cargo run --release --example measure_backends -- benchmarks/scalar_cse.fos target/cse.exe
```

Set `FOSTER_COMPILER_PROFILE=1` for phase reports from `profile_compiler`. Run
`measure_backends` alone on a quiet machine. See [benchmarking](../../documentation/benchmarking.md)
for the measured boundaries and [research](../../research/README.md) for saved findings.
