# LSP snapshot request measurements

Measured September 18, 2026 on AMD Ryzen 5 4500, Windows, using release builds.
The comparison build already contains the shape-key, library-cache, and capture-mode
changes (#2, #4, #1). The new build additionally separates interactive requests from
checking and publishes immutable semantic snapshots (#5).

The calculator consumer uses the same source and compiled Taker dependencies as the
cache/capture benchmark. Each variant uses one fresh LSP process, waits for initial
diagnostics, then performs three body edits. After each edit, the harness waits
250 ms (past the 150 ms debounce) and sends five rounds of hover, completion, and
definition requests in a separate unchanged function. It validates the returned
semantic data and waits for diagnostics for the matching version. Runs were
sequential, with no concurrent builds or tests. These are small observed samples,
not confidence intervals or a guarantee for every project.

| Calculator dependency | First hover median before → after | All request median after | Maximum request after | Requests completed before diagnostics after |
| --- | ---: | ---: | ---: | ---: |
| Source | 1146.5 → 33.8 ms | 0.71 ms | 37.0 ms | 45 / 45 |
| Compiled `.flib` | 1052.7 → 26.7 ms | 0.45 ms | 28.3 ms | 45 / 45 |

The first request exposes waiting behind checking in the comparison build; later
requests there mostly run after the check. All requests in the new build complete
while diagnostics are still pending. Diagnostic medians in the new build were
1484.0 ms with source dependencies and 1053.6 ms with compiled dependencies. This
change separates request latency from diagnostic latency; it does not promise to
make the check itself faster.

Raw samples, executable hashes, and timestamps:

- [Source, before](lsp-snapshots-before-source.json)
- [Source, after](lsp-snapshots-source.json)
- [Compiled dependency, before](lsp-snapshots-before-flib.json)
- [Compiled dependency, after](lsp-snapshots-flib.json)

Reproduce using `node benchmarks/lsp_snapshots.cjs <foster.exe> <document.fos> <results.json>`.
The harness only changes editor overlays. It discovers the nearest `foster.toml`.

Correctness validation: all 77 LSP Rust tests pass. A deterministic test holds the
checking worker while requesting semantic features, including shifted unchanged
functions, malformed edits, keyword fallback, fresh-only rename, filesystem
invalidation, and close/reopen. Existing cancellation, recovery, cache eviction,
dependency, and source-range tests also pass.

On a cold open, semantic requests can return no result until the first publication.
Inside an edited function, keyword and applicable syntax-based auto-import completion
remain available; typed results wait for a new snapshot. Unchanged functions use
the existing guarded source mapping.
