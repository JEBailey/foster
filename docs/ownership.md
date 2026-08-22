# Foster Ownership and Borrowing

**Status:** implemented foundation with provisional rules and known conservative checks.

This document describes Foster's ownership model, its source-level behavior, and how the compiler
implements it today. It is intentionally separate from
[`effect-derivation.md`](effect-derivation.md): ownership answers *who controls a value and when a
reference remains valid*, while effects describe *what a function may do to a borrowed group*.

## Design goals

Foster uses single ownership with explicit borrowing. The model is intended to provide predictable
resource cleanup, safe mutation, transferable values for remote workers, and useful function
contracts without reproducing Rust's lifetime syntax.

The central ideas are:

- Every non-reference value has one owning place.
- Moving a value transfers ownership and invalidates the previous place.
- Copy types may be duplicated without invalidating their source.
- A reference borrows a place rather than owning its value.
- References are associated with named **groups**, not source-level lifetime parameters.
- A function signature declares the operations it may perform on each borrowed group.
- Structural mutation invalidates references into elements whose location may have changed.

## Values, locals, and places

A **value** is data such as an `Int`, `String`, list, record, closure, or remote handle. A local name
identifies a **place** that can contain a value. Member and index projections extend a root place:

```foster
people             // root place
person.name        // field place
people[index]      // indexed place
```

Ownership operations apply to the root and, where supported, its projection. This distinction is
important: reading `people[index]` reads a value, while `ref people[index]` creates a loan into the
storage owned by `people`.

## Copy and move

The built-in copy types are currently:

- `Unit`
- `Bool`
- `Int`
- `Float`
- `CodePoint`
- `Byte`
- `Symbol`

Other values are ownership-bearing and default to move semantics when captured. Explicit move-out
is represented in HIR as `MoveOut(place)`. Once a root has been moved, subsequent reads or writes
through that root are rejected.

Function calls are different from closure capture: arguments are borrowed by default. A function
that takes ownership names each ownership-taking parameter with a `consume` contract, and callers
write `move` when transferring an existing place:

```foster
func inspect(message: String) -> Int {
    message.length
}

func send(message: String) -> Unit [consume message] {
    deliver(move message)
}

message = "hello"
inspect(message)     // borrowed; message remains usable
send(move message)   // transferred; message is now uninitialized
```

Copy arguments (`Unit`, `Bool`, `Int`, `Float`, `CodePoint`, `Byte`, and `Symbol`) need no `move`, even
when the parameter has a `consume` contract. Fresh temporaries also transfer directly because there
is no source place to invalidate. Explicit `ref[group] T` remains available when a borrow must be
named, stored, returned, captured, or related to group effects; it is not required for an ordinary
call.

A borrowed parameter with a `mut` effect shares its caller place for the duration of the call. This
allows generic stateful algorithms to update readers, writers, iterators, and ordinary records
without manufacturing explicit reference types at every call boundary. Read-only borrowed
parameters may be represented by an independent read view because their contract cannot expose a
mutation; consuming parameters receive the transferred value instead.

Structural record adaptation follows the same ownership rules. Passing a wider record where a
narrower public shape is expected borrows the original value without allocating a wrapper.
Consuming through that narrower contract moves the original value and invalidates the source; the
extra runtime fields do not create a second owner.

Callable types preserve this behavior positionally. Parameters borrow unless prefixed with
`consume`:

```foster
func(String) -> Unit
func(consume String) -> Unit
```

When `func send(message: String) [consume message]` becomes a function value, the compiler converts
the name-based declaration into a positional `Consume` mode for parameter zero. That mode survives
closure assignment, partial application, generic instantiation, and compiler-inferred callable
erasure. Indirect
calls therefore require the same `move` as direct calls. Parameter modes describe what happens to
the argument itself; group effects continue to describe access through references.

Closure captures make the choice visible:

```foster
[copy count] () -> count
[move message] () -> message
[ref person] (name: String) -> person.name = name
```

`copy` requires a copy type. `move` transfers the captured value into the closure. `ref` stores a
borrowed connection to the original place. When no capture mode is written, type checking resolves
the pending mode to `copy` for copy types and `move` otherwise.

## References and groups

A reference type names the group from which it borrows:

```foster
func rename[people: group Person](person: ref[people] Person, name: String)
    -> Unit [mut people]
{
    person.name = name
}
```

`people` is a group parameter. `ref[people] Person` says that `person` may refer into that group.
`mut people` says the body may mutate values in it. Group names are part of the function contract:
using an undeclared group is an error, and a type parameter and group parameter may not share a
name.

