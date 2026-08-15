# Foster core library

Foster has no prelude. Core modules are available to every package, but programs explicitly import
the modules they use:

```foster
import core.list
import core.option

func first_name(names: List[String]) -> Option[String] {
    list.first(names)
}
```

Importing a module makes its public declarations directly available and also binds the final module
component for qualification. Qualification is preferred when common names such as `map`, `first`,
`minimum`, or `contains?` would otherwise be ambiguous.

Every core function, including private implementation helpers, has an attached Markdown
documentation comment. Public documentation is available through language-server hover and
completion details. A compiled-HIR coverage test prevents undocumented library functions from
being added accidentally.

## Modules

| Module | Purpose |
| --- | --- |
| `core.option` | Optional values, mapping, chaining, fallbacks, flattening, and queries |
| `core.result` | Success/error values, transformations, recovery, flattening, and queries |
| `core.ordering` | `Less`, `Equal`, and `Greater`, with ordering queries and reversal |
| `core.sequence` | Shared map, filter, fold, search, slicing, and query algorithms for strings and lists |
| `core.list` | Search, map, filter, folds, slicing, flattening, joining, and predicates |
| `core.map` | Generic maps with associated construction, lookup, insertion, keys, and values |
| `core.character` | Unicode scalar validation and ASCII/whitespace classification |
| `core.string` | Boundary queries, slicing, splitting, joining, case conversion, trimming, and characters |
| `core.bool` | Boolean composition and conditional singleton-list construction |
| `core.int` | Bounds, comparison, sign, parity, ranges, formatting, and integer powers |
| `core.float` | Bounds, comparison, sign, and clamping |
| `core.io` | Typed text-file, directory, and path operations |
| `core.net.tcp` | Typed TCP listeners and connections |

## Boundary with the runtime

The library is written in Foster wherever the language can express the operation. The bootstrap
runtime supplies representation-level primitives and capabilities that must cross the host boundary:

- sequence, list, and string `empty?`, `length`, `head`, and `rest`;
- functional list `append` and mutable list `push`;
- string concatenation;
- `code_point`, `from_code_point`, and `parse_float`;
- printing and remote-object runtime operations;
- filesystem and platform path operations used by `core.io`;
- TCP socket operations used by `core.net.tcp`.

The host-facing intrinsics are private implementation details. Public APIs, opaque resource records,
and conversion into `Result` values are defined in Foster. `IoError` includes the operation, path,
and host message; `NetworkError` includes the operation and host message. TCP resources expose no
public handle field, so user code obtains them only through `listen`, `connect`, and `accept`.

`String` implements `Sequence[CodePoint]`, and `List[T]` implements `Sequence[T]`. This is a
zero-conversion view: generic sequence functions operate on the original string or list value.
Code-point literals use single quotes, while string literals use double quotes. Operations that
return an owned generic element, such as `sequence.first`, consume their sequence argument;
observations such as `count`, `contains?`, `any?`, and `all?` borrow it.

TCP currently transports UTF-8 text. It is deliberately a small blocking transport layer rather
than an HTTP library; protocol parsing and higher-level policy belong in Foster. Byte buffers,
socket readiness, TLS, and explicit filesystem/network capability tokens remain future work.

Core APIs should not bypass ownership. In particular, operations that must retain an owned generic
value after invoking user code require a borrowed-callback type; they are intentionally omitted
until that contract can be expressed without weakening move checking. For the same reason,
`core.map.get`, `keys`, and `values` consume the map when returning owned generic values, while
queries such as `contains_key?`, `length`, and `empty?` only borrow it.
