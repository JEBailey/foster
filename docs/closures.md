# Foster Closures and Group Borrowing

Status: **accepted design; executable closure and group-borrowing foundation complete**

Implemented today:

- anonymous expression and block closures;
- nested named functions;
- lexical free-name resolution and minimal local capture sets in HIR;
- concrete synthetic HIR functions for closure bodies;
- inferred `copy` capture for primitive copy types and `move` capture otherwise;
- closure parameter/result inference and direct calls;
- concrete closure environments in the register VM;
- explicit `[copy ...]`, `[move ...]`, and `[ref ...]` capture clauses;
- identity-bearing places and mutable borrowed captures;
- group-parameterized reference types and `read`/`mut`/`reshape`/`consume` effects;
- effect-checked callable contracts with compiler-inferred representation erasure;
- projected list references with structural-invalidation diagnostics;
- `_` placeholder partial application.

Move and initialization analysis runs over ownership MIR control flow, including conservative
branch joins. Direct references, aggregate-held references, and captured references share one HIR
provenance analysis. Structural invalidation is driven by callable `reshape`/`consume` metadata
rather than a list of recognized method names.

This document records Foster's accepted closure model and the parts implemented by the compiler
and VM. Capture semantics affect parsing, type inference, ownership, groups, effects, HIR, and the
runtime representation of functions.

## Design goals

Closures should:

- use the same single-ownership and group-borrowing rules as every other value;
- support safe mutable aliasing without runtime borrow checks;
- make escaping borrowed state visible in types;
- avoid heap allocation when a concrete closure environment has a known size;
- distinguish mutation of a closure environment from mutation through a captured reference;
- permit useful invalidation patterns that Rust's aliasing-xor-mutability rule rejects;
- have predictable lowering to records and call functions.

## Surface syntax

### Anonymous closures

An expression body is concise:

```foster
let triple = (value: Int) -> value * 3
```

A block body supports statements:

```foster
let describe = (name: String, score: Int) -> {
    println(name, ":", score)
    score
}
```

Parameter and result types participate in ordinary inference:

```foster
let increment = (value) -> value + 1
```

### Nested named functions

Nested functions are closures when they use bindings from an enclosing function:

```foster
func multiplier(factor: Int) {
    func apply(value: Int) -> Int {
        factor * value
    }
}
```

Like every other statement in a Foster block, a nested function declaration has a value. Its value
is the function it declares, so a declaration in final position is the block's implicit result. A
nested function with no captures lowers to an ordinary function item.

### Explicit capture clauses

Capture sets are inferred by default. A capture clause documents or overrides individual capture
modes:

```foster
let counter = [ref count] () -> count
let consumer = [move resource] () -> use(resource)
let snapshot = [copy scale] (value) -> scale * value
let mixed = [move name, ref person] () -> rename(person, name)
```

Capture modes are:

- `ref local`: capture a non-owning `PlaceHandle` for the local's place;
- `move value`: transfer ownership into the closure environment;
- `copy value`: copy one of Foster's built-in copy types.

The capture list does not need to repeat inferred captures that are not being overridden. A strict
lint for complete explicit capture lists is possible later but is not implemented.

Whole-closure shorthand is reserved but not implemented:

```foster
move (value) -> factor * value
ref () -> count
```

## Closure environments

A closure has a unique anonymous record type. This closure:

```foster
let factor = 3
let triple = [copy factor] (value: Int) -> factor * value
```

lowers conceptually to:

```foster
type TripleEnvironmentGenerated = {
    factor: Int
}

func triple_call_generated(self: ref TripleEnvironmentGenerated, value: Int) -> Int {
    self.factor * value
}
```

The runtime closure value is the pair:

```text
(call function, environment)
```

The register VM currently stores captures in a managed environment vector. Native closure layout,
including when environments can be inline or require allocation, is a backend decision rather than
a promise of the bootstrap VM.

Reference captures and explicit projected references share the same runtime `PlaceHandle`
representation. A handle contains a weak origin slot plus its projection and structural generation.
Capturing a reference parameter flattens the parameter wrapper, so an escaping closure points to
the caller's original place. Borrow edges therefore cannot keep their own origin alive or form an
`Rc` cycle; the static loan rules guarantee liveness, while a failed weak upgrade is a defensive
`borrowed place has expired` runtime error.

## Inferred captures

