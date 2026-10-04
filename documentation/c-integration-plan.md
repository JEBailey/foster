# C integration implementation plan

Status: the Windows x86-64 bridge and owning-resource foundation is implemented;
see [C integration](c-integration.md) for the supported contract and limitations.
The milestones below remain the broader roadmap. Borrowed views, callbacks,
general ownership-transfer parameters, and package bundling are not implemented.

## Objective and existing foundation

Let Foster applications use C libraries through ordinary Foster APIs whose ownership,
cleanup, borrowing, and mutation contracts remain meaningful on both VM and native
backends. Wrapper authors explicitly describe behavior that cannot be inferred from C.

Reuse the current language:

- Single ownership, consuming parameters, and explicit `move` transfer responsibility.
- `deinit` releases resources at ownership end; fallible closing remains a separate API.
- Groups track borrow origins; `read`, `mut`, and `reshape` describe access to those origins.
- Standalone named scopes such as `:request { ... }` visibly delimit local resource
  lifetimes. They do not become arenas, runtime ownership groups, or implicit destinations
  for resources created in callees.
- Private wrapper implementation details stay behind public Foster methods.

The existing [host-provider boundary](host-providers.md) supplies Rust implementations
of selected platform services. The [native runtime ABI](native.md) connects generated
code to Foster's own runtime. Neither currently provides general C imports. Keep these
responsibilities distinct rather than adding every third-party library to `HostProvider`
or the built-in intrinsic registry.

## Proposed architecture

```text
Foster application
    |
    | ordinary owned values, borrows, effects, Result
    v
Managed wrapper written in Foster
    |
    | explicit trusted foreign declarations and conversions
    v
Checked foreign-call descriptor + generated C bridge
    |                                      |
    v                                      v
VM bridge dispatch                    Native bridge calls
    \                                      /
     +------------- C library ------------+
```

Use one foreign-call descriptor for symbol identity, target ABI, argument/result wire
types, pointer direction, nullability, ownership, borrow provenance, and effects.
Generate backend adapters from that descriptor so they cannot silently disagree.
The compiler checks descriptor consistency and Foster callers; it cannot prove that
the foreign implementation honors the contract.

The proposed first implementation uses generated C bridges. The target C compiler
checks included declarations and handles the foreign calling convention; bridges expose
a small, versioned Foster wire interface. VM execution loads a bridge library, while
native builds link equivalent generated adapters. Pin the bridge ABI before implementation.
Direct native calls and a general dynamic-call engine are later optimizations, not two
independent implementations to build initially.

Compilation requires an explicitly configured target C toolchain. Begin with Windows
x86-64, matching the current development environment; validate a Linux x86-64 target
before claiming cross-platform support. Additional targets require ABI conformance tests.

## Milestone 0: settle the boundary contract

Deliver a short language/ABI specification and compile-pass/compile-fail examples before
adding source syntax. Final spellings for foreign declarations, raw pointers, and trusted
operations are deliberately undecided here.

Specify these obligations:

| Concern | Required decision |
| --- | --- |
| Scalar representation | Define C integer widths, signedness, floating types, pointer width, and checked Foster conversions. Do not equate Foster `Int` with C `int` or `long`. |
| Raw access | Restrict foreign invocation and pointer construction/dereference to a visible trusted boundary; safe code cannot manufacture a resource token from an integer or a structurally similar record. |
| Ownership | Distinguish owned results, borrowed results, call-only input pointers, output parameters, and transfer to C. Specify who releases partially initialized outputs. |
| Conditional transfer | If C adopts only on success, return ownership on failure or retain it in the wrapper until success; a consume annotation alone is insufficient. |
| Memory | Define null-plus-length rules, bounds, alignment, pointer provenance, read/write access, encoding, and the exact lifetime of bridge temporaries. |
| Destruction | Define explicit-close failure states and automatic cleanup without double release or silently losing a still-live resource. Preserve existing failure-cleanup precedence. |
| Execution | Specify same-thread synchronous calls initially. Foreign handles are non-transferable to remote workers until their thread contract is represented and checked. |
| Failure crossing | Do not allow foreign exceptions or nonlocal jumps across Foster frames. Define how bridge failures become Foster failures or typed results. |

