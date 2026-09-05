# Foster semantic specification

Status: **draft normative specification**, revision 3, 2026-09-05.
Baseline: **language version 7, ownership-model version 3**.

This specification states the observable meaning of Foster programs independently of the VM,
Cranelift, reference counting, or physical layouts. It consolidates existing contracts; publishing
it does not change the language or ownership versions. It is not a complete formal semantics or a
proof that either backend implements every rule.

## 1. Authority and terminology

**S-01 — Contract and conformance.** “Must” and “must not” identify requirements of this draft.
Implementation gaps are listed in section 12; they do not create alternative language meanings.
An unresolved question is not permission to rely on whichever behavior one backend happens to
produce. Changes follow the [pre-release development policy](development-policy.md).

The [language design](language-design.md) remains the syntax inventory. The detailed
[ownership contract](ownership.md) and [effect rules](effect-derivation.md) supplement this
specification. Conflicts must be reconciled explicitly, not resolved by silently changing code or
treating documentation alone as an implementation change.

**S-02 — Semantic entities.** A *value* is typed data. A *place* is storage that may contain a
value: a local, a field, an indexed element, or a place reached through a reference. An *owner*
controls an ownership-bearing value. A *loan* permits access to an originating place without
owning it. A *group* describes possible origins and access permissions, not a runtime owner.
An *effect* describes an operation a callable may perform. An *invocation* is one execution of a
function, including its parameters, locals, temporaries, and control position.

The abstract state consists of bindings to places, each place's initialization state, owned
values, loan origins and validity, and active invocations. Remote execution additionally has
worker state, message queues, and future outcomes. Implementations need not represent this state
literally.

Expressions are classified by their resolved meaning, not by whether a backend represents their
result with an address. A local is a place. A declared stored field rooted in a place and an indexed
element rooted in a place are projected places. A reference expression produces a value that
retains its origin place; stored projections through that reference can designate storage.
Literals, operators, calls, constructors, branches, closures, and computed members produce values.
A value may be materialized in temporary storage when a context needs a place, but that does not
change the expression's category or extend the temporary's lifetime.

## 2. Names, types, and contracts

**S-03 — Static meaning.** Names, types, overload choices, ownership modes, and effect contracts
must be checked before ordinary execution. `let` introduces a local; assignment does not declare
one. Declarations are private unless made public. Module qualification uses `::`; fields, methods,
associated functions, and enum cases use `.`. Imports do not run application initialization code.
Module constants have compile-time initializers; cyclic constant dependencies are rejected.

**S-04 — Conformance.** Structural conformance requires compatible accessible fields and methods,
including their ownership modes, effects, and suspension requirements. It does not require nominal
inheritance. Adapting a value to a narrower contract must preserve its ownership and loan origins;
the adaptation does not manufacture an independent owner. An intersection requires every component
contract. A union contract admits its member types and is not an enum constructor or source-level
tagged variant. An enum has distinct cases and an optional single payload per case.

**S-05 — Generic and callable identity.** Generic substitution must preserve type relationships,
parameter modes, effect substitutions, and borrower provenance. Specialization and representation
erasure must not weaken those obligations. Overloads are selected by arity and compatible parameter
types, preferring exact matches over lossless conversion; equally ranked choices are errors.
Return types, effects, consumption, and suspension alone do not distinguish overloads.

## 3. Values and primitive operations

**S-06 — Scalar domains.** `Int` is a signed 64-bit integer. Integer addition, subtraction,
multiplication, negation, and division must fail on an unrepresentable result; integer division
also fails on zero and otherwise truncates toward zero. `Float` uses IEEE-754 binary64 operations
and comparisons; it is not implicitly interchangeable with `Int`. This draft does not prescribe
NaN payload bits or cross-platform floating-point text formatting.

`Byte` ranges from 0 through 255. Its bitwise operations produce bytes; shifts require a count
from 0 through 7 and left shifts discard bits beyond the byte. `CodePoint` is a Unicode scalar
value, excluding surrogates. `Byte` and `CodePoint` widen losslessly when `Int` is expected;
integer arithmetic on them produces `Int`. Narrowing requires the appropriate checked API.
There is no universal nullable type; absence is represented explicitly, commonly by `Option<T>`.