Capture inference begins with the closure's free local identities. It does not capture an entire
scope, but projected field capture is currently rooted at its local:

```foster
let show_name = () -> println(person.name)
```

The current capture is the local `person`; the field access remains in the closure body. Capturing
only `person.name` is future provenance/layout work.

The implemented mode inference rules are:

1. An explicit capture clause wins.
2. A read of a `Copy` value is captured by copy.
3. Any other implicit capture moves the value into the closure.
4. Borrowing is always explicit with `[ref value]`; a borrowed capture may escape only when its
   group is exposed by the enclosing function's result contract.

This keeps the default independent of escape analysis. When a borrowed local would escape, the
compiler suggests transferring it instead:

```text
error: closure outlives borrowed local `factor`

  the returned closure captures `factor` by reference
  use `[move factor]` to transfer it into the closure
```

Explicit `[copy value]` is accepted only for the built-in copy types. Explicit `[move value]`
documents a transfer that would otherwise be inferred.

## Function types and latent effects

A closure's effects occur when it is called, not when it is created or passed around. Function
types therefore include a latent effect row:

```foster
func(Int) -> Int
func() -> String [read people]
func(String) -> () [mut people]
func() -> () [reshape entities.items]
func() -> Resource [consume self]
```

The effects are:

- `read group`: dereference or observe places in a group;
- `mut group`: change values without changing their storage identity;
- `reshape group`: move, create, or destroy members of a dynamic child group;
- `consume place-or-group`: transfer or destroy owned values;
- `mut self`: mutate owned fields in the closure's own environment;
- `consume self`: a call may consume the closure environment.

Effects are ordered by capability:

```text
read < mut < reshape
```

`consume` is tracked separately because consuming a value is not simply stronger mutation.

Concrete closure types retain their exact captures and effects. Erased function types retain the
group parameters and an upper bound on effects.

## Mutable aliasing

Multiple closures may capture references into the same mutable group:

```foster
let rename = [ref person] (name: String) -> {
    person.name = name
}

let birthday = [ref person] () -> {
    person.age = person.age + 1
}
```

Their inferred callable types are approximately:

```foster
rename: func(String) -> () [mut people]
birthday: func() -> () [mut people]
```

Both may remain live and both may be called. Foster does not require unique mutable closure
captures. Safety follows from group effects, place initialization, and invalidation tracking.

## Mutating owned captures

An owned capture belongs to the closure environment:

```foster
let next = [move count] () -> {
    count = count + 1
    count
}
```

Its type includes `mut self`:

```foster
func() -> Int [mut self]
```

Calling it requires mutable access to the closure value, but it does not mutate any external group.
If the environment contains a reference and the closure mutates through that reference, the effect
names the referenced group instead:

```foster
func() -> () [mut people]
```

A call may have both kinds of effect.

## Escaping borrowed closures

A borrowed closure can escape only if its group relationship appears in the result type:

```foster
func make_renamer[people: group Person](person: ref[people] Person)
    -> func(String) -> () [mut people]
{
    [ref person] (name: String) -> {
        person.name = name
    }
}
```

Returning a closure that borrows an unexposed local group is rejected:

```foster
func invalid() {
    let person = Person { name: "Grace" age: 37 }
    [ref person] () -> person.name
}
```

Moving the value into the environment is valid:

```foster
func valid() {
    let person = Person { name: "Grace" age: 37 }
    [move person] () -> person.name
}
```

## Dynamic-container invalidation

A closure may contain a reference into a dynamic child group:

```foster
let selected = ref people[0]
let show = [ref selected] () -> println(selected.name)
```

Value mutation preserves callability:

```foster
people[0].name = "Ada"
show()
```

Structural mutation may invalidate the captured reference:

```foster
people.push(new_person) // reshape people.items
show()                  // error
```

Foster does not need to forbid the `push` merely because `show` exists. Instead, `push` invalidates
the captured reference, and a later call is rejected. The closure may still be dropped. Other uses
are allowed only if they do not inspect or propagate invalid captures.

Initial rule: invalidation of a captured reference permanently makes that closure value uncallable.
We may later support repairing explicit capture fields, but closures do not automatically retarget
after a container changes shape.

## Calling capability

Foster exposes one `func(...) -> ... [effects]` type syntax rather than separate `Fn`, `FnMut`, and
`FnOnce` traits.