Groups describe sets of possible locations. They are not values, modules, hidden owner objects, or
lexical lifetime variables. A reference may escape a function only when its group is exposed by a
reference, nested type, callable result, or effect in the declared result type. A reference into a
frame-local value cannot be returned.

Methods may use `self` as their receiver group. Non-method functions cannot declare effects on
`self`. Compiler-created closure functions derive group effects from their reference captures.

## Group effects

Access permissions form this ordering:

```text
read < mut < reshape
```

- `read group` observes borrowed data.
- `mut group` may replace values without changing the container's structure.
- `reshape group` may structurally change a collection, potentially relocating elements.
- `consume group-or-parameter` is independent of that ordering and permits ownership to be
  extracted. Naming a parameter is the ordinary ownership-taking function contract.
- `suspend` is a separate function property, not an ownership effect.

`mut owner` also covers extracting an owned descendant when the operation replaces that part of
the owner, which is how `Iterator.next()` yields `T` while advancing its cursor. It never covers
consuming `owner` itself.

The compiler derives effects from typed HIR. When a function omits an effect clause, its inferred
effects become the function contract. When a function provides an explicit bracketed clause, the
compiler requires:

```text
effects required by the body ⊆ effects declared by the signature
```

An explicit contract missing a permission is an error. An unnecessarily strong explicit
permission produces a warning. Inferred functions require neither repetition nor annotations.
Calls propagate the callee's contract after substituting its group arguments. See
[`effect-derivation.md`](effect-derivation.md) for the derivation algorithm.

## Structural invalidation

Replacing an element and changing the shape of its owner are different operations. A reference to
`people[0]` can remain valid across ordinary reads and value mutation, but an operation such as
`people.push(...)` may reallocate the list. Foster therefore invalidates projected loans after a
reshape of their root:

```foster
first = ref people[0]
people.push(other)
first.name // error: the projected reference was invalidated
```

Consuming the root invalidates all loans from it. The compiler permits structural mutation after a
loan's last use; it does not require the loan to remain active until the end of the lexical block.

Closures are checked as well. If a closure captures a projected reference and its origin is later
reshaped, calling that closure is rejected because the captured reference is no longer valid.

## Borrowed closure escape

A returned closure may own or copy its captures. A closure that borrows a local cannot outlive the
function frame:

```foster
func invalid() {
    value = Person { name: "Ada" }
    [ref value] () -> value.name // cannot escape
}
```

A borrowed capture may escape when it originates from a reference parameter and the result type
exposes the same group:

```foster
func make_renamer[people: group Person](person: ref[people] Person)
    -> func(String) -> Unit [mut people]
{
    [ref person] (name: String) -> person.name = name
}
```

The returned callable's effect contract communicates that invoking it may mutate `people`.

Borrowed values cannot be stored back into the place from which they borrow. For example, assigning
a `[ref object]` closure into a field of `object` is rejected. This prevents an object from owning a
borrower of itself and makes the runtime borrow graph non-owning and acyclic.

## Remote ownership boundary

`remote value` transfers an owned object to a worker. Calls on its remote handle enqueue messages
and return futures. Ordinary explicit reference values cannot be transferred as owned mailbox
values: that would connect two independently executing ownership domains without a loan protocol.
Foster instead has two controlled read-loan forms described below.

`remote ref value` is the explicit exception for the remote receiver itself. It creates a
persistent read-only capability backed by the owner's live storage; it does not send a reference as
a mailbox argument. The owner remains free to mutate the record. Remote reads take shared access,
and owner method calls take exclusive access and commit atomically, so readers observe either the
state before or after a mutation rather than an intermediate state. A borrowed remote handle cannot
call a method with `mut self`, `reshape self`, or `consume self` effects.

Borrow-mode remote method arguments are call-scoped read-only loans over the same shared storage.
The mailbox retains the capability while queued; the worker acquires shared access for the complete
invocation and releases it when the method returns. Awaiting the future is not what releases the
loan. A consume-mode parameter instead requires `move` and crosses the mailbox as an owned value.
The compiler rejects non-read effects on borrowed remote parameters; ownership analysis prevents a
borrowed parameter from being moved into actor state or returned as an owned result.

```foster
pending = analyzer.inspect(document)  // temporary read-only loan
analyzer.submit(move document)        // ownership transfer
```

Owned records, variants, lists, primitives, and other recursively transferable values may be sent.
The worker owns its object, so its cleanup is tied to the lifetime of the remote worker/handle rather
than to an untracked global task.

