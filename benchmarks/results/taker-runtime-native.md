# Java, Foster VM, and native Foster Taker

Measured 2026-09-11 00:13:23 UTC on AMD Ryzen 5 4500 6-Core Processor (win32). Release compiler SHA-256: dd5604a364704bcca98fa4358d3d133fd0c22cb999e14fabfbc10532b295003b. This working-tree build includes the native callable return, assignment, generic specialization, and CodePoint arithmetic fixes.

All four native workloads compiled and ran successfully. Native Foster is 11.8–14.5 times faster than the Foster VM here. Java remains 647–1173 times faster than native Foster on these fixed-input microbenchmarks; these ratios are not full-application performance estimates.

| Workload | Java ns/parse | Foster VM ns/parse | Native ns/parse | VM / native |
| --- | ---: | ---: | ---: | ---: |
| literal16 | 23.0 | 335735.9 | 27001.0 | 12.4× |
| integer9 | 51.6 | 531955.5 | 44572.6 | 11.9× |
| scan256 | 576.4 | 5659487.5 | 391519.9 | 14.5× |
| trimmed_integer | 103.3 | 788388.3 | 66865.0 | 11.8× |

Three fresh processes per backend, five measured batches per workload per process, median of process medians. Batches calibrate to at least 100 ms; Java receives two seconds of JIT warmup per workload and runs with a 256 MB heap. Foster uses the default optimized bytecode and native builds. Timings exclude compilation, startup, parser construction, output, and checksum validation. Backends run sequentially with alternating order; all 180 batch checksums passed.

Inputs and workloads are unchanged from the [baseline report](taker-runtime.md): successful parsing of reused ASCII inputs, including complete-input validation. Java can inline and eliminate allocations. This custom harness is not JMH and does not cover changing inputs, backtracking failures, or large grammars.

The native literal process medians ranged from 26,743.7 to 37,498.7 ns/parse; the Java trimmed-integer medians ranged from 102.8 to 143.6 ns/parse. These observed ranges are not confidence intervals. All process medians and individual samples are in the [raw results](taker-runtime-native.json).

Reproduce with `node benchmarks/taker_runtime.cjs` using Java 21+, the release Foster compiler, and the sibling Taker repositories.
