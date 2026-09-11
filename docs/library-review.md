# Library review

This review covers all 51 Foster source modules under `library/core` and `library/std`, their public
type declarations and signatures, the embedded-library checks, and VM/native integration coverage.
It follows the current language: `&` composes contracts, implementations live in `impl` blocks,
transparent aliases have one target, and alternatives belong to enums.

## Changes

- `Map<K, V>` and `Set<T>` are storage-free contracts composing `Collection`. `ListMap` and `ListSet`
  provide the insertion-ordered implementations; `HashMap` and `HashSet` compose the same respective
  contracts. `Map.empty()`, `Set.empty()`, and `Set.from(values)` remain forwarding factories.
- Consuming updates return `self` in the contracts, preserving the concrete implementation when a
  function accepts a map or set abstractly. Concrete-only operations, such as `HashMap.remove` and
  `HashSet.values`, now have explicit declarations in their type bodies.
- List-backed map search, replacement, and extraction use indexed loops and snapshot iterators.
  Replacement retains the original key position, and extraction does not require `Copy` values.
- TCP wrapper construction uses qualified `Result.Ok` consistently with the rest of the library.
- The generic `Range<T>` module description no longer incorrectly restricts its views to integers.
- The library module inventory and collection documentation distinguish abstract contracts,
  concrete storage, ownership, and iteration-order guarantees.

Use `Map<K, V>` and `Set<T>` annotations for functions that accept either storage implementation.
Use `ListMap<K, V>` and `ListSet<T>` where insertion order or that concrete representation is part
of the API. Factory call sites using inferred types retain their existing behavior.

## Verification

`tests/library_contracts.rs` checks explicit public function annotations, required method signatures,
single-target aliases, storage-free foundational contracts, and hash-collection composition across
the source tree. The collection fixture exercises updates through abstract contracts, both storage
implementations, membership, consuming lookup, key/value extraction, deduplication, and insertion order in the VM and
native backend with optimization enabled and disabled.

The existing library suite checks portable behavior and documentation coverage. Host tests cover
filesystem, TCP, time, entropy, resources, and TOML; native and backend-parity suites check execution
and ownership behavior. These checks establish the supported tested surface, rather than proving
every generic instantiation or possible program correct.

The library integration suites can be run with:

```text
cargo test --release --offline --test backend_parity --test library_contracts --test core_host --test foster --test native -- --test-threads=4
```

TCP listeners and connections implement `Drop`, with explicit consuming `close()` for error
reporting. Native move-out projections detach shared storage, moved and consumed homes remain
empty across control-flow edges, and retain operations preserve partially moved null slots.
Socket-lifetime and native collection/iterator regressions cover automatic cleanup.

## Remaining boundaries

- `Collection.empty?` remains a required method supplied by each implementation. Adding a shared
  body exposed a native distinction between partial inherited defaults and concrete implementation
  storage; that compiler work is recorded in the roadmap.
- The `graphemes.fos` parity fixture has a type-checking failure when `std.sequence` is imported:
  `Sequence<String>` is expected where a `String` is supplied. This also occurs with the TCP
  declarations from before automatic cleanup and is independent of the native lifetime fixes.
- Host operations retain conservative mutation effects because reading an integer handle can
  mutate the external socket or random source. Some TOML helpers also retain broader private effect
  annotations. The compiler reports these as warnings, not missing type information.
- List operations that preserve their source while extracting owned elements still require `Copy`
  for those elements. Borrowing operations and consuming collection lookup have different ownership
  contracts; they should not be made interchangeable by implicit copying.
- `Arena`, shared interior mutability, effect-polymorphic callbacks, and re-exports remain roadmap
  work. This review does not introduce those language or runtime features.

See [the library reference](core-library.md), [hash collections](hash-collections.md), and
[the roadmap](roadmap.md) for the maintained API and remaining work.
