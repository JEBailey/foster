# Larger native-from-bytecode comparison

Measured on 2026-10-03 on Windows x64, AMD Ryzen 5 4500. Both routes used the same
release compiler (SHA-256 `9d2e55a63ad48e34c44d667b2a40b0877c46f6130f3217f7dae86525acc75d81`).
The compiler was held fixed throughout measurement; reconstruction failures were not repaired
or replaced with the default route during the experiment.

## Recommendation

Continue investigating the optimization wins, but keep this route experimental. It improves
several control-flow workloads substantially, but cannot yet compile important library-backed
programs. This is evidence for pursuing the optimizations, not for replacing default native
compilation with bytecode reconstruction.

Of 31 cases, default native compilation passed all 31. The bytecode route passed 25 and failed
six during SSA reconstruction. Every executable that ran returned its independently expected
result. Iterator and snapshot workloads failed at both sizes; the larger formatter and parser
programs also failed.

## Runtime

The following ranges describe reductions in median runtime across the measured input sizes.
Positive reductions mean the bytecode route takes less time.

| Workload family | Cases passed | Median runtime reduction |
| --- | ---: | ---: |
| Branching counter | 2/2 | 17.6–25.6% |
| Enum dispatch | 2/2 | 15.0–15.8% |
| Recursive Fibonacci | 3/3 | 19.8–20.9% |
| Repeated scalar expressions | 2/2 | 13.5% |
| Fresh lists | 2/2 | 3.0–6.6% |
| Arithmetic loop | 2/2 | 0.5–2.3% |
| Record allocation | 2/2 | 1.5–2.6% |
| Record mutation | 2/2 | 0.7–1.6% |
| List growth | 2/2 | 0.2–1.3% |
| Field reads | 2/2 | 1.6–8.7% |
| Method reads | 2/2 | 0.7–4.2% |
| Floating-point loop | 2/2 | 0.6–1.9% |
| List iterator | 0/2 | Reconstruction failed |
| List snapshot during mutation | 0/2 | Reconstruction failed |
| Formatter, 1,000 functions | 0/1 | Reconstruction failed |
| Taker parser, 10,000 integers | 0/1 | Reconstruction failed |

The family-balanced geometric mean across the 12 fully supported microbenchmark families is
1.085x default/bytecode runtime, equivalent to **7.8% less time**. Failed cases and the two
larger program probes are excluded from that aggregate. It does not predict application-wide
performance.

Paired bootstrap intervals from the nine samples exclude parity at every tested size for
branching, enum dispatch, repeated expressions, and Fibonacci. Many small changes in the other
families overlap parity; the field-read improvement at the larger size does too. These intervals
describe sample variability on this machine, not variation across hardware or longer periods.

## Build time, size, and coverage

The family-balanced geometric mean of warmed build times is **1.1% higher** for the bytecode
route. List growth costs **11.3–25.8% more build time** while showing little runtime change.
Executable sizes are equal or slightly smaller with bytecode input; the largest observed
reduction among supported cases is about 0.7%.

The failed cases encounter opaque/scalar types or incompatible block argument types while
reconstructing library bodies. Recorded examples include `ByteBuffer.length`, `float_text?`,
`map_into`, and `ListMap.values`; the formatter fails in `drop_sign` and the parser in
`clock_time`. Which incompatible body is reported first can vary with traversal order.
The default route successfully executes the formatter in a median 1,543.9 ms and parser in
935.0 ms; there is no bytecode-route runtime comparison for those programs.

Before considering a default switch, recover correct types and ownership across library and
indirect calls, and repeat the comparison on real applications. Investigate which VM
optimization passes produce the control-flow gains and whether those gains can be obtained
directly in shared SSA without the reconstruction step.

## Method and reproduction

- 29 microbenchmark cases cover 14 families, generally at a base size and four times that size.
  Arithmetic reaches 20 million iterations; record and branch loops reach 8 million; list
  growth reaches 2 million. Fibonacci uses inputs 28, 32, and 35.
- Each successful variant has a warm build and checked warm execution, five alternating
  warmed build samples, and nine alternating checked runtime samples. Build samples use
  separate output files to avoid Windows executable locks.
- Runtime includes process startup and cleanup. Runtime caches are warm. No other benchmark
  suite was run concurrently. The suite does not measure peak memory, cold-cache builds,
  LSP latency, or cross-platform behavior.
- The two larger program probes have one build per route and nine runtime samples when
  supported. Their single build times are descriptive and excluded from compilation aggregates.
- New benchmark fixtures were source-checked, format-checked, and verified against VM results;
  the documentation examples also passed. The recorded source hashes and expected checksums
  match the reproducible suite.

The experimental bytecode-to-native route and its build harnesses have been removed.
These are archived measurements; current native performance is covered by the
[default native cleanup comparison](native_cleanup.md). Analyze the saved samples with:

```powershell
node benchmarks/analyze_native_from_bytecode.cjs benchmarks/results/native_from_bytecode_large.json
```

The program probe requires the sibling `taker_foster` checkout. It records its source-tree hash.
See [raw microbenchmark samples](native_from_bytecode_large.json),
[bootstrap and family analysis](native_from_bytecode_large.analysis.json), and
[larger program results](native_from_bytecode_programs.json).
