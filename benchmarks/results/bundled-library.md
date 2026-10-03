# Bundled core and standard library measurements

Measured October 2, 2026 on AMD Ryzen 5 4500 6-Core Processor, Windows, Node v24.18.0. The baseline is commit `6b38368`; the after executable includes the bundled-library implementation in this checkout. Release executable hashes and unchanged fixture hashes are recorded in the raw results.

Precompiling the library reduced median checking time by about 70% for the three library-heavy fixtures, bytecode build time by 19–21%, initial LSP diagnostics by 52–59%, and LSP body edits by 75–79%. Cached hover remains below 1 ms. The executable is 14.8% larger. These are local workload measurements, not cross-machine performance guarantees.

## CLI compilation

Milliseconds; median (minimum–maximum) of seven fresh-process runs after one discarded warmup per command. Bytecode builds use the default optimizer.

| Command / fixture | Before | After | Median time reduction |
| --- | ---: | ---: | ---: |
| startup | 11.1 (10.3–15.5) | 11.5 (10.5–14.8) | -3.3% |
| check/fibonacci.fos | 23.5 (22.7–36.5) | 18.4 (17.0–22.0) | 21.6% |
| build/fibonacci.fos | 27.2 (24.5–48.3) | 20.8 (20.3–23.1) | 23.2% |
| check/hash_collections.fos | 1154.0 (1100.7–1605.9) | 336.9 (325.0–384.9) | 70.8% |
| build/hash_collections.fos | 4961.8 (4810.4–6630.4) | 3924.1 (3821.5–3997.5) | 20.9% |
| check/json.fos | 1186.5 (1104.8–1273.0) | 348.2 (327.6–402.4) | 70.7% |
| build/json.fos | 4984.8 (4850.9–6140.3) | 4061.4 (4015.2–4111.4) | 18.5% |
| check/unicode.fos | 1119.8 (1070.6–1585.7) | 331.2 (287.5–355.7) | 70.4% |
| build/unicode.fos | 4739.3 (4562.9–5560.3) | 3759.9 (3611.0–3910.3) | 20.7% |

## Editor handling

Milliseconds; seven fresh LSP processes per fixture after a discarded warmup. Initial diagnostics start at `didOpen`, after initialization. Each sample uses five cached hovers and three body edits; each run contributes their medians. Error and repair samples inject a Bool result into an Int function, then restore valid text. All diagnostics are checked against the requested document version. Edit times include the existing debounce.

| Fixture | Operation | Before | After | Median time reduction |
| --- | --- | ---: | ---: | ---: |
| fibonacci | Initial diagnostics | 371.7 (361.1–456.1) | 368.6 (355.8–389.2) | 0.8% |
| fibonacci | Cached hover | 0.134 (0.112–0.261) | 0.147 (0.128–0.357) | -9.7% |
| fibonacci | Body edit | 170.3 (161.5–171.8) | 156.6 (156.4–162.5) | 8.0% |
| fibonacci | Type error | 172.1 (157.3–174.8) | 156.7 (155.8–161.1) | 8.9% |
| fibonacci | Error repair | 169.7 (156.3–173.2) | 157.1 (155.1–169.7) | 7.5% |
| hash_collections | Initial diagnostics | 1542.5 (1518.0–1709.4) | 739.0 (681.2–763.2) | 52.1% |
| hash_collections | Cached hover | 0.257 (0.200–0.707) | 0.232 (0.119–0.499) | 9.8% |
| hash_collections | Body edit | 1254.7 (1212.6–1364.2) | 295.4 (290.0–307.5) | 76.5% |
| hash_collections | Type error | 1448.7 (1366.5–1548.3) | 346.6 (329.9–405.5) | 76.1% |
| hash_collections | Error repair | 1247.5 (1180.9–1351.1) | 293.7 (287.3–315.9) | 76.5% |
| json | Initial diagnostics | 1593.8 (1489.8–1791.0) | 759.2 (732.8–762.4) | 52.4% |
| json | Cached hover | 0.356 (0.191–0.536) | 0.211 (0.181–0.421) | 40.6% |
| json | Body edit | 1246.9 (1216.5–1403.3) | 305.6 (297.1–321.2) | 75.5% |
| json | Type error | 1439.5 (1396.0–1732.7) | 346.4 (334.4–360.2) | 75.9% |
| json | Error repair | 1233.8 (1187.3–1633.2) | 299.1 (292.9–334.0) | 75.8% |
| unicode | Initial diagnostics | 1604.3 (1494.5–1867.8) | 660.4 (643.7–691.3) | 58.8% |
| unicode | Cached hover | 0.364 (0.212–0.698) | 0.213 (0.138–0.428) | 41.6% |
| unicode | Body edit | 1246.8 (1225.1–1372.8) | 257.6 (249.5–263.0) | 79.3% |
| unicode | Type error | 1471.0 (1396.6–1519.6) | 296.0 (287.8–310.7) | 79.9% |
| unicode | Error repair | 1228.6 (1209.3–1403.9) | 256.4 (248.7–274.6) | 79.1% |

