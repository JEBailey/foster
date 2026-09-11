# Foster standard library

The standard library provides text, collections, iteration, error values, time,
randomness, and host I/O. Foundational types live under `core`; general-purpose
facilities live under `std`. Import modules explicitly: Foster has no prelude
that imports their declarations for you.

The `.fos` files here are the authoritative implementation and API documentation.
Their Markdown comments appear in generated pages and LSP hovers. Installed
compilers embed the library, so consumers do not need a checkout.

## Getting started

Save this as `main.fos` and run `foster run main.fos`:

```foster
import core.list
import core.option

func main() -> Int {
    let doubled = [10, 20, 30].map((value: Int) -> value * 2)
    assert(doubled.first() == Option.Some(20))
    0
}
```

An import exposes public names and a module qualifier: `import core.option`
supports both `Option` and `option::Option`. Use qualifiers for ambiguous names.
Receiver methods serve operations such as `text.slice(0, 3)`; module functions
serve algorithms such as `sha256::digest(bytes)` and `io::copy(reader, writer)`.

Generate the API reference from the repository root:

```powershell
foster docs library
foster docs library --serve
```

The site is written to `library/documentation`. Edit source comments and
regenerate; do not maintain generated HTML by hand.

## Find an API

| Task | Modules |
| --- | --- |
| Unicode text, builders, text cursors | [core.string](core/string.fos), [core.code_point](core/code_point.fos) |
| Bytes and binary construction | [core.byte](core/byte.fos), [core.bytes](core/bytes.fos), [core.bytes.buffer](core/bytes/buffer.fos) |
| Lists and eager algorithms | [core.list](core/list.fos), [core.range](core/range.fos), [std.sequence](std/sequence.fos) |
| Lazy traversal | [std.iter](std/iter.fos), adaptors [map](std/iter/map.fos), [filter](std/iter/filter.fos), [take](std/iter/take.fos), [skip](std/iter/skip.fos) |
| Checkpoints and lookahead | [std.cursor](std/cursor.fos) |
| Maps and sets | [std.collections.map](std/collections/map.fos), [set](std/collections/set.fos), [hash_map](std/collections/hash_map.fos), [hash_set](std/collections/hash_set.fos), [hashing](std/collections/hashing.fos) |
| Collection contracts and ordered access | [std.collections](std/collections.fos), [stack](std/collections/stack.fos), [queue](std/collections/queue.fos), [deque](std/collections/deque.fos) |
| Optional values and failures | [core.option](core/option.fos), [core.result](core/result.fos), [core.remote_error](core/remote_error.fos) |
| Scalar helpers | [core.int](core/int.fos), [core.float](core/float.fos), [core.bool](core/bool.fos), [core.symbol](core/symbol.fos) |
| Common contracts | [core.copy](core/copy.fos), [core.drop](core/drop.fos), [core.ordering](core/ordering.fos), [core.functions](core/functions.fos) |
| Files and resource locations | [std.fs](std/fs.fos), [std.path](std/path.fos), [std.uri](std/uri.fos), [std.resource](std/resource.fos) |
| Streams and TCP | [std.io](std/io.fos), [std.net.tcp](std/net/tcp.fos) |
| Process inputs | [std.process](std/process.fos), [std.env](std/env.fos) |
| Time and calendars | [std.time](std/time.fos), [civil](std/time/civil.fos), [zone](std/time/zone.fos), [format](std/time/format.fos) |
| Random values | [std.random](std/random.fos), [generator](std/random/generator.fos), [distribution](std/random/distribution.fos), [secure](std/random/secure.fos), [sequence](std/random/sequence.fos) |
| Configuration | [std.toml](std/toml.fos) |
| SHA-256 digests | [std.crypto.sha256](std/crypto/sha256.fos) |

[core.unicode](core/unicode.fos) and its generated tables support the public text
APIs. Applications generally use `String` and `CodePoint` methods instead.
See [Unicode maintenance](../tools/unicode/README.md) for provenance and regeneration.

## Ownership and errors

Read the signature as well as the summary. A consuming operation transfers
ownership; use `move` when passing an existing binding. Map updates return the
updated collection, while `Map.get` consumes the entire map to return one optional
value. `contains_key?` borrows the map when only membership matters.

`List.at` distinguishes `OutOfBounds` and `NotCopyable`. `get`, `first`, and `last`
return `None` for either unavailable or noncopyable elements. Direct indexing and
checked slices have preconditions; they are not fallible lookup APIs.

Fallible operations return `Result<T, E>`. Use `try` to forward the same error
type; use `branch`, `map_error`, or recovery methods to choose another policy.
`Option<T>` represents absence without an explanation. Eager fallback arguments
are evaluated before a call; `_else` callbacks defer computation until needed.

