# Foster Roadmap

This document collects work that is not part of the implemented language described in
[Language Design](language-design.md). It records directions and unresolved design areas, not
compatibility promises or a release schedule. Items move into the language design document only
after the compiler, runtime, and tests agree on their behavior.

## Implemented baseline

The current baseline includes:

- Records, transparent type aliases, `&` composition, method-level generic requirements, and
  ordered default implementations. The rightmost compatible default wins, with a concrete type's
  own implementation taking final precedence. Alternatives belong to enums; `type` definitions
  no longer support union alternatives.
- Compile-time module constants, enum and scalar literal patterns, explicit user-defined `Copy`,
  and automatic `Drop`/`deinit` at ownership end, including cleanup after modeled failures.
- TCP listeners and connections implement `Drop`; consuming `close()` remains available to report
  errors. Indexed accesses and abrupt host failures have explicit ownership-MIR cleanup successors,
  with synchronous call failures propagating through the caller's cleanup path.
- Bounded reasoning about compound Boolean conditions and result-provenance summaries through
  supported direct and indirect calls. Unknown targets and computed predicates remain conservative.
- Foster-written HashMap and HashSet collections composing storage-free Map and Set contracts,
  insertion-ordered ListMap and ListSet implementations, a per-machine host context, the
  exact/civil/zoned `std.time` taxonomy, and the
  source/generator/distribution/secure/sequence `std.random` taxonomy.
- Cranelift execution of supported records, enums, generic collections, strings and bytes,
  closures and callable contracts, references, structural dispatch, remote objects, and futures.
  Native host services include filesystem operations, clocks, entropy, and TCP. Remote failure
  containment, scoped owner cancellation, and managed-value cleanup have cross-backend tests.

These are tested capabilities, not a claim that every possible program is supported. The items
below describe remaining extensions. See the [semantic specification](semantics.md),
[analysis precision](analysis-precision.md), and [native compilation](native.md) for exact limits.

## Strengthen the existing model

The immediate priority is to make the ownership, group, effect, and structural-contract model more
general without weakening its current guarantees.

- Generalize path-correlated loan states to computed predicates, computed-value comparisons, and
  richer range facts. Bounded `&&`, `||`, and `not` reasoning over supported stable facts is
  already implemented; arithmetic equivalence and stored predicate formulas remain conservative.
- Extend result provenance and projected-reference invalidation precision for dynamically selected
  callables, opaque factories, hidden captured borrowers, and effectful calls. Direct fixed-point
  summaries, supported indirect-target summaries, and checked type/group fallbacks already exist.
- Define effect-polymorphic callable contracts beyond the implemented concrete effect annotations.
- Decide whether public APIs require explicit annotations beyond the checks already performed by
  inference.
- Define explicit re-exports while preserving the filesystem-derived module model and declarations
  that are private by default.
- Validate partial inherited defaults on abstract library contracts before adding a shared
  `Collection.empty?` body. Native representation selection must distinguish an abstract contract
  with some defaults from a concrete implementation.

The focused [ownership](ownership.md), [closure](closures.md), and
[effect derivation](effect-derivation.md) documents contain the detailed constraints behind this
work.

## Self-hosting preparation

Writing the compiler in Foster is a new direction to develop in stages. The Rust compiler remains
the bootstrap implementation; a self-hosted compiler and the arena API below are not implemented.

- Prototype an append-only `Arena<T>` and typed `ArenaId<T>` in Foster using existing list storage.
  The arena owns its nodes; graph edges store IDs. Define insertion, borrowed lookup, replacement,
  ID copying, invalid-ID handling, and protection against using an ID with the wrong arena.
- Validate noncopyable-node access, ownership effects, growth, and cleanup in both VM and native
  execution. IDs should survive insertion; borrowed element references must obey existing reshape
  invalidation rules. The initial design omits deletion and slot reuse. No new VM instruction is
  assumed necessary for this ID-based design; verify that with the prototype.
- Use arenas and the existing HashMap/HashSet to prototype compiler nodes, symbol tables, and
  analysis side tables. Identify concrete language or library blockers through these workloads
  before deciding the scope and order of compiler passes to port.
- Define staged bootstrap validation: build the Foster compiler with the Rust compiler, use that
  compiler to rebuild itself, and check successive stages against the language and backend
  conformance suites. Specify the output target and runtime/toolchain dependencies for each stage.

`Cell`-like interior mutability remains a separate design question, not a prerequisite for the
ID-based arena. Mutating through shared access would need explicit group, effect, and remote-access
rules. An arena that preserves direct references across allocation would also need a stronger
storage and borrowing contract than the proposed list-backed arena.

