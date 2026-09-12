# Ownership analysis precision

## Problem

The checker sometimes rejects safe programs because it cannot prove that a result is independent
of an input loan, or that an invalidation and a later use occur on mutually exclusive paths.
The analysis preserves the loan guarantees S-13 and S-14 in
[the semantic specification](semantics.md#6-loans-groups-and-invalidation).

A result's provenance is the set of storage origins it may still depend on. For example, a
returned reference depends on its source; a newly produced independent string need not depend
on the list element used to compute it. Keeping an unnecessary dependency can prevent reshaping
that list while the string is still used.

## Analysis model

- Direct calls use inferred receiver/parameter result summaries, propagated to a fixed point
  through direct call chains. Declared result groups must cover the actual returned origins.
- Indirect calls use known callable summaries or a typed fallback as described below. Unknown
  relationships retain conservative dependencies.
- Stable boolean places, enum patterns, and direct scalar comparisons retain branch facts.
  Equality/inequality normalization and some dynamic-index disjointness are supported.
- Boolean combinations and saved local Boolean conditions retain the bounded facts described below.
  Comparisons of computed values remain conservative. The loan checker widens to shared facts
  after sixteen alternatives; more reasoning must remain bounded.

See [ownership analysis](ownership.md#current-limitations-and-evolution) and
[verification](ownership-verification.md#place-reasoning). The principal implementation entry
points are `call_result_borrow_value` in `src/ownership/lower.rs`, result-summary inference in
`src/ownership/regions.rs`, and the ownership MIR's comparison/place representation.

## 1. Precise results through indirect callables

Passing a function through the supported variable, parameter, record, or callable-adapter forms
preserves provable result dependencies. An independent-string producer permits later list mutation;
a callable returning a reference into that list prevents invalidating mutation before the
reference's last use.

Supported forms:

1. Ownership MIR carries inferred result-parameter dependencies separately from a callable's
   captured loans. Bindings, moves, fixed fields/indices, and control-flow joins preserve this
   information. Joins union dependencies only when every predecessor knows the callable;
   a missing entry means unknown. Direct recursive summary inference also feeds callable targets.
2. Unknown/erased callables use their checked types: a recursively borrower-free result is
   independent of input loans, and explicit reference-result groups exclude unrelated reference
   parameter groups. This applies through callable parameters, returned callable values, adapters,
   and aggregate storage. Functions, opaque types, structural surfaces, unresolved generics, and
   recursion beyond the bounded type walk cannot establish borrower-free results.
3. Borrow-containing results retain the callable environment and all possibly relevant input
   origins. Callable adaptation checks result-origin relationships before call-site group
   substitution can collapse distinct groups to one frame group. Calls with mutation/consumption
   effects, reference captures, and writes through references forget potentially stale target
   knowledge. Native storage and return boundaries wrap concrete closures in the erased
   callable representation where required.

This metadata lives on ownership MIR values and CFG states, not in a new source annotation or
runtime lifetime mechanism. There is no new callable syntax or serialized callable-summary ABI.
Separately compiled or otherwise unknown targets use the checked contract fallback.

Constant-index selection from fixed local lists preserves the selected callable's
reference-parameter summary. Both `callbacks[0](...)` and assigning `callbacks[0]` to a local
retain the selected slot's dependencies, rather than combining all list elements. Replacing a
slot invalidates its previous summary. Tests distinguish two functions returning references to
different inputs, reject invalidation of the selected input, and verify VM/native results with
optimization enabled and disabled. An index supplied at runtime remains conservative.

Dynamic-index target selection, target identity returned through
opaque factories, capture-specific result summaries, and hidden borrowers in aggregate parameters
remain conservative. Known targets can still retain unnecessary environment dependencies for
borrow-containing results. Effectful calls and reference captures may discard more target knowledge
than necessary. Pending remote-request relationships are tracked separately and are not discharged by these proofs.

## 2. Bounded reasoning for compound conditions

The analysis recognizes mutually exclusive paths expressed using supported boolean combinations.
For example, reshape under `a && b` and use an earlier loan only under `not a || not b` are
mutually exclusive while both operands remain unchanged. Reshape and use under the same
condition remain a conflict.

The analysis supports `&&`, `||`, and `not` over existing supported facts. Preserve short-circuit evaluation
and any effects of evaluated operands. Add computed-value comparisons only in a later bounded
step with explicit identity, purity, and invalidation rules; do not assume repeated function calls
return the same value or apply arithmetic identities that ignore overflow behavior.

Implementation: boolean conditions are lowered to atomic ownership-CFG edges in evaluation order,
including boolean branch subjects and guarded returns, breaks, and continues. Integer comparisons
normalize complementary orders; floating-point ordering does not use total-order identities.
Assignments forget overlapping facts and facts about possible alias origins, including parent
reborrows. Calls forget facts for their declared mutation targets, including every argument in a shared group; indirect calls conservatively
forget all predicate facts. The forward and backward analyses use the same invalidation rules,
including across loop backedges. Each join retains at most 16 alternatives before widening to
common facts. Computed predicates (including dynamically indexed predicate places), repeated calls, and general arithmetic equivalence remain
conservative.

Saving a pure Boolean condition in a local preserves its relationship to the input locals.
For example, after `let can_update = ready && allowed`, a branch on `can_update` can be correlated
with `not ready || not allowed`. This supports Boolean local reads, constants, `not`, and pure
Boolean selections including `&&`/`||`, bounded to 64 HIR expression nodes per initializer.
Assignments and local aliases use the same tracking. Calls, projected reads, comparisons, and
larger expressions in initializers retain the conservative behavior.

The stored Boolean remains a snapshot: changing `ready` never changes `can_update`. Writes to an
input invalidate facts about that input, while the saved value and its unchanged aliases retain
their own facts. Assigning to the saved local invalidates its old facts and records the new result
when supported. Ownership MIR represents these results with `BooleanValue` edges after
initialization, sharing the existing forward/backward path analysis, alias invalidation, and
sixteen-alternative widening rather than re-evaluating the initializer.

Validation includes all 256 pairs of two-boolean truth tables, complementary and conflicting scalar
conditions, boolean subjects, guarded exits, assignment/alias/call invalidation, short-circuit side
effects, and loop backedges. A VM/native parity case runs all four boolean inputs and verifies
skipped and executed RHS calls in both optimization modes.
Saved-condition tests additionally cover all 256 truth-table pairs, input and destination writes,
reference/closure/call mutation, loop reassignment, snapshots, and aliases. VM/native parity tests
check every two-input combination and ensure effectful and short-circuited initializers execute
their operands only as required.

## Limits

The analysis does not prove every safe program. Unbounded theorem proving and general
arithmetic/alias analysis are outside its scope. Remote request completion requires separate
request/owner relationships; a callable's loan summary cannot discharge those obligations.
