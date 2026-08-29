# Foster examples

The examples are executable documentation for Foster. Each program focuses on a language or
library capability and is exercised by the test suite with bytecode optimization both enabled and
disabled.

## End-to-end examples

`live_inventory_pipeline.fos` models a concurrent inventory audit with owned remote actors,
futures, borrowed remote arguments, a persistent read-only loan, atomic owner mutation, records,
and ownership effects. It deterministically returns `1242`.

```console
cargo run --bin foster -- run examples/live_inventory_pipeline.fos
```

`iteration.fos` demonstrates structural `Iterable<T>` conformance, a mutable `Iterator<T>` cursor,
and `Option<T>` exhaustion through dynamically dispatched contract methods.

`value_contracts.fos` demonstrates composed equality, total-ordering, and hashing contracts.

`arguments.fos` demonstrates the typed `std.process.Arguments` entry structure, executable-name
access, positional values, and a flag branch. It runs through the VM or as a native executable:

```console
cargo run --bin foster -- run examples/arguments.fos -- --about
cargo run --bin foster -- build examples/arguments.fos --native -o arguments
./arguments first-value
```

`type_composition.fos` demonstrates declaration-side composition, intersection parameters, and
static duck typing without a wrapper or runtime conversion.

`collections.fos` demonstrates the shared `Collection<T>` contract, a Foster-written `Set<T>`, the
generic `Range<T>` sequence view, and borrowed `.iterator()` creation.

`bytes.fos` demonstrates bounded `Byte` values, compact immutable `Bytes`, UTF-8 and hexadecimal
boundaries, and mutation followed by ownership-transferring `ByteBuffer.freeze`.

`streams.fos` demonstrates generic `Reader<E>` and `Writer<E>` conformance, partial writes, clean
EOF, and the Foster-written `stream::copy` algorithm.

`result_propagation.fos` demonstrates `try`: a successful `Result` value is unwrapped while an
error is returned immediately, even when the operation and enclosing function have different
success types.

`linked_list.fos` implements an owned generic linked list as a recursive variant, including
constant-time prepend/pop, reverse, map, fold, and conversion to the built-in list type.

## Focused capability showcase

The programs in `showcase/` are small, directly runnable demonstrations:

| Example | Foster capability |
| --- | --- |
| `accounts_pipeline.fos` | Records, enums, methods, closures, and lazy iterator pipelines |
| `closures.fos` | Nested, captured, and returned closures |
| `enums.fos` | Enum construction and exhaustive pattern matching |
| `float_recursion.fos` | Recursive floating-point computation |
| `function_selection.fos` | Selecting ordinary functions through a shared callable type |
| `generic_records.fos` | Generic record construction and access |
| `generic_recursion.fos` | Generic recursive data processing |
| `higher_order_functions.fos` | Callable parameters and recursive higher-order functions |
| `http_dispatch.fos` | Typed requests, responses, and pure dispatch logic |
| `iterator_consumers.fos` | Iterators, closures, and terminal consumers |
| `loops.fos` | Mutable state, `loop`, guards, and `break` |
| `nested_functions.fos` | Lexically nested functions and numeric convergence |
| `ownership_closures.fos` | Groups, references, closure captures, and partial application |
| `partial_application.fos` | Placeholder-based partial application |
| `persistent_map.fos` | Symbols and persistent map updates |
| `record_methods.fos` | Record methods and mutation |
| `recursion.fos` | Tail recursion with explicit accumulator state |
| `remote_analysis.fos` | Remote actors, futures, and asynchronous aggregation |
| `routing.fos` | Record methods, routing guards, and response modeling |
| `source_metrics.fos` | Sequence traversal and recursive source analysis |
| `static_resources.fos` | Owned domain models for deterministic static resources |

Run any focused example directly:

```console
cargo run --bin foster -- run examples/showcase/ownership_closures.fos
```

## Package examples

`json_parser/` is a multi-file package demonstrating module imports, recursive descent parsing,
generic `Option<T>` results, Unicode escapes, and deterministic rendering.

`modules/` demonstrates nested module paths and module-level function access.

```console
cargo run --bin foster -- run examples/json_parser
cargo run --bin foster -- run examples/modules
```
