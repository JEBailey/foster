# LSP performance

Comparison of the Foster language server against the patterns used by rust-analyzer,
Pyright, and clangd, with speed recommendations grounded in the compiler phase
profiles in [benchmarking](benchmarking.md)
(`benchmarks/results/lsp-libraries.json`, commit `cc0d725`, 2026-09-10).

## Current baseline

Median latency on the Taker calculator and Unicode consumers (Ryzen 5 4500, Windows,
fresh LSP process per run):

| Operation | Source deps | `.flib` deps |
| --- | ---: | ---: |
| Initial diagnostics (`didOpen` → publish) | 2010 ms | 1925 ms |
| Body edit (`didChange` → publish) | 1249 ms | 1197 ms |
| Type-error diagnostics | 1578 ms | 1438 ms |
| Error repair | 1257 ms | 1198 ms |
| Cached hover (no check in flight) | 0.2 ms | 0.2 ms |

Phase breakdown, calculator in source mode:

| Phase | Initial check (1824 ms) | First body edit (1037 ms) |
| --- | ---: | ---: |
| `cache.prepare` (body-cache rebuild) | 172.9 ms | 175.6 ms |
| `types.initial` + `types.final` (typecheck passes) | 697.4 + 455.7 ms | 404.9 + 309.4 ms |
| `types.body_check` (bodies touched) | 7065 bodies / 493.9 ms | 3146 bodies / 255.6 ms |
| body cache: lookup + store overhead | 91.6 ms | 98.5 ms |
| effect worklist: functions re-derived | 8954 / — | 3756 / — |
| `ownership.total` | 142.7 ms (2×) | 78.2 ms |
| `hir.lower` | 40.6 ms (2×) | 21.7 ms |
| `package.load` | 50.5 ms | 21.0 ms (271.7 ms in `.flib` mode) |
| `pipeline.runs` | 2 (recovery loop) | 1 |

Counters for the first body edit: `body.hit` 2598 of ~6300 functions reused;
`effects.derived` 3756 (59% of all functions re-derived); `effects.summary_changed`
1629; `effects.caller_enqueued` 767.

## What successful servers do

**rust-analyzer** builds every analysis level as a Salsa query: per-file parse,
per-item lowering, per-function type checking. A revision increments on every edit;
queries whose inputs are unchanged are not re-run, and "durable" results that have
survived several revisions are reused without revalidation. Disk loading is
mtime-gated, and analysis parallelizes across files. An edit's cost is proportional
to the changed function and its dependents, not the crate.

**Pyright** is a lazy evaluator: interactive requests evaluate the type of the
requested identifier on demand instead of analyzing whole modules top to bottom.
Module source is cached and only changed modules are re-parsed; a separate
diagnostics pass runs on its own schedule so requests never queue behind it.
Pyright is 3–5× faster than mypy largely for these reasons.

**clangd** runs a dedicated AST worker per file. Updates are processed in the
background, stale versions are dropped, and the "preamble" (include AST) is built
once and reused; a keystroke re-parses only the main file. Requests are answered
from the latest stable AST while a newer one builds, with bounded CPU and memory.

The shared principles:

1. Memoize at fine granularity (file, item, function), keyed on content.
2. Never redo unchanged dependencies: mtime-gated disk reads, prebuilt metadata.
3. Make per-edit work proportional to the change via dirty-set propagation.
4. Keep interactive requests off the diagnostics critical path.
5. Drop stale work; debounce diagnostics.

## Where Foster stands

Already in place:

- Per-source parse cache (`ModuleCache`) — principle 1 at the parse level.
- Body-level incremental type checking (`BodyCache`): per-function reuse with
  dependency tracking, effect-summary reuse, and cached failures. This is a
  hand-rolled Salsa-like layer for function bodies; 2598 of ~6300 bodies are
  reused on a single edit.
- Stale-work detection via generation and document-version tags, cancellation
  probes, and a 150 ms diagnostic debounce — principles 5.
- Recovery stubs: one broken body does not abort the whole check.
- `.flib` compiled libraries — the prebuilt-dependency idea of principle 2,
  though only bodies are skipped today (see below).

Gaps, in cost order for a steady-state body edit:

| # | Gap | Cost today |
| --- | --- | --- |
| 1 | Second typecheck pass on every edit | ~309 ms |
| 2 | `BodyCache::prepare` whole-program rebuild | ~176 ms |
| 3 | Effect-worklist over-propagation | ~100–200 ms |
| 4 | Package re-walk, re-read, `.flib` re-decode per edit | 21–272 ms |
| 5 | Single worker thread: requests queue behind checks | up to ~1 s of latency while typing |
| 6 | Recovery loop re-runs the whole pipeline per error wave | ~900 ms on the cold check |
| 7 | HIR lowering, ownership, effect validation run whole-program every edit | ~100 ms |

## Recommendations

### 1. Eliminate the second typecheck pass

`infer_capture_modes` (`src/hir/ownership.rs`) returns `changed = true` whenever it
sees a `CaptureMode::Pending` capture. Every fresh HIR lowering produces all
captures as `Pending`, so the pipeline's `types.final` pass re-runs on **every**
keystroke — 309 ms of the 1037 ms edit cost — even when nothing changed.

The two-pass design exists because capture mode (Copy vs Move) depends on the
captured local's type, which type inference produces. Assign capture modes where
the local type is actually known — during closure checking in the typechecker —
and keep the second pipeline pass only as a rarely-firing safety net.

Expected: −300 ms per edit and per initial check.

### 2. Make `BodyCache::prepare` incremental and cheap

`prepare` (`src/typecheck/incremental.rs`) runs on every check and currently:

