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

Concrete list algorithms use indexed loops and one output list. `List.at` is the low-level checked
storage read that returns an owned element without moving it out of the source; it lowers to the
existing indexing instruction in both backends. `List.slice` and `Bytes.slice` copy only their
selected half-open ranges once; these APIs are value copies, not zero-copy slice views.

String algorithms scan one UTF-8 byte snapshot. Trimming and code-point slicing select byte
boundaries and copy the result once. `StringBuilder` encodes Unicode scalars and accumulates text
over the Foster `ByteBuffer`; UTF-8 validation, casing, splitting, joining, and encoding remain
Foster algorithms. ASCII casing preserves non-ASCII bytes. Substring search is a byte-range scan
without suffix allocations, but remains O(n*m) in the worst case.

`ByteBuffer.push` and `extend` update list storage directly. Native copy-on-write reuses uniquely
owned storage and detaches shared values; buffer growth uses checked geometric capacities.
Allocation, storage access, and platform operations remain low-level primitives. No text or
collection algorithm has been moved into a Rust runtime helper.

Iterator consumers, filtering, and skipping use loops and preserve short-circuit consumption.
Their behavior is covered by the Foster VM suite; native lowering of the generic
`SequenceIterator` adapter still cannot resolve its erased sequence's storage members.
General `Sequence` head/rest adapters still inherit their source's tail-copy costs; they are not
zero-copy indexed cursors. The concrete List overload of `String.from_code_points` avoids that
cost. More specialized collections and parsers remain candidates for later algorithm work.

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