**S-07 — Owned data and representation.** Physical sharing of ordinary owned record, list, and
byte storage must not make mutation of an independently obtained owned value mutate another
owner's data. An implementation may copy eagerly or detach shared storage on mutation.
This rule does not turn ordinary ownership-bearing assignment into an implicit clone operation.
It also does not make references, remote handles, or external resource capabilities independent
snapshots: their explicit connections retain their specified meaning.

Equality for ordinary data must compare the defined values rather than incidental heap addresses
or allocation padding. Record/list/enum equality compares corresponding initialized contents;
floating-point members retain IEEE equality. No universal equality or ordering contract for every
kind of callable, reference, or host handle is introduced here.

**S-08 — Text and collections.** Strings contain valid UTF-8. String code-point operations count
Unicode scalar values, not bytes or grapheme clusters; byte operations use byte offsets.
String equality does not imply Unicode normalization. Lists are homogeneous ordered collections.
Invalid checked indexing is a language failure, not unchecked memory access.

The following are library contracts, not new syntax:

- `List.at(index)` reads an independently consumable element without moving it out of the source.
  It must preserve any loans contained inside that element; an owned read is not lifetime erasure.
- `values[index]` can designate a projected place, and `ref values[index]` borrows that place.
- `String.bytes` produces byte data without consuming the string. An ordinary stored field named
  `bytes` does not acquire that behavior merely from its spelling.
- `List.slice` and `Bytes.slice` check half-open bounds and return range copies, not zero-copy views.
  `String.slice` uses clamped half-open code-point bounds, as specified by its library implementation.
- Builder mutation accumulates output; consuming finalization returns an owned result. Allocation
  reuse and capacity growth must not change the resulting values or invalidate fewer loans than
  the published effect contract requires.

A resolved member is one of a stored place, a computed value, or a method. Stored-place status
comes from a field declaration. Compiler-provided computed members produce values and cannot be
assigned to; their ordinary result type determines whether the result is copied or owned. Sharing
immutable storage is an implementation detail and does not turn a computed result into a place.
Foster does not currently provide user-defined getter/setter declarations or implicit
getter-modify-setter write-back; user-defined computations are methods.

Compiler-provided members have the following resolved classifications. The type checker records
the classification on each member expression; effects, ownership, the VM, and native lowering use
that shared result rather than inferring ownership from the member's spelling.

| Operation | Classification |
| --- | --- |
| Declared field | Stored place when its receiver is a place |
| `empty?`, `whitespace?`, `length`, `capacity`, scalar `head` | Computed copy value |
| `String.bytes`, owned `head`, and `rest` | Independent owned value; immutable storage may be shared |
| A computed result whose declared type is a reference | Borrowed value retaining its group origin |
| `iterator` selection | Method; calling it creates an independent owned cursor |
| `List.at` and an index read used as a value | Copy or independent owned result according to the element type, while preserving borrower provenance contained by that element |
| Indexing rooted in a place | Projected place |

## 4. Evaluation and control flow

**S-09 — Sequencing.** Statements execute in source order. Ordinary binary operands evaluate
left before right. `&&` evaluates its right operand only if the left is true; `||` does so only
if the left is false. `!` and `not` have the same Boolean meaning. A guarded transfer evaluates
its guard before its transferred expression; a false guard does not evaluate that expression.
Effects of an executed guard still occur on the fall-through path.

Assignment evaluates the complete right-hand expression first. It then evaluates the left-hand
place once, including member receivers and index expressions, and replaces the selected value.
The previous value is not replaced until both evaluations have completed successfully. Thus, in
`values[index()] = replacement()`, `replacement()` runs before `index()`.

Except for assignment and conditional evaluation stated above, multi-operand expressions evaluate
operands from left to right. A call evaluates its callable first and then its arguments in source
order. A method call evaluates its receiver before its arguments. List elements and record field
initializers evaluate in source order; physical record layout does not affect that order. Enum
payload expressions evaluate in source order. Indexing evaluates the collection before the index,
and member access evaluates its receiver before accessing the member. A branch evaluates its
subject once, then considers tests and guards in source order. If an evaluation fails or transfers
control, operands that follow it are not evaluated.