## Compiler phases and native preparation

Separate profiled runs use the hash-collections fixture. Each executable checks once per iteration, then compiles VM code and prepares/emits a native object with optimization disabled and enabled. The table shows medians of the final four of five iterations. These times include instrumentation, exclude source loading, native runtime linking, and Foster execution, and are separate from the CLI/LSP measurements. No builds or test suites ran concurrently.

| Phase | Optimization | Before (ms) | After (ms) | Median time reduction |
| --- | --- | ---: | ---: | ---: |
| frontend | — | 985.7 | 124.2 | 87.4% |
| vm | Off | 1863.2 | 1979.0 | -6.2% |
| native.prepare | Off | 1766.8 | 1815.6 | -2.8% |
| native.object | Off | 293.1 | 340.3 | -16.1% |
| vm | On | 3458.8 | 3567.0 | -3.1% |
| native.prepare | On | 2873.2 | 2993.6 | -4.2% |
| native.object | On | 358.3 | 393.2 | -9.7% |

The gain comes from avoiding frontend library analysis. Backend phases did not improve in
this sample and were slightly slower; native object emission increased by 10–16%. They
still process and optimize linked library code. Bundling does not remove final code generation.

The cold hash-collections LSP profile shows the work avoided, independently of elapsed time:

| Counter | Before | After |
| --- | ---: | ---: |
| parse.miss | 62 | 1 |
| body.checked | 2273 | 10 |
| body.empty_checked | 144 | 2478 |
| ownership.iterations | 4 | 2 |

`body.empty_checked` includes checked declaration stubs; it is not the number of original library bodies analyzed. The source-program body work remains, while ordinary library bodies are supplied as compiled code. Linking, specialization, lifetime lowering, and backend optimization still run.

## Size and build tradeoffs

The release executable grew from 31,773,184 to 36,480,512 bytes (+4,707,328, +14.8%). It retains embedded source text for diagnostics/navigation and adds minimal/full compiled bundles.

Rust toolchain builds now compile a build-host copy of the compiler and regenerate the bundles when compiler or library sources change. This increases developer build cost; this run did not establish a controlled clean-Cargo-build comparison. Ordinary Foster users need no writable disk cache or separately installed library sources. `.flib` format version is now 5; older independently compiled libraries must be rebuilt.

## Verification

Passed: 101 LSP tests, 126 backend parity cases (125 in the full suite plus the corrected numeric case rerun), 27 compiled-library tests, 26 CLI tests, 7 library-contract tests, 17 ownership tests, 2 bundled-library tests, the agent documentation test, and all 79 source-library tests. Rust formatting, numeric fixture formatting, and diff whitespace checks pass. Some baseline fixtures used obsolete bare imports or `float::Float`; they were corrected to the current contract. The baseline ownership-version assertion and public-type audit were also brought into agreement with the already implemented version/API.

## Reproduce and inspect

```powershell
node benchmarks/bundled_library.cjs target/bundled-library-baseline/foster.exe target/bundled-library-baseline/results.json 7
node benchmarks/bundled_library.cjs target/release/foster.exe target/bundled-library-after/results.json 7
```

The saved baseline executable is local under `target/bundled-library-baseline`; rebuilding commit `6b38368` reproduces the source-based compiler. Run comparisons without concurrent builds/test suites. Background desktop activity, filesystem caching, and scheduler noise remain uncontrolled. The harness uses Foster fixtures because the older Taker benchmark consumers have import-contract drift; these results make no Taker-specific performance claim. Peak memory and program execution speed were not measured.

Raw data: [before](bundled-library-before.json), [after](bundled-library-after.json), and [compiler/LSP profiles](bundled-library-profile.json). Benchmark harness: [bundled_library.cjs](../bundled_library.cjs).
