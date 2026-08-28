# Foster standard library

The standard library is written in Foster and is available through explicit imports. Foster has no
prelude and does not inject library declarations into user modules. Foundational language types
live under `core`; general-purpose facilities live under `std`.

```foster
import core.list
import core.option

func first_doubled(values: List<Int>) -> Option<Int> {
    list.first(list.map(values, (value: Int) -> value * 2))
}
```

`import core.option` exposes its public `Option` type directly. The `option.Option` spelling remains
available when qualification improves clarity or resolves an ambiguity. Common function names such
as `map` should normally remain module-qualified when several collection or result modules are
imported.

The compiler embeds these source modules so installed tools can resolve `core.*` and `std.*`
without depending on the repository layout. The files in this directory remain authoritative.
Every function carries a Markdown documentation comment. Public comments describe behavior,
ownership, boundary conditions, and errors where relevant; private comments identify the helper's
role in the implementation. The compiler retains these comments for language-server hover and
completion information, and the test suite enforces complete function coverage.
The implementations use fully qualified enum constructors and patterns, explicit public
signatures, and explicit record fields. Primitive members such as `List.push` and `List.append` are
owner-qualified intrinsic declarations, so their source identity is resolved before VM dispatch.
`String` is an opaque Foster record backed by valid UTF-8 `Bytes`; literals and host decoding are
its trusted construction paths, while its library algorithms are ordinary Foster functions.

Current modules:

- `core.functions`: reusable `Predicate<T>`, consuming `Consumer<T>`, and `Supplier<T>` callable aliases
- `core.option`: `Option`, mapping, chaining, eager and lazy fallbacks, flattening, and presence queries
- `std.iter`: stateful iteration contracts plus Foster-written `for_each`, `fold`, `find`, query,
  and counting consumers
- `std.iter.map`, `std.iter.filter`, `std.iter.take`, and `std.iter.skip`: lazy Foster-written
  iterator adaptors used to build fluent pipelines
- `core.result`: `Result`, mapping, error mapping, chaining, recovery, fallbacks, flattening, and queries
- `core.ordering`: `Ordering`, `Equality<T>`, `Ordered<T>`, `Hashing`, and `reverse`
- `std.sequence`: map, filter, fold, search, slicing, and query algorithms shared by strings and lists
- `core.list`: safe access, map, filter, fold, find, predicates, reverse, and concatenation
- `core.code_point`: validated Unicode scalar construction and conversion
- `core.string`: slicing, splitting, joining, trimming, case conversion, and Unicode helpers
- `core.bool`, `core.int`, and `core.float`: scalar algorithms and comparisons
- `core.byte`: checked construction and integer conversion for eight-bit unsigned values
- `core.bytes`: immutable compact bytes, hexadecimal conversion, hashing, and UTF-8 conversion
- `core.bytes.buffer`: mutable binary construction with consuming `freeze` and borrowing `snapshot`
- `std.io`: generic binary/text stream contracts plus `read_all`, `write_all`, and `copy`
- `std.collections.map`: a generic Foster-written map with opaque list-backed storage
- `std.fs`: typed text and binary files, directory creation/removal, copying, moving, and inspection
- `std.path`: platform path composition, inspection, and canonicalization
- `std.env`: process environment queries
- `std.toml`: a Foster-written TOML 1.1 parser, typed documents, table lookup, rendering, and positioned errors
- `std.net.tcp`: typed TCP listeners and `Duplex<NetworkError>` connections

The register VM executes imported core code and calls across modules after the normal checked-HIR
pipeline. Filesystem and TCP operations cross into the Rust runtime. TOML grammar, validation,
document construction, and rendering remain Foster source and use only general scalar primitives.

Fallible APIs return the Foster-written `Result<T, E>` type. Library implementations use `try`
only to forward the same error type; recovery, error mapping, and conversion remain explicit
`branch` expressions so those policy decisions stay visible.