- re-lexes every function body in the package and stringifies the token kinds
  (`format!("{:?}")`) to build a whitespace-insensitive shape key;
- builds `declaration_key` by `Debug`-formatting and joining every declaration in
  the package into one large string;
- builds the dependency graph with two `O(expressions × functions)` name-matching
  scans (function-name and member-name comparisons against the full function list).

Fixes, all local to `incremental.rs`:

- Replace the tokenized shape key with a 128-bit hash of the raw body bytes
  (length + two FxHash values). `raw_source` is already retained. This trades
  whitespace-insensitive invalidation for a hash compare — a comment-only edit
  would re-check once and then hit; acceptable, and relaxable later.
- Replace `declaration_key`'s string join with a chained hash fingerprint.
- Index functions per module by name (`HashMap<String, Vec<FunctionId>>`) so the
  dependency scans become `O(expressions + functions)`.

Expected: 176 ms → under 15 ms, and tighter dependency edges feed recommendation 3.

### 3. Propagate effects along resolved call edges

For a single body edit the effect worklist re-derived 3756 of ~6300 functions
(59%) and changed 1629 summaries. The over-propagation comes from the
name-matched edges built in `prepare`: a call dirties *every* function with the
matching name in the module, including unrelated overloads and methods.

The HIR already resolves each call to a specific `FunctionId`
(`ResolvedName::Function(target)`). Use those resolved edges for effect
propagation; keep name-based edges only as the conservative layer the body cache
needs for signature-change invalidation.

Expected: −100–200 ms at this project size, and the gap stops growing with
project size.

### 4. Stop re-reading and re-walking the package every edit

- `.flib` mode: `library::read` re-reads, decodes, and validates the 3.2 MiB
  artifact on every package load — 271.7 ms of the 975 ms rebuild. Cache the
  validated `Arc<Library>` keyed by (path, mtime, size). This was already
  identified in the benchmark report; it is the single biggest `.flib` win.
- Source mode: the loader walks the tree and `read_to_string`s every `.fos` file
  on each rebuild, then full-string-compares against the parse cache. Gate on
  `stat` (mtime + size) and read only when the stat changed.
- `workspace/didChangeWatchedFiles` currently clears the *entire* compilation
  cache. Use it to invalidate only the affected packages.

Expected: −270 ms in `.flib` mode, −20 ms in source mode, and cold-start cost
decoupled from artifact size.

### 5. Serve interactive requests from a published snapshot

One worker thread serializes diagnostics and every request. Cached hover is
0.2 ms only when no check is in flight; while a ~1 s check runs, hover,
completion, definition, and symbols queue behind it. clangd answers from the
latest stable AST while a newer one builds; rust-analyzer parallelizes analysis
on a job pool.

The worker already computes a full `Compilation`; publish it as an
`Arc<Compilation>` snapshot (atomic swap) when it completes, and handle
interactive requests against the latest snapshot off the critical path — on the
main thread or a small request pool. Keep the existing generation/version
staleness rules: a request served from a stale snapshot is fine for navigation
and hover (that is what `last_good` already is).

Expected: interactive latency stays low during typing instead of spiking to the
full check time. This is the largest perceived-speed win.

### 6. Make the recovery loop cheaper

`check_recovering_cached` re-runs the full pipeline per error "wave" — and each
iteration `package.clone()`s the whole package (deep clone of every module AST
plus source strings) before lowering, typechecking, and ownership-checking it
again. The cold check ran the pipeline twice (~900 ms duplicated); a file with
several independent errors pays again per wave.

Options, in increasing cost:

- Recover in place with rollback instead of cloning the package.
- Cap interactive recovery: publish diagnostics from the first pass and stop.
  Strictness matters for `foster check`; the editor only needs to show the first
  batch of errors and stay responsive.

Expected: initial check 1824 ms → ~1000 ms; type-error edit 1578 ms → ~1250 ms.

### 7. (Structural) Module-level memoization for the remaining whole-program passes

Even after 1–4, each edit still re-lowers the whole package HIR (~22 ms),
re-runs ownership (~78 ms), and re-validates effects, regardless of what
changed. The rust-analyzer answer is to treat each module as a query —
lowering and ownership contribution memoized on (module content hash,
dependency contracts) so unchanged modules reuse prior results. This is what
makes per-edit cost proportional to the change at workspace scale.

Do this after 1–6: it is the largest architectural change and pays off mainly
as projects and dependency trees grow.

### 8. Micro items

- `byte_range_to_lsp` rescans the source per position (O(n) per diagnostic).
  Build one line index per source per publish and reuse it.
- The cancellation probe locks a `Mutex<HashSet>` on every call and is invoked
  per expression in `infer_expression`; an atomic generation flag is enough for
  the hot path.
- `publish_diagnostics` compiles per open document; same-package documents hit
  the cache, but N standalone scratch files are N full compilations. Consider
  publishing the edited document first, the rest after.
- `diagnostics.clone()` at publish time can be a move.
- Full text sync is fine at current file sizes; switch to incremental sync if
  large generated files appear.

## Projected impact

For the Taker calculator consumer (source mode), with recommendations 1–4:

| Operation | Today | Projected |
| --- | ---: | ---: |
| Body edit | 1249 ms | ~350–500 ms |
| Initial diagnostics | 2010 ms | ~900–1100 ms |
| Type-error diagnostics | 1578 ms | ~650–800 ms |
| Interactive request while typing | up to ~1 s queued | low single-digit ms (rec. 5) |

The `.flib` mode package-load figure drops by 270 ms immediately from
recommendation 4. Recommendation 7 is what keeps body-edit latency flat as
dependency trees grow instead of with them.
