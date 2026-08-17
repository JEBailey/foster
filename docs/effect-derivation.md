# Foster effect derivation

**Status:** implemented. This document describes the current compiler rather than the historical
v1 implementation plan.

Effects state what a function may do to an ownership group. Most functions omit an effect clause;
the compiler derives their contract from typed HIR and stores it on the function. Explicit clauses
are useful for callable contracts and for APIs that intentionally publish an upper bound.

```foster
func set[state: group Int](value: ref[state] Int, next: Int) -> Int [mut state] {
    value = next
    value
}
```

A callable type fragment can publish the same contract without a body:

```foster
func(Event) -> Unit [mut application, suspend]
```

Effect clauses follow the result type or, for an anonymous closure, the arrow. They are bracketed
and comma-separated. Loose suffixes such as `-> Int mut state` are not valid Foster syntax.

## Effect model

Access permissions form an ordered family:

```text
read < mut < reshape
```

- `read group` observes values owned by the group.
- `mut group` may replace values while preserving container structure.
- `reshape group` may change storage structure and invalidate projected references.
- `consume group-or-parameter` permits ownership extraction and is independent of the access
  ordering.
- `suspend` is a function property rather than an effect on one group.

Effects use structured paths. A root permission covers its descendants, while a child permission
does not cover its parent or siblings:

```text
mut inventory          covers mut inventory.items and mut inventory.items.count
mut inventory.items    does not cover mut inventory or mut inventory.owner
```

The parser represents a target as `GroupPath { root, children }`. Indexed list storage uses the
synthetic `items` child, so an in-place `push` derives `reshape values.items`.

## Inferred and explicit contracts

For a function without an explicit clause:

```text
effects(signature) = fixed_point(derived_effects(body))
```

Inference runs to a fixed point because recursion and mutually dependent calls can propagate
effects and suspension. Capture classification feeds back into the process so closure ownership
and callable parameter modes are stable before final checking.

For an explicit clause:

```text
derived effects(body) subset-of declared effects(signature)
```

A missing permission is a compile error. A stronger permission or `suspend` marker that the body
does not need produces a warning through the compiler diagnostic channel. Over-declaration remains
sound, but callers should see the narrowest useful contract.

The entry-point `main` may await without publishing `suspend`, because Foster code never calls it.

## Group identity and ownership

References carry the group they borrow:

```foster
ref[people] Person
```

The type checker tracks an owner group for each relevant local:

- a `ref[g] T` parameter belongs to `g`;
- a local initialized from a reference retains that reference group;
- a method receiver belongs to the reserved `self` group;
- owned locals belong to the internal frame group.

Frame-local effects are implementation details and are filtered from public contracts. This is why
ordinary mutation of a local record does not force a source annotation.

`self` is accepted as an effect root only for a real instance method—one whose first parameter is
named `self`:

```foster
func increment(self: Counter, amount: Int) -> Int [mut self] {
    self.value = self.value + amount
    self.value
}
```

A non-method declaration containing `mut self` is rejected.

## Operations that derive effects

The effect walker covers every implemented HIR expression and statement. The important cases are:

| Operation | Derived capability |
| --- | --- |
| Read through a borrowed place | `read` on its structured group path |
| Assignment through a reference | `mut` on the reference group |
| Field or indexed assignment | `mut` on the rooted place path |
| In-place list `push` | `reshape <group>.items` |
| `move place` | `consume` on the place path |
| `await expression` | `suspend` plus effects needed to produce the future |
| Direct, method, or callable invocation | instantiated callee effects |
| Record/list/variant construction | effects of each supplied expression |
| `remote value` | effects needed to construct the transferred value; actor `self` effects are cut |

Functional `List.append` does not derive `reshape`; it returns a new list. The intrinsic policy is
centralized so future in-place operations must declare their effect instead of being recognized by
scattered name checks.

## Call delegation

Direct functions, instance methods, closures, erased callable values, and partial applications all
retain the same effect and suspension information.

At a call site, every callee group parameter is independently substituted from the corresponding
`ref[formal]` parameter and actual argument. Child path components survive substitution. A method's
`self` effect is instantiated according to its receiver:

| Receiver | Meaning of the callee's `self` effect |
| --- | --- |
| Frame-owned local record | Internal frame group; filtered from the caller contract |
| Borrowed record from `ref[g]` | Delegated to `g` |
| Owned `Remote<T>` | Cut at the actor boundary |
| `Remote<ref[g] T>` | Only `read self` is permitted and maps back to `g` |

A remote method call enqueues work and returns a future; it does not itself suspend the caller.
Evaluating `await` derives suspension. Owned remote arguments cross as messages. Borrow-mode remote
arguments are call-scoped read-only loans whose read effects map back to the caller's group.

## Closure effects

Lowered closures inherit the enclosing function's group parameters. Capture seeding records the
provenance of captured `LocalId`s before walking the closure body. `[ref value]` captures contribute
their external group; owned environment mutation remains frame-internal.

Anonymous closures may state a latent contract after the arrow:

```foster
update = [ref value] (next: Int) -> [mut state] {
    value = next
}
```

The compiler combines capture-derived requirements with this explicit row and checks the body
against the result. Function types preserve effects, suspension, and positional consuming modes:

```foster
func(consume Job) -> Unit [mut queue, suspend]
```

Erasure and indirect calls therefore do not discard ownership behavior.

## Compiler implementation

The relevant implementation is split by responsibility:

- `src/typecheck/effects.rs` walks typed HIR, derives structured group paths, delegates calls, and
  records suspension.
- `src/typecheck/predicates.rs` defines effect subset/coverage and callable contract helpers.
- `src/hir/ownership.rs` validates declared groups and seeds reference-capture effects.
- `src/ownership/` validates positional consume modes, moves, initialization, and partial moves on
  control-flow basic blocks.
- `src/diagnostic.rs` carries over-declaration warnings and their source spans.

Compilation first infers effects, updates HIR, and repeats until no function contract changes. It
then performs a final checked type/effect pass before loan and ownership validation.

## Current limits

The effect contract is implemented, but provenance can still become conservative:

- aggregate provenance for loans stored inside arbitrary records and variants is incomplete;
- structural invalidation metadata currently covers the implemented list-storage model rather
  than a general user-defined collection protocol;
- path-disjoint reasoning beyond named fields and the initial `items` model is limited; and
- branch joins conservatively retain an invalidation even when richer path facts could prove one
  arm unreachable.

These are precision limits, not syntax placeholders. The current compiler enforces root and child
effects, multiple group substitutions, explicit closure rows, move-out consumption, suspension,
remote boundary cuts, and warnings for unnecessarily broad explicit contracts.
