# G-07: Ownership analysis precision

Status: parts 1 and 2 implemented for the bounded supported forms below. This work improves
compiler precision without changing the language contract.

## Problem

The checker sometimes rejects safe programs because it cannot prove that a result is independent
of an input loan, or that an invalidation and a later use occur on mutually exclusive paths.
G-07 should accept specified additional safe cases while preserving S-13 and S-14 in
[the semantic specification](semantics.md#6-loans-groups-and-invalidation).

A result's provenance is the set of storage origins it may still depend on. For example, a
returned reference depends on its source; a newly produced independent string need not depend
on the list element used to compute it. Keeping an unnecessary dependency can prevent reshaping
that list while the string is still used.

## Existing baseline

- Direct calls use inferred receiver/parameter result summaries, propagated to a fixed point
  through direct call chains. Declared result groups must cover the actual returned origins.
- Indirect calls use known callable summaries or a typed fallback as described below. Unknown
  relationships retain conservative dependencies.
- Stable boolean places, enum patterns, and direct scalar comparisons retain branch facts.
  Equality/inequality normalization and some dynamic-index disjointness already work.
- Compound predicates and comparisons of computed values remain conservative. The loan checker
  widens to shared facts after sixteen alternatives; more reasoning must remain bounded.

See [ownership analysis](ownership.md#current-limitations-and-evolution) and
[verification](ownership-verification.md#place-reasoning). The principal implementation entry
points are `call_result_borrow_value` in `src/ownership/lower.rs`, result-summary inference in
`src/ownership/regions.rs`, and the ownership MIR's comparison/place representation.

## 1. Precise results through indirect callables

Target behavior: passing a function through a variable, parameter, record, or callable adapter
should preserve a provable result dependency instead of automatically adding all input origins.
For example, invoking an independent-string producer through a callable should permit the same
later list mutation that its direct invocation permits. A callable returning a reference into
that list must continue to prevent an invalidating mutation before the reference's last use.

Implemented approach and supported forms:

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

Remaining limits within part 1: dynamic-index target selection, target identity returned through
opaque factories, capture-specific result summaries, and hidden borrowers in aggregate parameters
remain conservative. Known targets can still retain unnecessary environment dependencies for
borrow-containing results. Effectful calls and reference captures may discard more target knowledge
than necessary. Pending remote-request relationships are tracked separately and are not discharged by these proofs.

Acceptance criteria:

- Safe independent results work through local callable aliases and known-target joins, then
  through the explicitly supported parameter, aggregate, capture, and erased-callable cases.
- Borrowed results keep every possible parameter/capture origin, including after moves,
  adaptation, reborrowing, and recursive summary propagation.
- A join containing one dependent target still rejects invalidation before result use. Unknown
  targets and incompatible contracts cannot silently discard dependencies.
- Paired direct/indirect regression cases demonstrate the intended precision gain; accepted
  runnable cases agree in VM and native execution with optimization enabled and disabled.

## 2. Bounded reasoning for compound conditions

Target behavior: recognize mutually exclusive paths expressed using supported boolean
combinations, rather than requiring the same simple comparison to appear on both branches.
For example, reshape under `a && b` and use an earlier loan only under `not a || not b` are
mutually exclusive while both operands remain unchanged. Reshape and use under the same
condition remain a conflict. These cases are supported.

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
conservative; assigning a compound result to a boolean does not preserve its defining formula.

Validation includes all 256 pairs of two-boolean truth tables, complementary and conflicting scalar
conditions, boolean subjects, guarded exits, assignment/alias/call invalidation, short-circuit side
effects, and loop backedges. A VM/native parity case runs all four boolean inputs and verifies
skipped and executed RHS calls in both optimization modes.

Release validation: the nine compound-condition tests (including all 256 truth-table pairs) took
1.52 seconds. All 38 VM/native parity tests passed in 162.55 seconds using the shared native runtime
cache, compared with 164.55 seconds for the preceding 37-test baseline. These are local suite timings,
not a portable benchmark. Path solving is skipped when conservative loan analysis finds no candidate
conflict; alias invalidation is computed once per operation rather than once per path alternative.

Acceptance criteria:

- Recognize conjunction, disjunction, negation, and their complementary paths over stable facts.
- Forget affected facts after assignment, overlapping mutation, or a call whose effects can
  change their operands. Facts must not incorrectly survive loop backedges or altered indices.
- Every newly accepted case has a nearby rejected case with a feasible invalidation/use path,
  including operand mutation between tests and side effects during predicate evaluation.
- Exhaustive small-CFG reference-model checks cover the new fact operations. State growth and
  compile time remain bounded; reaching a precision limit falls back conservatively and never
  converts an unknown relationship into proven disjointness.

## Scope and completion

Part 1 precedes part 2. Track their completion separately, with a documented list of
supported forms, remaining conservative cases, and regression/performance results. G-07 is
complete for this scope when the acceptance criteria above pass; it does not mean the compiler
can prove every safe program.

This work does not change ownership transfer, `Copy`, `Drop`, group syntax, or runtime lifetime
rules. G-08's nested indexed writes are a separate correctness bug. G-06's pending remote-request
transfers need request/owner relationships in addition to ordinary loan provenance; improving a
callable's loan summary alone must not remove those restrictions. Unbounded theorem proving,
general arithmetic/alias analysis, and remote scheduling are outside this issue.