A partial application evaluates its callee first and then each supplied, non-placeholder operand
from left to right when the partial is created. Those results are captured once. Copy, ownership
transfer, and borrowing begin at creation according to ordinary closure-capture rules. A later
invocation evaluates only the placeholder arguments and the resulting call; it does not reevaluate
the captured operands.

**S-10 — Selection and repetition.** A subject branch evaluates its subject once. Branch arms
are considered in source order; the first matching arm executes and does not fall through.
Enum coverage must account for refutable payload patterns, not merely mention each case name.
A branch-arm block that falls through must supply a final value expression.

`loop` repeats its body. `break` exits the nearest enclosing loop and `continue` begins that loop's
next iteration. Neither targets a branch. Both are invalid outside loops. `return` leaves the
enclosing function; otherwise its final result expression determines its result. Initialization,
move, and loan obligations apply to every reachable successor, including back-edges and early exits.

## 5. Ownership and transfer

**S-11 — Availability.** Reading, borrowing, or projecting through an unavailable place is invalid.
Moving ownership-bearing data transfers it and makes the source unavailable. Reassigning a complete
local can reinitialize it; using a field through an unavailable parent cannot. A partial move
invalidates the moved part and overlapping uses, not provably disjoint initialized siblings.

The built-in copy types are `()`, `Bool`, `Int`, `Float`, `Byte`, `CodePoint`, and `Symbol`.
Copying one preserves the source. There is no general user-defined `Copy` or `Clone` protocol in
this baseline, and shared runtime representations do not confer source-level copy permission.

**S-12 — Calls.** Parameters borrow by default. A consuming parameter receives ownership; callers
must use `move` when transferring an existing ownership-bearing place. Copy values and fresh owned
temporaries do not need that marker. Borrowed parameters with mutation effects access the caller's
place rather than an independently detached callee copy. Consuming an adapted structural view
consumes its underlying value, including ownership not exposed by the narrower contract.

Callable types preserve consumption positionally, for example `func(consume String) -> ()`.
Direct calls, methods, closures, and indirect calls must respect the same parameter contract.

## 6. Loans, groups, and invalidation

**S-13 — Validity.** A loan is usable only while its complete origin chain remains live and has
not been invalidated. A reference observes its live place, not a snapshot; it must not silently
retarget to replacement storage. A derived loan cannot outlive any possible parent loan.
Validity does not itself grant mutation permission.

Borrower provenance must survive aggregates, indexing, pattern extraction, branches, closure
capture, generic substitution, and structural or callable adaptation. Returning a borrower must
expose its dependence on input groups. A borrower of frame-local storage cannot escape its frame.
An owner must not contain a borrower whose lifetime requires that same owner to keep itself alive.

**S-14 — Invalidation and precision.** Consumption, destruction, overlapping replacement, and
structural mutation invalidate affected loans. Reshaping list storage invalidates its indexed
loans even if an allocator happens not to relocate the buffer. Ordinary permitted mutation through
a valid place is distinct from replacing an origin on which a derived loan depends.

Different named fields and different constant indices can be disjoint. Dynamic indices overlap
unless a live fact establishes otherwise. Mutation of a predicate operand invalidates reasoning
based on its old value. Checks may conservatively reject cases outside supported reasoning, but
must not accept a use that is invalid on a feasible path. Loan demand can end at last use rather
than at lexical scope end; a fresh loan after mutation does not revive the old loan identity.

## 7. Effects and closures

**S-15 — Permissions.** Access permissions are ordered `read < mut < reshape`; `consume` is
separate. A permission on a root covers its descendants, not vice versa. `suspend` is a callable
property, not a group permission. `mut owner` permits extracting and replacing an owned descendant,
but does not permit consuming the owner itself.

Inferred effects are the callable's contract. An explicit contract must cover the body's required
effects; over-declaration may warn, while missing permission is an error. Calls substitute actual
groups and preserve child paths. Representation erasure must preserve these upper bounds.

**S-16 — Capture and invocation.** An implicit read capture copies a built-in copy value and
moves an ownership-bearing value. Explicit `[copy ...]`, `[move ...]`, and `[ref ...]` select the
corresponding operation; explicit copy is invalid for non-copy data. Escape does not independently
change capture mode. A borrowed escaping closure must expose its input-group dependencies.

