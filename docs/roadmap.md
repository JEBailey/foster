# Foster Roadmap

This document collects work that is not part of the implemented language described in
[Language Design](language-design.md). It records directions and unresolved design areas, not
compatibility promises or a release schedule. Items move into the language design document only
after the compiler, runtime, and tests agree on their behavior.

The implemented baseline already includes transparent type aliases, compile-time module constants,
enum and literal patterns, a per-machine host context, the exact/civil/zoned `std.time` taxonomy,
the source/generator/distribution/secure/sequence `std.random` taxonomy, and the initial Cranelift
backend for scalar values, strings, and read-only command arguments. The
items below describe the remaining extensions to those facilities rather than proposing them from
scratch.

## Strengthen the existing model

The immediate priority is to make the ownership, group, effect, and structural-contract model more
general without weakening its current guarantees.

- Generalize path-correlated loan states beyond direct stable predicates to compound conditions,
  computed values, and richer range facts. Stable Boolean places, enum discriminants, and direct
  scalar comparisons are already correlated.
- Generalize interprocedural `reshape` metadata and projected-reference invalidation beyond the
  implemented fixed-point summaries for direct calls, including equivalent provenance through
  erased callable contracts.
- Define method-level generic requirements, default contract implementations, and
  effect-polymorphic callable contracts.
- Decide whether public APIs require explicit annotations beyond the checks already performed by
  inference.
- Define explicit re-exports while preserving the filesystem-derived module model and declarations
  that are private by default.
- Finish explicit ownership-MIR failure edges for dynamic runtime errors, then define resource
  destructors, destruction order, and unwinding behavior before a stable release.

The focused [ownership](ownership.md), [closure](closures.md), and
[effect derivation](effect-derivation.md) documents contain the detailed constraints behind this
work.

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
- Split the ownership region analyses, VM value/place machinery, execution machine, and native backend by
  phase and state ownership while retaining their current tested semantics.
- Narrow the public crate surface into supported compiler, tooling, and runtime APIs before the
  bootstrap implementation reaches a stable release.

## Complete everyday language facilities

- Add record and list patterns, branch guards, and more precise exhaustiveness checking for literal
  domains. Enum cases, nested enum payloads, bindings, wildcards, and scalar literal patterns are
  already implemented.
- Design functional record updates.
- Design distinct nominal wrapper declarations beyond the implemented transparent aliases.
- Extend compile-time constant expressions beyond primitive literals, constant references,
  unary-negative numeric literals, and recursively constant homogeneous lists while retaining
  declaration-only module bodies and avoiding observable module initialization order.
- Decide whether typed error effects or explicit error-conversion protocols should complement the
  implemented `try` propagation over `Result<T, E>` values.
- Define aggregate copy/clone contracts; today copy behavior is limited to built-in copy values.
- Decide the user-facing task, synchronization, `Send`, and `Share` model around the existing remote
  object and virtual-thread runtime.

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
  the current whole-file and TCP implementations.
- Refine scalar inference for dynamically erased values in the shared SSA verifier. The complete VM
  instruction surface now seals through shared SSA and de-SSA with deterministic record, enum,
  closure, and reference layouts; erased heterogeneous joins retain an explicit opaque type until
  the bytecode ownership/type verifier resolves their concrete flow state.
- Complete instruction lowering for the target-specific String/Symbol/bytes, erased box,
  heterogeneous callable, remote, and future layouts now materialized by native
  specialization. Then extend native runtime services and cross-target object output while
  retaining the register VM as the semantic reference.
- Compact the bytecode encoding after its instruction model is stable.

## Longer-horizon questions

Inheritance, higher-kinded types, arbitrary type-level programming, macros, operator overloading,
reflection, and a stable ABI are deliberately uncommitted. They should be evaluated only when a
concrete use case shows how they interact with structural typing, ownership, groups, and effects.

LLVM is also optional rather than planned; native backend work currently uses Cranelift.