Acceptance: each rule has an executable fixture planned, and unsupported signatures have
specific diagnostics. Existing Foster ownership rules remain intact.

## Milestone 1: a complete minimal call path

Build a tiny repository-owned C fixture with scalar operations, an opaque allocation,
reads/writes, destruction counters, and injected failure paths. It is the primary
conformance fixture, independent of a third-party library.

Implement:

- Foreign declaration parsing, typed HIR descriptors, and descriptor validation.
- Initial wire types: void/unit, selected explicit-width integers and floats, opaque
  pointers, and pointer-plus-length buffers handled by bridge helpers.
- Generated bridge compilation, symbol resolution, VM dispatch, and native linking.
- Typed diagnostics for missing symbols, unsupported calling conventions, incompatible
  targets, missing toolchains, and unavailable libraries.
- Manifest configuration for include paths, library paths/names, target toolchain,
  and explicit build inputs. Invoke tools with argument arrays, not shell fragments.
- Cache identity covering generated bridge source, transitive headers, target, toolchain,
  flags, and library identity. Specify how runtime dependencies are located.
- Foreign metadata in compiled libraries and bytecode, with format/version validation.
  Loading bytecode must not silently compile untrusted native source or resolve libraries
  from arbitrary working-directory search paths.

Use copied input/output buffers first. No retained pointers into ordinary Foster
`String`, `Bytes`, or collection storage. Physical sharing and copy-on-write do not
establish stable C addresses.

Acceptance: the same fixture runs under VM and native, with optimization on and off;
scalar boundaries and failure diagnostics agree. Checking source does not execute C.

## Milestone 2: safe resource wrappers — first usable release

Wrap the fixture in Foster using an unforgeable foreign resource token, private fields,
ordinary methods, `deinit`, and `Result`. A raw pointer's ability to be copied must not
duplicate the owning token or its cleanup obligation. Define behavior for structural
adaptation, explicit copy attempts, moved resources, and internal runtime storage sharing.

Cover acquisition, mutation, consuming adoption, explicit close, and automatic cleanup.
For fallible release, classify whether an error leaves the resource live or destroys it;
the wrapper's state transition must follow that API's actual behavior. Require an explicit
policy for destructor errors rather than assuming every destructor succeeds.

Acceptance:

- Exactly one release after normal scope exit, nested named scopes, replacement, moves,
  early return, `try`, loop transfer, and ordinary language failure.
- Failed acquisition releases any partially created native resource.
- Adoption on success and rejection on failure preserve exactly one owner.
- Attempts to forge, copy, reuse, or remotely transfer unsupported resources are rejected.
- Copied strings validate UTF-8 and handle embedded NUL according to the declared API.

This is the MVP: useful safe wrappers around synchronous C APIs using opaque resources
and owned data. Do not advertise borrowed foreign references or retained callbacks yet.

## Milestone 3: foreign borrow origins and invalidation

Extend the descriptor with abstract storage domains for opaque resources, such as buffer
elements or a statement's current row. These domains are not required to correspond to
visible Foster fields. Tie each returned view to its owner and domain, and map operations
that invalidate it to checked effects. Determine whether existing `reshape` rules suffice
or an explicit invalidation effect is necessary before choosing syntax.

Implement a foreign-view representation carrying the required address, bounds, origin,
and access contract. Do not reinterpret an arbitrary C address as a normal Foster managed
record/reference: the VM and native backend must both understand how to access it.
Start with read-only byte views and explicit owned copies; add mutable views only after
aliasing and conversion rules have dedicated tests.

Add an owned child-resource dependency: an object can own its C handle while borrowing
the parent needed to use and destroy it. Model this dependency through moves, aggregate
storage, generic calls, compiled interfaces, and cleanup. Reverse local binding order
alone cannot establish correctness for every aggregate or escaped child. Where the
current destructor order cannot satisfy dependencies, reject the shape or require an
explicit owning wrapper until the destruction model is extended.