The compiler derives call requirements from effects:

- no `self` effect: callable through shared access to the closure value;
- `mut self`: callable through mutable access to the closure value;
- `consume self`: calling consumes the closure value;
- external group effects: checked against group access at each call site.

These distinctions remain present in HIR and MIR even though they do not use three surface traits.

## Copying and moving closure values

Closure values follow ordinary single ownership:

```foster
let other = closure // moves the closure
```

Closure values are currently ownership-bearing even when every capture is a copy type. Foster does
not yet expose general `Copy` or `Clone` protocols for aggregate or closure values.

Closures that may `consume self` are not callable after their consuming call.

## Type erasure

Every closure expression has a different concrete type. A callable type states the shared contract
needed for heterogeneous storage:

```foster
handlers: List<func(Event) -> () [mut app]>
```

When necessary, the compiler erases the concrete closure representation while retaining its VM call
target and capture environment. A closure conforms to a callable contract when:

- parameter and result types are compatible;
- its effects are a subset of the callable contract's declared effects;
- every borrowed capture group is represented by the callable contract;
- its call capability is no stronger than the callable contract permits.

Foster has only the `func` spelling. Whether a callable remains concrete, is specialized, or uses an
erased environment is an internal compiler and VM decision.

## Partial application

Placeholder partial application is closure syntax sugar:

```foster
let add_five = add(5, _)
```

lowers before capture analysis to:

```foster
let add_five = (value) -> add(5, value)
```

Multiple placeholders become parameters in left-to-right order. Captured supplied arguments obey
ordinary closure capture rules.

## Control-flow and diagnostics

Closure capture validity is flow-sensitive. At each call, the compiler verifies:

- the closure value is initialized and has not been consumed;
- every captured reference required by the call remains valid;
- the caller permits the closure's latent external effects;
- mutable or consuming access to the closure environment is available when required.

Diagnostics should name both the closure and the operation that invalidated it:

```text
error: closure `show` is no longer callable

  `show` captures a reference into `people.items`
  `people.push(...)` reshaped `people.items` here
```

## Concurrency

Group borrowing guarantees memory safety within a thread; it does not by itself permit concurrent
mutation. Sending or sharing a closure depends on its captures:

- an owned closure is sendable when every owned capture is sendable;
- a borrowed closure is sendable only when the captured groups may cross the concurrency boundary;
- concurrent calls that mutate an overlapping group require synchronization or exclusive task
  ownership;
- representation-erased closures preserve these requirements.

The exact `Send`, `Share`, task, and synchronization model is deferred to the concurrency design.

## Compiler implementation status

1. Parse anonymous closures, nested functions, and capture clauses. **Complete.**
2. Add closure expressions, nested function IDs, and free-name references to HIR. **Complete.**
3. Resolve free names lexically and compute minimal local capture sets. **Complete.**
4. Infer copy/move capture modes and explicit borrow effects; diagnose escaping local borrows.
   **Complete for the executable type/place foundation.**
5. Lower concrete capture layouts and call functions to VM bytecode. **Complete.** Ownership MIR
   records capture uses; VM closure frames materialize the environment.
6. Extend place/group validity analysis across closure construction, storage, and calls.
   **Complete for local and projected list places, with conservative control-flow joins.**
7. Execute concrete closure environments in the VM. **Complete for copy, move, mutable-reference,
   and shared-reference captures.**
8. Add compiler-inferred representation erasure for callable values. **Complete in the VM.**
9. Add `_` partial application sugar. **Complete.**

The VM now supports owned, copied, and shared-place environments. The compiler
rejects use after move, invalid explicit copies, escaping borrows whose group is absent from the
result type, effect-unsafe erasure, and calls after a projected capture is structurally invalidated.

## Accepted decisions

1. Arrow syntax for anonymous closures and ordinary `func` syntax for nested named closures.
2. Minimal inferred capture sets with `[ref ...]`, `[move ...]`, and `[copy ...]` overrides.
3. No silent move solely because a closure escapes; require an explicit move or copy.
4. One surface function type with latent effects rather than `Fn`/`FnMut`/`FnOnce` traits.
5. Safe mutable aliasing between closures through shared group parameters.
6. Structural mutation invalidates affected captured references instead of being prohibited.
7. Inline concrete environments; boxing only for erasure or layout requirements.
8. Partial application as closure sugar.
