# LSP cache and capture-mode optimization measurements

Measured September 18, 2026 on AMD Ryzen 5 4500 6-Core Processor, win32, Node v24.18.0.
The baseline is an isolated archive of commit `5462f5f`; the optimized build is that commit plus
the accompanying implementation. Both use the same current Taker consumers and the same harness.
Executable hashes, raw samples, and separate phase profiles are in [the results](lsp-cache-capture.json).

## End-to-end latency

Milliseconds, before → after. Baseline medians use 3 fresh-process runs;
optimized medians use 7. Each process contributes the median of three body edits.
Runs were sequential, with no compilation or test runs during measurement. Each variant had a
complete untimed warmup. Initial timings exclude protocol initialization and include an empty
in-process compiler cache; filesystem caches were warm. These are observed samples, not confidence intervals.

| Consumer | Dependencies | Initial diagnostics | Body edit | Edit reduction |
| --- | --- | ---: | ---: | ---: |
| calculator | source | 5,718 → 5,403 | 1,888 → 1,384 | 26.7% |
| calculator | flib | 5,399 → 5,395 | 1,544 → 1,130 | 26.8% |
| unicode | source | 5,797 → 5,387 | 1,863 → 1,351 | 27.5% |
| unicode | flib | 5,338 → 5,362 | 1,551 → 1,130 | 27.1% |

Warm edits improve by about 27% in all four variants. Initial source-mode diagnostics
improve modestly; initial compiled-library timings are effectively unchanged within
the observed variability. The baseline and optimized compilers produced byte-identical
Taker artifacts (SHA-256 `ef974e92fdbc83efe8b4ffe3b814b39871976814638c7b9dc497e256cc23c01b`).

The September 10 results used an older compiler and workload. They are not the before numbers
for this change, and their projected 350–500 ms edit latency is not a measured outcome here.
The current compiled Taker artifact is 3,558,315 bytes.

## First calculator body edit: phase profiles

Milliseconds from separate profiling runs. Inclusive phases overlap and must not be summed.

| Phase | Source dependencies | Compiled dependencies |
| --- | ---: | ---: |
| `cache.prepare` | 232.8 → 15.1 | 190.2 → 12.2 |
| `types.initial` | 601.3 → 612.0 | 411.9 → 414.3 |
| `types.final` | 402.7 → 0.0 | 0.0 → 0.0 |
| `package.load` | 28.7 → 29.5 | 405.6 → 139.9 |
| `ownership.total` | 620.7 → 531.4 | 293.3 → 296.0 |

The optimized compiler derives closure-construction effects from inferred local types and commits
capture modes after successful type checking. The second pipeline typecheck no longer runs in these samples.
Body guards hash raw source, retain exact equality for collision safety, and share guard text;
name indexes and a reverse dependency queue replace repeated scans. Declaration guards retain
arena-identity protection. Disk text remains separate from editor overlays, and validated artifacts
are reused until metadata or watched-file changes invalidate them. Cold artifact validation still runs.

## Validation

- 334 library unit tests passed across the final run and native-runtime rerun; one ignored.
  Eight native tests initially hit a denied user-cache directory and passed with
  `FOSTER_NATIVE_CACHE_DIR` set under the workspace's `target` directory.
- All 121 language/ownership integration tests passed.
- All 11 Foster function/effect tests and the agent-documentation example test passed.
- Three targeted watcher tests passed after updating the old full-reset test to scoped invalidation.
- The benchmark asserts valid diagnostics, the expected intentional type error, repair, and hover content.
- Rust formatting and diff whitespace checks passed.

## Reproduction

Build each compiler in release mode. Run the existing harness with `FOSTER_BENCH_EXE`
pointing to the desired binary, using the same consumer checkout for both builds:

```powershell
$env:FOSTER_BENCH_EXE = 'C:/path/to/foster.exe'
node benchmarks/lsp_libraries.cjs 7
```

Raw benchmark directories for this run:
- Baseline: `target/lsp-library-benchmark-1789768620124`
- Optimized: `target/lsp-library-benchmark-1789768034634`