Acceptance: a resizable C buffer rejects stale views after growth/replacement/free,
permits valid reads and independent-domain mutation, and preserves provenance through
records, collections, closures, and indirect calls. A child cannot outlive its parent.
An operation cannot be classified read-only merely because its C pointer is `const`.

## Milestone 4: SQLite as the first substantial wrapper

Build a deliberately narrow package: database open/close, prepare/finalize, parameter
binding with explicit copy/retention policy, stepping, and owned column extraction.
Keep the raw binding module separate from the safe public API. Pin the tested library
version and record build inputs; no implicit download during ordinary compilation.

Use SQLite to validate contracts rather than assume it is a trivial handle example:

- Statements depend on their database. Closing a database with outstanding statements
  has API-specific behavior; choose and document the wrapper's close policy.
  See [SQLite connection closing](https://www.sqlite.org/c3ref/close.html).
- Column views may expire on step, reset, finalize, or certain conversions, not only
  database destruction. Add zero-copy views only after milestone 3 can represent those
  effects. See [SQLite column values](https://www.sqlite.org/c3ref/column_blob.html).
- Finalization can report an execution error while destroying the statement; do not
  retry destruction on that error. See [SQLite finalization](https://www.sqlite.org/c3ref/finalize.html).

Acceptance: a real query completes inside a standalone named scope with deterministic
cleanup. Tests cover failed preparation, early exit, close policy, child dependencies,
copied result escape, and stale-view rejection. Both backends pass the same suite.

## Milestone 5: callbacks and broader C coverage

Treat callback support as a separate release gate:

1. Synchronous, non-retained callbacks with call-scoped userdata and failure containment.
2. Retained callbacks owned by an explicit registration token. Destruction unregisters
   and establishes that no callback remains in flight before freeing closure storage.
3. Reentrant and foreign-thread callbacks only after thread affinity, runtime entry,
   synchronization, scheduling, and cancellation have explicit contracts.

Group lifetime annotations alone do not establish callback quiescence or thread safety.
If an API cannot provide safe teardown, require a different wrapper strategy rather
than freeing userdata optimistically. Do not unwind through C on a Foster failure.

After the core milestones, prioritize C struct layout and by-value aggregates, function
pointers, unions, bitfields, varargs, additional calling conventions, and header-driven
binding generation based on actual wrapper needs. Generated ABI declarations never
replace manually reviewed ownership/invalidation contracts. C++ integration is separate.

## Work areas and completion rules

| Area | Existing integration points |
| --- | --- |
| Syntax and tooling | `compiler/src/lexer.rs`, `compiler/src/parser/`, `compiler/src/ast.rs`, formatter, LSP, generated documentation |
| Static contract | `compiler/src/hir/`, `compiler/src/typecheck/`, `compiler/src/ownership/`, semantic and ownership specifications |
| Execution | `vm/src/`, `compiler/src/native/`, `runtime/src/`; introduce a shared foreign subsystem rather than duplicating contracts |
| Build and distribution | `src/project.rs`, `compiler/src/package/`, `compiler/src/library/`, native build/cache code, bytecode/package validation |
| Validation | C fixture, compile-pass/fail cases, backend parity, resource counters, compiled-library round trips |

Each milestone updates implementation, tests, diagnostics, tooling, and implemented
documentation together. Keep proposed syntax here until it works. Advance language,
artifact, or ABI versions where their contracts change; do not add compatibility shims.

Use the C fixture to test leaks, double releases, allocation failures, invalid output
states, pointer invalidation, and later callback teardown. Run memory instrumentation
where supported in addition to deterministic counters. A clean instrumentation run does
not prove arbitrary C implementations satisfy their declared contracts.

The first implementation task is milestone 0's descriptor and resource state-machine
specification, followed by the milestone 1 fixture and scalar vertical slice. Do not
begin with a general header importer or a large third-party wrapper.
