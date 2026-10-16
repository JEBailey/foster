# Performance workloads and harnesses

| Location | Purpose |
| --- | --- |
| [Criterion entry points](../benches/README.md) | Sampled compiler and VM microbenchmarks |
| `fibonacci.fos`, `fibonacci.lua`, `scalar_cse.fos` | Small comparison workloads |
| `native_runtime/` | Native allocation, dispatch, storage, and polling workloads |
| `taker/` | Matching Foster/Java parser consumers; requires sibling Taker repositories |
| `harnesses/` | Repeatable CLI, LSP, C bridge, formatter, and application measurements |
| [Rust profiling tools](../tools/profiling/README.md) | Compiler-phase and VM/native comparison examples |
| [Research](../research/README.md) | Diagnostic experiments and saved measurements |

Run harnesses from the repository root. For example:

```text
cargo bench --bench runtime
node benchmarks/harnesses/bundled_library.cjs target/release/foster.exe target/research/bundled-library/results.json 7
python benchmarks/harnesses/lsp_latency.py target/release/foster.exe path/to/main.fos
python benchmarks/harnesses/c_argument_packets.py --help
```

Read each harness's platform/tool requirements before running it. See
[benchmarking](../documentation/benchmarking.md) for timing boundaries, warmups,
checksums, profiling, and interpretation. CI syntax-checks harnesses and compiles
Rust benchmark entry points when needed; it does not run timing comparisons.

Keep fresh logs, executables, captured objects, and bulk samples under ignored
`target/research/<investigation>/`. Curate findings and supporting data into
`research/results/` once they are useful to retain. Diagnostic modifications,
such as bypassing cancellation in a copied binary, belong under
`research/experiments/` and must remain separate from normal workloads.
