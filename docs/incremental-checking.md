# Interactive checking

The LSP keeps a checking session per package root. Strict `foster check` continues
to reject the original source and does not use interactive recovery.

## Independent errors

Type checking checkpoints inference state around input functions with explicit
parameter and return types. A failed body rolls back its constraints, allowing
other independent bodies to be checked in the same pass. Inferred signatures
remain conservative: a failure that could affect shared inference ends the pass.
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
