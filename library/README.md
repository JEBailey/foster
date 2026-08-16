# Foster core library

The core library is written in Foster and is available through explicit imports. Foster has no
prelude and does not inject library declarations into user modules.

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

The compiler embeds these source modules so installed tools can resolve `core.*` without depending
on the repository layout. The files in this directory remain the authoritative implementation.
Every function carries a Markdown documentation comment. Public comments describe behavior,
ownership, boundary conditions, and errors where relevant; private comments identify the helper's
role in the implementation. The compiler retains these comments for language-server hover and
completion information, and the test suite enforces complete function coverage.
The implementations use fully qualified variant constructors and patterns, explicit public
signatures, and explicit record fields. Operators and primitive members such as `List.head`,
`List.rest`, `List.append`, `String.head`, and `String.rest` are the current lowest-level language
operations; they will move behind trusted intrinsic declarations once that mechanism exists.

Current modules:

- `core.option`: `Option`, `map`, `and_then`, `unwrap_or`, and `present?`
- `core.result`: `Result`, `map`, `map_error`, `and_then`, and `success?`
- `core.ordering`: `Ordering` and `reverse`
- `core.sequence`: map, filter, fold, search, slicing, and query algorithms shared by strings and lists
- `core.list`: safe access, map, filter, fold, find, predicates, reverse, and concatenation
- `core.character`: validated Unicode scalar construction and conversion
- `core.string`: slicing, splitting, joining, trimming, case conversion, and Unicode helpers
- `core.bool`, `core.int`, and `core.float`: scalar algorithms and comparisons
- `core.map`: a generic Foster-written map with opaque list-backed storage
- `core.io`: typed text-file, directory, and path operations
- `core.net.tcp`: typed TCP listeners and connections

The register VM executes imported core code and calls across modules after the normal checked-HIR
pipeline. Filesystem and TCP operations necessarily cross into the host runtime; their public
records, result types, and policy wrappers remain Foster source.
