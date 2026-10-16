# Investigations and measurement evidence

`experiments/` contains diagnostic tools tied to particular investigations, such
as native route inspection and cancellation ablation. `results/` contains saved
reports, raw samples, profiles, and interpretation limits. Routine harnesses and
workload sources live under [benchmarks](../benchmarks/README.md).

These reports describe the recorded revision and machine. Their paths, compiler
hashes, and measurements are evidence from that run; they do not establish current
performance or the language contract. Use [documentation](../documentation/README.md)
and tested source for current behavior. Some older experiments require retained
objects, a matching compiler revision, Windows-specific tools, or sibling projects.

The standalone [Foster lexer prototype](experiments/lexer/README.md) also lives
here. Its package tests cover scanning independently of the production Rust lexer.

| Investigation | Evidence |
| --- | --- |
| Failure-state handles, selective caching, and actor follow-up | [Reports and raw samples](results/failure-cache/README.md) |
| Native storage/cleanup | [Cleanup comparison](results/native_cleanup.md) |
| Native runtime and polling | [Runtime profile](results/native-runtime-profile.md), [status reuse](results/native-poll-status-reuse.md) |
| Native compilation routes | [Route analysis](results/native_route_analysis.md) |
| Compiler/optimizer cost | [Compiler measurements](results/compiler-optimizer.md) |
| Library algorithms | [Collection measurements](results/library-algorithms.md) |
| Embedded library | [Bundled-library comparison](results/bundled-library.md) |
| LSP | [Snapshot comparison](results/lsp-snapshots.md), [library comparison](results/lsp-libraries.md), [earlier investigation](results/lsp-performance.md) |
| Application parsing | [Taker comparison](results/taker-runtime.md) |

Start new local investigations under ignored `target/research/<name>/`. Build all
variants before measuring and avoid running builds or correctness tests during
timing. Retained reports should identify the question, revisions/hashes, workload,
hardware/OS/toolchain, timing boundary, warmups/repeats, correctness checks,
results, and limitations. Keep a report beside its supporting JSON; avoid committing
executables, object files, caches, dependency trees, or bulk profiler captures.