```foster
import core.byte
import core.result

func main() -> Int {
    branch Byte.from(256) {
        Result.Ok(_) -> { assert(false, "256 cannot fit in a byte") }
        Result.Error(error) -> { assert(error.value == 256) }
    }
    0
}
```

## Text positions and cursors

| API | Position unit | Invalid bounds |
| --- | --- | --- |
| `String.length`, `String.slice` | Extended grapheme clusters | Slice bounds are clamped |
| `String.scalar_length()`, `scalar_slice` | Unicode scalars | Slice bounds are clamped |
| `String.byte_length()` | UTF-8 bytes | No bounds argument |
| `StringCursor.checkpoint`, `text_span` | UTF-8 bytes | Split scalars and invalid ranges are rejected |
| `Bytes.length`, `Bytes.slice` | Bytes | Slice bounds assert |
| `List.length`, `List.slice` | Elements | Slice bounds assert |

A scalar is not necessarily a displayed character: combining marks and emoji
sequences can contain several scalars. String counting, slicing, iteration, head/rest,
first/last, reversal, and splitting on an empty separator keep extended grapheme clusters
together. Cluster values are strings: `"é".length == 1`, while
`"é".scalar_length() == 2` and `"é".byte_length() == 3`.
Use `code_points()`, `scalar_head`, `scalar_rest`, `scalar_slice`, and `scalar_take_while`
when working with Unicode scalars explicitly. Unicode casing can also change length.
Full `lower`, `upper`, and `case_fold` return strings; `CodePoint`'s `simple_*`
methods return one scalar. Casing uses Unicode 17.0.0 without normalization or
language-specific tailoring. `ascii_lower` and `ascii_upper` affect ASCII only.

Use `StringCursor.from(text)` for repeated scanning without constructing suffix
strings. `Cursor<T>` describes the yielded item type, independently of the offset
unit. Failed restores leave the reader unchanged; invalid spans return `None`.
Spans materialize their result without advancing. Checkpoints are plain offsets:
callers must keep them with the same source snapshot, because the cursor cannot
recognize an offset copied from an unrelated source.

## Choosing a collection

`Map.empty` and `Set.empty` construct insertion-ordered list implementations with
linear membership searches. Hash collections use caller-supplied hashes and have
expected constant-time bucket lookup, with a linear worst case. Equal keys must
have equal hashes, and stored keys' hashes must remain stable. Hash iteration
order is unspecified. See [hash collections](../docs/hash-collections.md).

List and sequence transformations are eager. Iterator adaptors defer work until
items are requested; terminal operations advance the remaining iterator. Use
`StringBuilder` or `ByteBuffer` for incremental construction. `snapshot` preserves
a byte buffer; `freeze` consumes it. Capacity methods are hints, not promises about
reserved allocation size.

## Host behavior and limits

- Filesystem, TCP, clock reads, and operating-system entropy use the host runtime.
  Text algorithms, collections, calendar arithmetic, parsing, and SHA-256 are Foster code.
- Whole-file I/O allocates in proportion to input. Stream helpers stop at the
  first error without rolling back earlier I/O. `File.flush` is a no-op, not a
  durability guarantee.
- Paths use host-platform rules. Canonicalization accesses the filesystem; URI
  construction performs no network I/O and does not parse every URI component.
- Use monotonic clocks for elapsed time. Civil spans are not elapsed durations.
  Time-zone contracts allow custom providers, but no IANA database ships here.
- Seeded generators have explicit reproducibility guarantees. Secure entropy
  APIs produce unpredictable tokens; collection hashes are not cryptographic digests.
- Numeric helpers retain their documented edge behavior. `Float.compare` returns
  `Equal` for unordered comparisons involving NaN; it is not a total ordering.

## Contributing and verification

Follow the [documentation standard](DOCUMENTATION.md). Public comments describe
observable behavior, ownership, bounds, errors, units, and relevant limitations.
When a method is declared in a type and implemented separately, keep both comments
consistent. Keep private implementation notes out of public summaries.

Use mutable `let` bindings and explicit ownership transfers. Keep public effect
bounds precise; do not broaden field permissions just to shorten a signature.
Private helpers may infer effects where an explicit bound adds no useful contract.

From the repository root:

```powershell
foster test library
foster test tests/foster
foster docs library
```

Portable tests live beside their implementations. Rust integration tests cover
host resources and check that every library function has documentation. Comment
coverage alone does not establish correctness: compile and run examples and
review their claims against implementations and boundary tests.