Closure effects occur when invoked, not simply when the closure is constructed. Construction still
performs its capture operations. Multiple reference-capturing closures may coexist under valid
loan and effect contracts; Foster does not impose Rust-style exclusive mutable-reference types.
A call must reject unavailable closure storage or required invalid captures. Partial application
preserves the resulting callable's ownership and effect requirements.

## 8. Outcomes and lifetime boundaries

**S-17 — Recoverable errors.** `Result<T, E>` is ordinary tagged data. `try` evaluates one Result
operand once: `Ok(value)` yields the payload; `Error(error)` returns an error from the enclosing
function. That function must return `Result<U, E>` with the same error type. `try` does not catch
assertions, bounds failures, arithmetic failures, or arbitrary host exceptions.

**S-18 — Failure and cleanup.** A failed assertion stops its invocation without executing its
ordinary continuation. Checked arithmetic and bounds failures likewise are language failures,
not permission for invalid memory access. Exact diagnostic prose is not a portable semantic value.

Borrowed non-place expressions have full-expression temporary storage. A full expression is the
principal expression of one source statement: a binding initializer, assignment, expression
statement, assertion, or unguarded transfer. Calls, their arguments, aggregate initializers,
branch subjects, tests, guards and selected bodies nested inside that expression share its boundary.
An assignment's right side and subsequently evaluated destination also share one boundary.

A postfix `return`, `break`, or `continue` guard is a separate full expression because a false guard
continues in the current function; a taken `return` then evaluates its result as another full
expression. Temporaries are destroyed in reverse creation order at the boundary. A taken transfer,
`try` error, assertion failure, or other modeled failure destroys every active temporary before
leaving its current control path. A value successfully moved into a destination or closure
environment is no longer owned by its temporary.
Owned function storage is subject to the destruction rules in the ownership model; borrowed
parameters do not destroy their caller's storage. Earlier last-use disposal is allowed only when
observably equivalent. This draft does not introduce user-defined destructors or promise identical
allocator reclamation timing across backends. VM failure teardown and native cleanup gaps are
distinguished in section 12.

## 9. Remote execution

**S-19 — Messages and outcomes.** An owned remote receiver retains its state between invocations.
Messages to one worker execute in FIFO mailbox order; this is not a total order across workers
or a scheduling guarantee among concurrent producers. A remote call returns a future without
awaiting its completion. `await` suspends the caller until its outcome is available, and consumes
the future once. The accepted lifecycle contract is specified by [remote rules R-01–R-09](remote-semantics.md).
The owning remote value controls worker lifetime. Owner destruction cancels running and queued
requests and resolves outstanding futures with shutdown errors; futures do not keep the worker
alive. Statically established outstanding requests at owner exit must be rejected. Remote execution
failure is contained, terminal for that worker, and resolves outstanding and subsequent requests
with errors. The planned awaited outcome is `Result<T, RemoteError>`; that API and the new lifetime
checks are pending implementation. Existing VM failure delivery does not establish this entire
contract, and native process-fatal failures are a conformance gap.

**S-20 — Remote loans.** Owned messages transfer only supported transferable values. Ordinary
explicit references, closures, and futures cannot be transferred as owned mailbox arguments in
this baseline. Borrow-mode arguments instead use call-scoped read-only capabilities. Queued work
retains the capability; shared access covers the invocation and ends when the invocation returns,
not when its future is awaited. Borrowed arguments cannot be mutated or retained as owned actor data.

`remote ref value` is a persistent read-only capability over live owner storage. Owner method
mutation uses exclusive access and commits atomically with respect to those remote reads. This
is not a blanket guarantee for unsynchronized mutation of arbitrary shared state. Loans across
suspension must remain live and valid. Cancellation must preserve storage needed by executing code
and release loans safely. Fairness, deadlock freedom, host interruption latency, and global shutdown
ordering remain open; owner-driven cancellation instead of draining is now an accepted decision.

## 10. Host and library boundary

**S-21 — Authority and policy.** Resource identifiers identify resources; they do not implicitly
grant access. Host operations execute through the supplied host context and capability interfaces.
Recoverable host errors use the documented typed results. Host adapters must preserve the ownership
and lifetime contract of every argument and result.

