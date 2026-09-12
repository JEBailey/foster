# Library verification and limits

See [the library reference](core-library.md) and [contract audit](library-contract-audit.md)
for public APIs, structural contracts, and ownership semantics.

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

## Remaining boundaries

- A shared body for `Collection.empty?` needs validation across all standard collection
  implementations before adoption. Each implementation currently supplies its own method.
- Host operations retain conservative mutation effects because reading an integer handle can
  mutate the external socket or random source. Some TOML helpers also retain broader private effect
  annotations. The compiler reports these as warnings, not missing type information.
- List operations that preserve their source while extracting owned elements still require `Copy`
  for those elements. Borrowing operations and consuming collection lookup have different ownership
  contracts; they should not be made interchangeable by implicit copying.
- `Arena`, shared interior mutability, effect-polymorphic callbacks, and re-exports remain roadmap
  work.

See [the library reference](core-library.md), [hash collections](hash-collections.md), and
[the roadmap](roadmap.md) for the maintained API and remaining work.