## Compiler implementation

The relevant compilation order is:

```text
AST
  → lower to resolved HIR
  → infer effects for reference captures
  → validate group and effect declarations
  → infer types and derive body effects
  → resolve pending closure capture modes
  → check moves and borrowed closure escape
  → check direct loans, escape, and invalidation
  → check captured-reference invalidation
  → lower canonical places into ownership MIR
  → run control-flow initialization and move analysis
```

The implementation is divided by responsibility:

- `src/hir/lower/` resolves names and converts source references, moves, captures, and places into
  stable HIR IDs.
- `src/typecheck/effects.rs` derives group access, consume, and suspension requirements.
- `src/hir/ownership.rs` validates groups, resolves capture modes, checks borrowed closure escape,
  and checks captured-reference invalidation.
- `src/hir/loans.rs` tracks direct loans and rejects invalid use or escape.
- `src/hir/queries.rs` contains shared, policy-free place and expression queries used by the
  ownership passes.
- `src/ownership/` lowers typed HIR to ownership MIR basic blocks containing explicit
  read/copy/move/borrow/initialize operations, validates consuming call contracts, then checks
  initialization and partial-move state at control-flow joins.
- `src/vm/value.rs` implements slots and the common weak `PlaceHandle` used by projected references
  and borrowed captures.
- `src/vm/runtime.rs` and VM capture instructions represent copied/moved values and borrowed places.

HIR uses resolved `LocalId` and `ExprId` identities, so ownership checks do not compare source names.
Canonical places contain a root local plus field, index, or dereference projections. Ownership MIR
uses those places to distinguish whole-value moves from partial moves of disjoint fields. A loan
currently records its root local and whether it projects an indexed item. Moves and invalidations
are found by recursively walking each statement's expression tree.

At runtime, VM references retain the origin's structural generation but only a weak connection to
the origin slot. A reshape increments that generation, and dereferencing an older reference fails.
An expired weak place also fails safely. These are defensive runtime backstops; well-typed programs
should be rejected statically before reaching either condition.

Mutable binary construction follows the same rules. `ByteBuffer.snapshot` borrows the buffer and
copies its current contents into immutable `Bytes`. `ByteBuffer.freeze` consumes the buffer, written
as `(move buffer).freeze()`, and transfers its allocation into `Bytes`. Structural operations such
as `push`, `extend`, `clear`, `truncate`, and `reserve` invalidate outstanding indexed loans because
they may relocate or remove elements; replacing an existing byte through an indexed mutable loan
does not change the buffer's shape.

## Diagnostics

Ownership violations are compile errors. Current errors cover:

- using or assigning a value after it was moved;
- explicitly copying a non-copy value;
- returning a reference into a frame local;
- returning a borrowed closure whose group is not exposed by its result type;
- storing a borrower into the place from which it borrows;
- using a projected loan after structural mutation;
- calling a closure after mutation invalidated a captured reference;
- sending an explicit reference as an ordinary mailbox value;
- calling a mutating method through a read-only remote loan;
- mutating a borrow-mode remote method parameter;
- undeclared, missing, or invalid group effects.

Effect over-declaration is safe and therefore uses the compiler's warning channel instead.

## Current limitations and evolution

The implemented model is useful but is not yet a general Rust-equivalent borrow checker:

- Move and initialization analysis is control-flow-aware, but direct-loan analysis remains a
  conservative forward pass over HIR statements.
- Ownership places model field, index, and dereference projections. Loan invalidation still reduces
  these to a root and whether an indexed projection is relocation-sensitive.
- Structural invalidation currently recognizes the implemented reshape operations, notably list
  `push`, rather than a trait- or metadata-driven set of operations.
- Closure invalidation tracks closures bound directly to locals; ownership flowing through records,
  variants, containers, and returned higher-order values needs a more general provenance analysis.
- Copy behavior is currently a built-in type classification. User-defined copy types have not been
  designed.
- Runtime values still use managed host representations in the VM. Ordinary registers are inline
  and promote to stable slots only when their identity becomes observable. The bytecode compiler
  emits deterministic `Drop` instructions after register last use, while observable shared slots
  remain alive through frame teardown. Borrow edges are weak and therefore do not create reference
  cycles. Native layout, arbitrary cyclic owned graphs, resource destructors, and destructor
  ordering remain backend work.

The intended evolution is richer place/provenance tracking and control-flow-aware loan states while
preserving the source model: ownership transfer stays explicit, references name groups, and API
effects remain readable contracts rather than inferred lifetime syntax.