Collection algorithms, text encoding/validation policy, and other library decisions remain Foster
code. Low-level allocation, checked storage access, scalar/representation operations, and host
operations may be primitives. Primitive replacement or inlining must preserve the same values,
effects, failures, and borrower dependencies.

## 11. Backend conformance and witnesses

**S-22 — Observable equivalence.** For the same supported program and host inputs, VM and native
execution, with and without optimization, must preserve results, observable mutations, required
effect ordering, and specified failure propagation. Concurrent executions may differ only within
the scheduling freedom above. Register numbers, object addresses, allocation counts, instruction
selection, and asymptotic performance are not semantic results. They must not be used to excuse
invalid ownership or changed observable behavior. Unsupported native cases should be diagnosed;
successful type checking alone does not certify complete native support.

Existing witnesses provide regression evidence, not a proof or exhaustive conformance claim:

| Rules | Existing witness location |
| --- | --- |
| S-03–S-05 | [language tests](../tests/language.rs), [CLI tests](../tests/cli.rs) |
| S-06–S-08 | [backend parity](../tests/backend_parity.rs), [library algorithm fixture](../tests/fixtures/programs/library_algorithms.fos), [library sources/tests](../library/) |
| S-09–S-10 | [language tests](../tests/language.rs), [portable tests](../tests/foster/), [control-flow lowering](../src/control_flow.rs) |
| S-11–S-16 | [ownership tests](../tests/language_ownership.rs), [rule-indexed witnesses](../tests/ownership_soundness.rs), [reference model](../src/ownership/model.rs) |
| S-17–S-18 | [ownership tests](../tests/language_ownership.rs), [backend parity](../tests/backend_parity.rs) |
| S-19–S-20 | [remote ownership tests](../tests/language_ownership.rs), [native tests](../tests/native.rs); failure gap below |
| S-21 | [host tests](../tests/core_host.rs), [intrinsic registry](../src/intrinsics/registry.rs) |
| S-22 | [backend parity](../tests/backend_parity.rs), [native tests](../tests/native.rs) |

For a changed rule, add a smallest accepted and rejected witness where meaningful, plus runtime
parity coverage for supported executable behavior. A known backend violation must be recorded
explicitly rather than hidden by weakening an assertion. See [testing](testing.md) and
[ownership verification](ownership-verification.md) for execution commands and test organization.

## 12. Open decisions and implementation gaps

These entries distinguish missing language decisions from missing implementation work:

- **G-02 — User-defined accessors (deferred feature):** stored fields, compiler-provided computed
  values, and methods have distinct semantics. A future property protocol must preserve those
  categories and cannot silently introduce implicit getter-modify-setter write-back.
- **G-03 — Remote failure implementation:** native worker failures currently terminate the process
  rather than populating their futures, even for unawaited calls. Sticky failure and the planned
  typed remote outcomes require implementation and cross-backend witnesses.
- **G-04 — Reclamation (implementation gap/open design):** native strings and argument containers
  participate in managed destruction. Full exceptional cleanup, user-visible resource destruction ordering,
  and a general destructor protocol are not uniformly established. See [native gaps](native.md#known-runtime-correctness-gaps).
- **G-05 — Generic sequence execution (implementation gap):** native `SequenceIterator` cannot
  yet resolve all erased sequence storage members. Head/rest adapters can copy tails; neither
  structural conformance nor slicing promises zero-copy traversal.
- **G-06 — Scoped remote lifetime (accepted, not implemented):** implement owner-exit cancellation,
  completion tracking, and diagnostics under [the remote contract](remote-semantics.md). Dropping a
  future does not discharge its request. Cross-worker scheduling, liveness, host interruption, and
  process-wide shutdown ordering remain open.
- **G-07 — Analysis precision (implementation limit):** indirect callable-result provenance and
  richer path facts remain conservative. Better precision may accept more safe programs but must
  not discard a real origin or lifetime dependency.

Resolving an open decision requires a documented rule, implementation, and conformance tests.
Fixing an implementation violation should restore the contract without redefining the violating
behavior as valid Foster semantics.
