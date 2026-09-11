# LSP source versus compiled libraries

Measured 2026-09-10T23:16:38.707Z on AMD Ryzen 5 4500 6-Core Processor              , win32, Node v24.18.0. Compiler commit: cc0d7256711c632bdb45c9ddb3fd58b8e75d0fb0. Release executable SHA-256: b02cdb712512b0f5cb3cd97bb8abf5ce66d9b479da9fb08bb80dfb8beb805b87.

Compiled Taker dependencies improved median body-edit latency by about 4% in both real consumers. Initial diagnostics improved by 4.2% for the calculator and 1.4% for Unicode; initial timing ranges overlap, particularly for Unicode. Cached hover was about 0.2 ms in both modes. These results do not demonstrate a large editor speedup from packaging alone.

## Measurements

Milliseconds; median of seven fresh-process runs per variant. Parentheses show the minimum and maximum across those runs. For cached hover and body edits, each run contributes its median of five requests and three edits, respectively.

| Consumer | Operation | Source dependency | `.flib` dependency | Median latency reduction |
| --- | --- | ---: | ---: | ---: |
| calculator | Initial diagnostics | 2010.2 (1956.7–2088.9) | 1925.4 (1900.6–1990.2) | 4.2% |
| calculator | Cached hover | 0.217 (0.182–0.704) | 0.192 (0.176–0.253) | 11.6% |
| calculator | Body edit | 1248.7 (1211.9–1294.8) | 1196.9 (1152.1–1228.8) | 4.1% |
| calculator | Type-error diagnostics | 1577.8 (1499.7–1727.1) | 1437.9 (1358.6–1503.1) | 8.9% |
| calculator | Error repair | 1256.7 (1197.5–1334.0) | 1198.0 (1156.5–1304.1) | 4.7% |
| unicode | Initial diagnostics | 1961.5 (1949.7–2031.2) | 1934.5 (1895.8–2032.5) | 1.4% |
| unicode | Cached hover | 0.224 (0.181–0.317) | 0.226 (0.173–0.417) | -0.8% |
| unicode | Body edit | 1231.9 (1211.6–1261.0) | 1180.8 (1152.2–1236.6) | 4.1% |
| unicode | Type-error diagnostics | 1542.7 (1492.3–1582.4) | 1391.8 (1366.1–1528.2) | 9.8% |
| unicode | Error repair | 1213.2 (1183.7–1329.9) | 1156.5 (1141.1–1230.9) | 4.7% |

## Method

- Same Taker calculator and Unicode consumer source; only the dependency manifest changes. A small independent function is appended for repeatable body edits and hover requests.
- One full warmup per variant, then seven repetitions with source/artifact order alternating. Processes run sequentially. Operating-system filesystem caches are warm; each measured LSP process starts with empty in-process caches.
- Initial latency runs from `didOpen` to diagnostics. Edits run from `didChange` to diagnostics carrying the matching version. All valid stages assert no consumer errors, the error stage asserts a Bool/Int error, and hover asserts the expected declaration.
- Process initialization is excluded. Five cached hovers, three valid body edits, one type error, and one repair run in each process.
- Profiling is disabled for timed samples. One additional profiling process per variant supplies the phase breakdown below. Inclusive phases overlap and must not be summed.
- Measurements reflect this Windows machine and these consumers; ranges are observed variability, not confidence intervals.

One library build took 2.182 seconds. The artifact is 3,246,392 bytes (3.10 MiB). That one-time build is excluded from editor timings.

## Why the gain is modest

Separate calculator profiling, first body edit:

| Phase | Source | `.flib` |
| --- | ---: | ---: |
| Package loading | 21.0 ms | 271.7 ms |
| Function body checking | 255.6 ms | 156.2 ms |

The package loader calls `library::read` each time it rebuilds a compiled dependency. That reads, decodes, and validates the artifact again, while source parses already have an in-process cache. The measured loading penalty offsets some of the benefit from skipping library bodies. Caching validated library artifacts with correct invalidation is the next concrete optimization suggested by these measurements. This benchmark does not measure the performance of that proposed change.

## Reproduction

Run `node benchmarks/lsp_libraries.cjs 7` with a current Windows release compiler and `taker_foster` beside this repository. See [the harness](../lsp_libraries.cjs) and [raw samples and profiling data](lsp-libraries.json).