## Compiler architecture

Checked phase orchestration now lives behind `compiler::Compiler` rather than inside HIR, and one
intrinsic registry owns builtin identities, source keys, declaring modules, host classification,
and stable bytecode tags. The remaining structural work should preserve that dependency direction:

- Introduce a reusable compiler session and source database so the language server can request
  checked snapshots without owning a parallel whole-package compilation policy.
- Extract shared source-signature, type, and effect presentation used by generated documentation
  and language-server features.
- Separate package discovery, module graphs, bootstrap-library selection, validation, caching, and
  source diagnostics into focused modules.
- Continue separating ownership region analyses, VM value/place machinery, execution, and native
  lowering by phase and state ownership. Existing compiler orchestration and native support modules
  already provide part of this separation.
- Narrow the public crate surface into supported compiler, tooling, and runtime APIs before the
  bootstrap implementation reaches a stable release.

## Complete everyday language facilities

- Add record and list patterns, pattern-branch guards, and more precise exhaustiveness checking for literal
  domains. Enum cases, nested enum payloads, bindings, wildcards, and scalar literal patterns are
  already implemented.
- Design functional record updates.
- Design distinct nominal wrapper declarations beyond the implemented transparent aliases.
- Extend compile-time constant expressions beyond primitive literals, constant references,
  unary-negative numeric literals, and recursively constant homogeneous lists while retaining
  declaration-only module bodies and avoiding observable module initialization order.
- Decide whether typed error effects or explicit error-conversion protocols should complement the
  implemented `try` propagation over `Result<T, E>` values.
- Decide whether aggregate copy derivation or additional clone conveniences should extend the
  implemented explicit structural `Copy` capability. User-defined copying already works; implicit
  copying remains limited to built-in copy values.
- Decide the user-facing task, synchronization, `Send`, and `Share` model around the existing remote
  object and virtual-thread runtime. Extend conservative completion proofs across function
  boundaries and resolve scheduling, liveness, host interruption, and process shutdown ordering.

## Runtime and platform

- Generalize the implemented per-machine `HostContext` into a pluggable host-provider boundary and
  expose explicit filesystem, network, wall-clock, and monotonic-clock capability tokens suitable
  for production, sandboxed, deterministic, and in-memory hosts. These would complement the
  existing structural resource and `Clock<T>` contracts.
- Supply a versioned IANA time-zone database behind `TimeZoneDatabase`, including aliases,
  transition lookup, a deliberate system-zone API, and reproducible tzdata selection. The current
  `TimeZone` contract, fixed-offset implementation, and explicit unique/ambiguous/skipped local
  resolution are already implemented.
- Extend the time modules with unit-selected `until`/`since`, rounding and balancing, calendar-span
  difference, transition introspection, reusable format patterns, and locale providers. Add
  non-ISO calendar implementations behind `Calendar` only when their era and month semantics have
  explicit contracts; the ISO calendar remains the portable baseline.
- Extend the implemented random distributions beyond uniform integers/floats, Bernoulli, and
  weighted indices when concrete use cases justify normal, exponential, or other models. Add a
  stronger named portable generator only with a frozen algorithm, seed mapping, output sequence,
  and cross-target compatibility suite; `LehmerRandom` remains the current portable baseline.
- Add socket readiness and TLS support to the I/O boundary, and extend resource providers beyond
  the existing filesystem and TCP implementations.
- Refine scalar inference for dynamically erased values in the shared SSA verifier. The complete VM
  instruction surface now seals through shared SSA and de-SSA with deterministic record, enum,
  closure, and reference layouts; erased heterogeneous joins retain an explicit opaque type until
  the bytecode ownership/type verifier resolves their concrete flow state.
- Implement resumable suspension/state-machine lowering; native `await` currently blocks.
  Extend native runtime services and cross-target object output, and address additional unsupported
  cases as conformance tests identify them. Existing text/bytes, erased values, callable, remote,
  and future lowering should be extended from its tested baseline, with the VM as semantic reference.
- Compact the bytecode encoding after its instruction model is stable.

## Longer-horizon questions

Nominal inheritance beyond the implemented structural composition and defaults, higher-kinded
types, arbitrary type-level programming, macros, operator overloading,
reflection, and a stable ABI are deliberately uncommitted. They should be evaluated only when a
concrete use case shows how they interact with structural typing, ownership, groups, and effects.

LLVM is also optional rather than planned; native backend work currently uses Cranelift.
