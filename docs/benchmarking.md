# Optimization testing and benchmarks

Optimization is enabled by default. It can be selected explicitly from the CLI:

```text
foster run program.fos --optimize
foster run program.fos --no-optimize
```

Library users select the same behavior with `vm::CompileOptions { optimize }` and
`vm::compile_with_options` or `vm::run_with_options`.

## Correctness gates

The VM test suite seals a representative language corpus through shared SSA, compiles it to
bytecode both ways, verifies both programs, executes them, and requires identical results. A
separate structural test requires the optimizer to reduce instruction and register counts for a
representative program. These are stable CI gates; elapsed-time assertions are deliberately
excluded because scheduler load and machine differences make them unreliable.

## Language-server latency

Measure cold requests, repeated requests without edits, and requests after an open-buffer change
separately. Use a release compiler for interactive latency measurements; a debug compiler measures
unoptimized compiler work as well as the language-server behavior. Include a document with errors:
semantic recovery currently restarts checking after replacing each failed body with a stub.

The type checker reuses the converged inference pass for final effect and composition validation.
Within each pass, it caches expanded record fields and methods only when both generic arguments
and resulting contracts contain no inference variables. Caches are discarded between passes so
changed effect contracts and overload substitutions cannot reuse stale structural information.
Inference substitutions use shared pages: overload and structural-matching backtracking snapshots
copy page references, and a new binding detaches only its affected page.
These optimizations preserve full semantic checking; they do not make package checking incremental.

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
for Foster-defined `String`, `Symbol`, `Bytes`, `ByteBuffer`, and `List` values. Each workload
reports optimized and unoptimized VM execution separately.

The `vm/list_fold` pair compares the indexed Foster implementation against the former recursive
shrinking-tail strategy on the same 2,048-element input. Both measurements include identical input
construction. Run just that comparison with `cargo bench --bench runtime -- list_fold`; it is a
diagnostic comparison, not a timing-based correctness gate.

A local Windows release run on 2026-09-04 measured 67.1 ms for the recursive-tail reference and
2.83 ms for the indexed fold with optimized bytecode (about 24x faster). This compares both
strategies in the same build, not two historical releases; timings are machine-dependent.

## Foster library algorithms

Concrete list algorithms use indexed loops and one output list. `List.at` checks bounds and Copy
capability, returning a `Result` containing an independent element copy or a typed read error.
`List.slice` and `Bytes.slice` copy only their
selected half-open ranges once; these APIs are value copies, not zero-copy slice views.

String algorithms scan one UTF-8 byte snapshot. Trimming and grapheme/scalar slicing select byte
boundaries and copy the result once. `StringBuilder` encodes Unicode scalars and accumulates text
over the Foster `ByteBuffer`; UTF-8 validation, casing, splitting, joining, and encoding remain
Foster algorithms. Grapheme segmentation uses pinned Unicode tables in Foster; counting and
slicing scan forward, and reversal materializes the clusters before appending them in reverse.
ASCII casing preserves non-ASCII bytes. Substring search is a byte-range scan
without suffix allocations, but remains O(n*m) in the worst case.

`ByteBuffer.push` and `extend` update list storage directly. Native copy-on-write reuses uniquely
owned storage and detaches shared values; buffer growth uses checked geometric capacities.
Allocation, storage access, and platform operations remain low-level primitives. No text or
collection algorithm has been moved into a Rust runtime helper.

Iterator consumers, filtering, and skipping use loops and preserve short-circuit consumption.
The generic `SequenceIterator` adapter resolves erased sequence accessors in both backends;
parity tests cover built-in and user-defined sources, lazy pipelines, and exhaustion.
Concrete list and byte `.iterator()` calls use an index; strings reuse the library's UTF-8 byte
cursor. Advancing these cursors does not allocate suffix collections. Snapshot creation may still
copy backing data, and the snapshot remains owned until the cursor is released. General `Sequence`
head/rest adapters still inherit their source's tail-copy costs, including explicit
`Iterator.from_sequence` calls. The runtime benchmark group `vm/iterator` compares concrete cursors
with that adapter at 2,048 and 4,096 elements for lists and Unicode strings, using identical input
construction. Run `cargo bench --bench runtime -- vm/iterator` to reproduce the comparison.

A local Windows release run on 2026-09-06 measured these optimized VM times (including input
construction; these are measurements, not CI thresholds or native-backend timings):

| Input | Elements | Head/rest adapter | Collection cursor |
| --- | ---: | ---: | ---: |
| List | 2,048 | 28.2 ms | 7.25 ms |
| List | 4,096 | 91.0 ms | 14.2 ms |
| Unicode string | 2,048 | 22.4 ms | 15.5 ms |
| Unicode string | 4,096 | 68.1 ms | 31.0 ms |

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

## Taker runtime comparison

With Java 21+, a Windows release Foster compiler, and sibling `taker/taker` and
`taker_foster` repositories, run `node benchmarks/taker_runtime.cjs`. It compiles both
libraries from current source, uses matching parser workloads with checked results, and
reports in-process parse latency across three independent processes per backend. Java
gets an explicit JIT warmup; Foster runs prebuilt optimized bytecode. Native compilation
is attempted and failures are recorded separately. See the [measured results and scope](../benchmarks/results/taker-runtime.md).

## Language server latency

```text
python benchmarks/lsp_latency.py target/release/foster.exe path/to/module.fos
python benchmarks/lsp_latency.py target/release/foster.exe path/to/module.fos --outline
python benchmarks/lsp_latency.py target/release/foster.exe path/to/module.fos --interrupt
python benchmarks/lsp_latency.py target/release/foster.exe path/to/module.fos --profile-log target/lsp-profile.log
```

Use a release `foster lsp` process and the real project root. Measure cold semantic
requests, repeated requests, and requests after a body edit separately. Also measure
diagnostic publication and cancellation during active checking. Document symbols now
use parsing only, so outline latency is not a proxy for full semantic-check latency.
For edits, cover unchanged contracts, changed effects/signatures, and error repair.
Compare diagnostics and current source ranges against a fresh checked compilation.
The implementation boundaries are documented in [interactive checking](incremental-checking.md).

### Source versus compiled library dependencies

With `taker_foster` beside this repository and a current Windows release compiler, run:

```text
node benchmarks/lsp_libraries.cjs 7
```

This builds Taker into a `.flib` and compares its calculator and Unicode consumers using
source dependencies and that artifact. It creates isolated consumer projects under
`target/lsp-library-benchmark-*`; the original Taker projects are not edited. Each sample
uses a fresh LSP process, with dependency order alternating between repetitions. A complete
warmup for each variant precedes measurement, so initial analysis has an empty LSP cache
but does not represent a cold operating-system filesystem cache.

The harness measures opening a document to its initial diagnostics, cached semantic hover,
three body edits, an intentional type error, and error repair. Edit timings end at diagnostics
for the matching document version, and diagnostic/hover assertions guard against fast failures.
Process initialization and the one-time library build are excluded from editor latency.
Raw samples, per-process medians, ranges, library size/build time, and machine information
are saved in `results.json`. Separate profiling runs capture frontend phases without adding
profiling overhead to the timed samples.
