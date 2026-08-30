# Foster Roadmap

This document collects work that is not part of the implemented language described in
[Language Design](language-design.md). It records directions and unresolved design areas, not
compatibility promises or a release schedule. Items move into the language design document only
after the compiler, runtime, and tests agree on their behavior.

## Strengthen the existing model

The immediate priority is to make the ownership, group, effect, and structural-contract model more
general without weakening its current guarantees.

- Extend path-correlated loan states beyond stable boolean places to enum discriminants,
  comparisons, and dynamic-index facts.
- Generalize interprocedural `reshape` metadata and projected-reference invalidation.
- Define method-level generic requirements and default contract implementations.
- Decide whether public APIs require explicit annotations beyond the checks already performed by
  inference.
- Define explicit re-exports while preserving the filesystem-derived module model and declarations
  that are private by default.

The focused [ownership](ownership.md), [closure](closures.md), and
[effect derivation](effect-derivation.md) documents contain the detailed constraints behind this
work.

## Complete everyday language facilities

- Add record and list patterns, branch guards, and more precise exhaustiveness checking for literal
  domains.
- Design functional record updates.
- Decide on transparent aliases and distinct wrapper declarations.
- Improve compile-time constants while retaining declaration-only module bodies and avoiding
  observable module initialization order.
- Decide whether typed error effects or explicit error-conversion protocols should complement the
  implemented `try` propagation over `Result<T, E>` values.
- Define aggregate copy/clone contracts; today copy behavior is limited to built-in copy values.
- Decide the user-facing task, synchronization, `Send`, and `Share` model around the existing remote
  object and virtual-thread runtime.

## Runtime and platform

- Introduce explicit host-provider filesystem and network capability tokens suitable for
  production, sandboxed, and in-memory hosts; these would complement the existing structural
  resource contracts.
- Add socket readiness and TLS support to the I/O boundary, and extend resource providers beyond
  the current whole-file and TCP implementations.
- Stabilize a backend-neutral lowered IR.
- Extend the initial Cranelift AOT backend from primitive values to aggregates, closures, native
  runtime services, and cross-target object output while retaining the register VM as the semantic
  reference.
- Compact the bytecode encoding after its instruction model is stable.

## Longer-horizon questions

Inheritance, higher-kinded types, arbitrary type-level programming, macros, operator overloading,
reflection, and a stable ABI are deliberately uncommitted. They should be evaluated only when a
concrete use case shows how they interact with structural typing, ownership, groups, and effects.

LLVM is also optional rather than planned; native backend work currently uses Cranelift.
