# Foster examples

`iteration.fos` demonstrates structural `Iterable<T>` conformance, a mutable `Iterator<T>`
cursor, and `Option<T>` exhaustion through dynamically dispatched contract methods. The Pima
`foreach.fos` and `showcase.fos` ports demonstrate terminal iterator consumers and lazy filtering.

`value_contracts.fos` demonstrates composed equality, total-ordering, and hashing contracts.

`live_inventory_pipeline.fos` is the flagship end-to-end example. It models
a concurrent inventory audit with owned remote actors, futures, borrowed remote
arguments, a persistent read-only loan, atomic owner mutation, records, and
ownership effects. It uses normal Foster source with inferred effects; compiler
documentation, core interfaces, and focused tests show explicit bracketed contracts.

Run it with:

```console
cargo run --bin foster -- run examples/live_inventory_pipeline.fos
```

The program deterministically returns `1242`:

- `1000` for one healthy audit
- `200` for two alerting audits
- `30` weighted shortage points
- `12` items observed through the live remote inventory view

The smaller programs under `pima/` focus on individual language features.

`type_composition.fos` demonstrates declaration-side composition, intersection parameters, and
static duck typing without a wrapper or runtime conversion:

```console
cargo run --bin foster -- run examples/type_composition.fos
```
`collections.fos` demonstrates the shared `Collection<T>` contract, a Foster-written `Set<T>`,
the generic `Range<T>` sequence view, and borrowed `.iterator` creation.

`bytes.fos` demonstrates bounded `Byte` values, compact immutable `Bytes`, UTF-8 and hexadecimal
boundaries, and mutation followed by ownership-transferring `ByteBuffer.freeze`.

`streams.fos` demonstrates generic `Reader<E>` and `Writer<E>` conformance, partial writes,
clean EOF, and the Foster-written `stream.copy` algorithm.

`linked_list.fos` implements an owned generic linked list as a recursive variant, including
constant-time prepend/pop, reverse, map, fold, and conversion to the built-in list type.
