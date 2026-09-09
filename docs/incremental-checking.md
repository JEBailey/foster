# Interactive checking

The LSP keeps a checking session per package root. Strict `foster check` continues
to reject the original source and does not use interactive recovery.

## Independent errors

Type checking starts a transaction around input functions with explicit
parameter and return types. A failed body rolls back its constraints, allowing
other independent bodies to be checked in the same pass. Inferred signatures
remain conservative: a failure that could affect shared inference ends the pass.
Transactions journal map writes (including in-place signature changes) and new
set members instead of cloning accumulated checker tables. Successful bodies discard
their undo records; failed bodies restore overwritten entries, remove additions,
and truncate appended constraints, dispatch keys, and diagnostics. Substitution pages
retain their existing copy-on-write snapshots, and allocation counters are restored.
Cancellation discards the unfinished checker; partial results are not published.
Move, region, and remote validation collect independent function failures too.
The recovery driver replaces each failed body in the batch with a recovery stub,
then resumes checking. Original diagnostics remain attached to the input source.

## Body reuse

A function is identified by module, qualified name, and overload ordinal. A
declaration fingerprint guards nominal and function arena identities; declaration,
import, and overload changes conservatively clear the session. Function source
tokens, concrete input types, and observed callee contracts guard body results.
Results store expression/local positions within the function, and replay them into
the new arenas. Moving a function therefore does not reuse obsolete source offsets.
Dispatch slots are interned again in the current checker.

Unchanged callers can reuse type checking when a callee's implementation changes
but its callable contract does not. Type, effect, and suspension changes invalidate
consumers. Potential overload and method candidates are dependencies too. Effect
inference still runs to a fixed point. Changed bodies and their reverse dependency
components start without old inferred effects, so removing an effect cannot leave
a recursive component stuck at its previous summary.

Effect derivation resolves outer-local ownership groups only when the body references
them, preserving parameter and local-binding overrides. It does not scan the package's
local-variable table for every function. Each checker pass retains derived effects and
suspension for both inferred and explicitly annotated functions. Only inferred contracts
participate in fixed-point updates; once they converge, explicit bounds and advisory
warnings are checked against those same summaries without walking the bodies again.
Direct-call and remote-call effect dependencies are settled by a work queue inside each
checker pass. A changed inferred summary schedules its observed callers; explicit callees
continue to expose their declared bounds. Dirty functions begin with empty working rows,
so recursive cycles cannot sustain removed effects from stale working summaries. The
existing conservative reverse-dependency invalidation resets affected HIR contracts on edits.

Converged summaries and their observed dependencies are stored with eligible body results.
They are reused only when the body cache's source, type, and callee-contract guards pass.
A recomputed callee whose published contract stays the same leaves cached callers alone.
Changed or removed effects notify them. A cache miss falls back to derivation. Higher-order
callable types and capture modes still require the enclosing type/effect convergence loop;
the work queue does not replace typing with effect-only analysis.

Eligible body failures are cached separately, with function-relative labels and
an exact source-text key. Moving an unchanged failure remaps its labels; editing
its contents rechecks it. Failed constraints are never replayed into inference.

Reuse is deliberately limited to concrete, isolated body results. Unresolved
inference, closures/captures, deferred member constraints, and structural method
adaptation use ordinary checking. Recovery stubs are not successful body entries.
Ownership lowering and provenance fixed points are still rebuilt, and ownership
validation still runs across the package. This is incremental type checking,
not a fully incremental frontend. Internal session counters distinguish checked
and reused bodies and count recovery pipeline runs.

## Scheduling and cancellation

The worker preserves edit/request order and postpones queued background diagnostics
until interactive work has run. A new request interrupts active diagnostics; they
are rescheduled after the request if the document generation is still current.
Edits interrupt obsolete requests and diagnostics. Explicit request cancellation
also stops active frontend work, and shutdown interrupts the active generation.

A scoped probe checks cancellation at pipeline, function, expression, and fixed-point
boundaries. Cancellation is not a source diagnostic and is never stored as a failed
compilation. The worker checks request identity and document generation again before
responding; diagnostic publication checks generation and cancellation before sending.
Completed body entries can be reused after cancellation because each entry is checked
against its inputs on reuse. Cancellation is cooperative: a single parser/lowering
operation or ownership dataflow operation can still delay the next checkpoint.

Document symbols use the current document's cached recovering parse without invoking
semantic checking. If lexical damage prevents parsing, the existing guarded semantic
snapshot fallback remains available. Hover and other typed features request semantic
analysis on demand.

## Profiling

Set `FOSTER_LSP_PROFILE=1` in the server's environment to emit one
`FOSTER_LSP_PROFILE {json}` line on stderr for each `compile_for` or standalone
document parse. Profiling is disabled by default. It does not write to LSP stdout
or alter checking behavior. Reports include the document URI/path, but no source
text. A successful recovering compilation has outcome `ok` even if it contains
source diagnostics; other outcomes are `error` and `cancelled`.

Each report has schema version 1 and request-local `analysis` fields:

- `total_ms`: wall time for that operation, excluding queue wait and JSON emission.
- `phases`: call count and `inclusive_ms` for parsing, HIR lowering, cache setup,
  initial/final type checking, declaration setup, body checks, transaction setup,
  cache lookup/store, effect inference, capture analysis, and ownership phases.
  Nested timings overlap: do not add a parent phase to its child phases.
  Repeated calls accumulate across inference iterations and recovery retries.
  `types.checkpoint` measures transaction setup, `types.commit` and `types.rollback`
  measure its completion, and body-check time includes recording mutations.
- `counters`: compilation and parse cache hits/misses, body hits/error hits/checks,
  body miss reasons, store exclusions, declaration cache resets, and pipeline,
  type-inference, and ownership iterations. Missing counters mean zero.
  `body.transaction_commit` and `body.transaction_rollback` count completed transactions.
  Effect propagation adds `effects.derived`, `effects.reused`, `effects.summary_changed`,
  and `effects.caller_enqueued`. `effects.reused` counts bodies not walked in that pass;
  a body revisited by propagation counts again in `effects.derived`.
- `body_hit_rate`: `(body.hit + body.error_hit) / (hits + body.checked)`, or null
  when no nonempty body was attempted. This is reuse across all checking passes,
  including reuse within the same request; it is not a percentage of unique functions.
  Empty bodies, intrinsics, and recovery stubs are counted separately as
  `body.empty_checked`. Cancelled attempts can count as checked without completing.

Use `benchmarks/lsp_latency.py --profile-log <file>` to enable profiling for a
benchmark subprocess and retain the reports. Cancellation reports retain completed
phases and elapsed time in the interrupted phase. Profiling adds measurement overhead;
compare performance with profiling disabled when making final latency claims.
