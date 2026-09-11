# Foster standard library

## Writing library code

Use mutable `let` bindings and explicit `move` when transferring an existing owned
value to a consuming parameter. Use references when a helper must update caller-owned
storage. Keep public effect bounds precise and checked; allow private helpers to infer
effects where an explicit contract adds no useful constraint.

Group sibling paths with the same effect and owner when this reduces repetition:
`[mut self(buckets.items, size), consume self]`. Keep different owners and effect kinds
separate, and use dotted syntax for a single path. Do not replace a list of field
permissions with a broader root permission just to shorten the declaration.

Run `foster test library` and `foster test tests/foster` after changing shared library
code. Examples should use the same syntax and ownership conventions as library code.

`std.crypto.sha256` provides `sha256::digest(bytes)`, returning a 64-character
lowercase hexadecimal SHA-256 digest. It is implemented entirely in Foster and
preserves its input bytes:

```foster
import std.crypto.sha256
func main() -> String { sha256::digest("abc".bytes) }
```

`core.code_point` provides Unicode 17.0.0 classification: `category()` returns a
`GeneralCategory`; predicates include `letter?`, `alphabetic?`, `digit?`, `numeric?`,
`alphanumeric?`, `lowercase?`, `uppercase?`, `titlecase?`, `mark?`, `punctuation?`,
`symbol?`, `separator?`, `control?`, `assigned?`, `identifier_start?`, and
`identifier_part?`. `letter?` means category L; `alphabetic?` uses the broader
Alphabetic property. `digit?` means decimal digit (Nd), while `numeric?` means any
Number category (N). Identifier predicates use XID_Start and XID_Continue;
these do not change Foster's identifier syntax. Predicates are called as methods,
for example `'λ'.letter?()` and `'٣'.digit?()`.

`String.lower()` and `upper()` now apply full, locale-independent Unicode casing.
`case_fold()` supports caseless comparison, for example
`"Straße".case_fold() == "STRASSE".case_fold()`. These operations also exist on
`CodePoint` and return strings because mappings can expand (`ß` becomes `SS`).
`CodePoint.simple_lower()`, `simple_upper()`, and `simple_title()` return a single
code point. String lowercasing handles Greek final sigma in context. Casing and
folding do not normalize text or apply Turkish/Lithuanian locale tailoring.
`String.ascii_lower()` and `ascii_upper()` preserve the previous ASCII-only behavior.
See [Unicode data maintenance](../tools/unicode/README.md) for provenance and regeneration.
The new algorithms and lookup tables are written in Foster.

The standard library is written in Foster and is available through explicit imports. Foster has no
prelude and does not inject library declarations into user modules. Foundational language types
live under `core`; general-purpose facilities live under `std`.

```foster
import core.list
import core.option

func first_doubled(values: List<Int>) -> Option<Int> {
    values.map((value: Int) -> value * 2).first()
}
```

`import core.option` exposes its public `Option` type directly. The `option::Option` spelling remains
available when qualification improves clarity or resolves an ambiguity. Operations that have one
natural nominal receiver are instance methods, so scalar values, lists, strings, options, results,
orderings, and TOML values use fluent calls such as `base.power(exponent)`,
`values.map(transform)`, and `document.get(key)`. Module
functions remain for construction and algorithms without one nominal receiver, such as
`toml::parse(source)`, `sequence::map(values, transform)`, and `io::copy(reader, writer)`.

The compiler embeds these source modules so installed tools can resolve `core.*` and `std.*`
without depending on the repository layout. The files in this directory remain authoritative.
Every function carries a Markdown documentation comment. Public comments describe behavior,
ownership, boundary conditions, and errors where relevant; private comments identify the helper's
role in the implementation. The compiler retains these comments for language-server hover and
completion information, and the test suite enforces complete function coverage.
The implementations use fully qualified enum constructors and patterns, explicit public
signatures, and explicit record fields. Primitive members such as `List.push` and `List.append` are
owner-qualified intrinsic declarations, so their source identity is resolved before VM dispatch.
Library implementations use `not`, `&&`, and `||` for Boolean logic and short-circuiting.
`String` is an opaque Foster record backed by valid UTF-8 `Bytes`; literals and host decoding are
its trusted construction paths, while its library algorithms are ordinary Foster functions.
Hexadecimal conversion, UTF-8 validation, text-I/O adaptation, and the list-backed `ByteBuffer`
implementation are Foster code; only compact byte packing/unpacking and trusted string construction
remain at that representation boundary.

Portable library behavior is tested with Foster `test` declarations beside the implementation.
Run the complete library suite with `foster test library`; the Rust integration harness executes it
with and without bytecode optimization during `cargo test`. Host-dependent filesystem, process,
network, clock, and operating-system entropy behavior remains in Rust integration tests so those
tests can control or inspect operating-system resources.

Current modules:

- `core.copy` and `core.drop`: explicit independent copies and automatic ownership cleanup
- `core.symbol`: immutable symbolic identifiers
- `core.range`: reusable list-backed range views
- `core.remote_error`: remote execution failures and the reserved shutdown outcome
- `std.collections`: storage-free `Collection<T>` contract for sized, repeatable collections
- `std.collections.set`: storage-free `Set<T>` contract and insertion-ordered `ListSet<T>`
- `std.collections.stack`, `std.collections.queue`, and `std.collections.deque`: concrete
  last-in-first-out, first-in-first-out, and double-ended collections
- `std.process`: typed executable name and command arguments supplied to `main`
- `std.collections.hash_map`: Foster-written `HashMap<K, V>` with separate collision chains,
  automatic growth, replacement, removal, consuming lookup, and snapshot iteration
- `std.collections.hash_set`: Foster-written `HashSet<T>` sharing HashMap storage and hashing
- `std.collections.hashing`: deterministic integer, UTF-8 text, and byte hash functions

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
- `core.code_point`: validated Unicode scalar construction plus `as_int` and `as_string` conversion
- `core.string`: slicing, splitting, joining, trimming, case conversion, and Unicode helpers
- `core.bool`, `core.int`, and `core.float`: scalar algorithms and comparisons
- `core.byte`: checked construction and integer conversion for eight-bit unsigned values
- `core.bytes`: immutable compact bytes, hexadecimal conversion, hashing, and UTF-8 conversion
- `core.bytes.buffer`: mutable binary construction with consuming `freeze` and borrowing `snapshot`
- `std.io`: generic binary/text stream contracts plus `read_all`, `write_all`, and `copy`
- `std.resource`: abstract resource locations plus readable, writable, and read/write structural capabilities
- `std.collections.map`: storage-free `Map<K, V>` contract and insertion-ordered `ListMap<K, V>`
- `std.fs`: `File` resources, typed text and binary I/O, directory mutation, copying, moving, and inspection
- `std.path`: typed `Path` values plus compatible string-based composition, inspection, and canonicalization
- `std.uri`: parsed URI resource locations; protocol-specific I/O remains separate
- `std.env`: process environment queries
- `std.toml`: a Foster-written TOML 1.1 parser, typed documents, table lookup, rendering, and positioned errors
- `std.net.tcp`: typed TCP listeners and `Duplex<NetworkError>` connections with explicit `close`
- `std.time`: exact `Instant` and `Duration` values, half-open `Interval` values, and generic wall
  and monotonic `Clock<T>` implementations
- `std.time.civil`: ISO `Date`, `TimeOfDay`, `DateTime`, `YearMonth`, `MonthDay`, calendar-aware
  `Span`/`Period`, civil intervals, and the structural `Calendar` contract
- `std.time.zone`: offsets, fixed and structural time zones, explicit unique/ambiguous/skipped local
  resolution, `OffsetDateTime`, and `ZonedDateTime`
- `std.time.format`: portable ISO-8601 and RFC-3339 parsing and formatting
- `std.random`: structural source contracts, operating-system randomness, and unbiased half-open integer ranges
- `std.random.generator`: named portable `LehmerRandom` and release-local `FastRandom` generators
- `std.random.distribution`: uniform integer/float, Bernoulli, and weighted-index distributions
- `std.random.secure`: secure entropy bytes and hexadecimal or URL-safe token generation
- `std.random.sequence`: random choice, shuffling, and sampling without replacement

The register VM executes imported core code and calls across modules after the normal checked-HIR
pipeline. Filesystem, TCP, clock readings, and operating-system entropy cross into the Rust
runtime. Exact and civil time arithmetic, fixed-zone resolution, ISO/RFC formatting, TOML grammar,
validation, document construction, and rendering remain Foster source and use only general scalar
primitives.
The entropy boundary has no additional Rust package dependency; random range reduction,
generators, distributions, tokens, and sequence algorithms remain Foster source as well.

Fallible APIs return the Foster-written `Result<T, E>` type. Library implementations use `try`
only to forward the same error type; recovery, error mapping, and conversion remain explicit
`branch` expressions so those policy decisions stay visible.

`Map` and `Set` define shared behavior; `ListMap`/`HashMap` and `ListSet`/`HashSet` supply storage.
Existing `Map.empty()`, `Set.empty()`, and `Set.from(values)` factories construct the list-backed
implementations. Use a concrete type annotation when its representation or insertion order matters;
use the contracts for functions accepting either implementation. See
[hash collections](../docs/hash-collections.md) for hashing and ownership semantics.

## Checkpointable cursors

`std.cursor.Cursor<T>` extends the iterator contract with `peek`, `checkpoint`,
`restore`, and `span`. The type parameter is the item type. Construct a concrete
reader with `StringCursor.from(text)` (`core.string`), `BytesCursor.from(bytes)`,
or `ListCursor.from(values)` (`std.cursor`). List reads require copyable elements.

```foster
import core.string
import std.cursor

let reader = StringCursor.from("Aλ🙂")
reader.next()
let start = reader.checkpoint() // byte offset 1
reader.next()
let matched = reader.text_span(start, reader.checkpoint()) // Some("λ")
reader.restore(start)
```

Checkpoints are source offsets: UTF-8 byte offsets for strings, byte offsets for
bytes, and element offsets for lists. Use checkpoints with the same source
snapshot. String readers reject offsets inside a multibyte encoding. Failed
restores leave the position unchanged; invalid spans return `None`. End-of-input
reads repeatedly return `None` without advancing. `span` returns a `List<T>`;
`StringCursor.text_span` and `BytesCursor.byte_span` preserve the source type.
Spans materialize their result. Cursors reuse stable backing storage and do not
buffer arbitrary iterators. All cursor algorithms are implemented in Foster.

Concrete cursor operations and generic `Cursor<T>` dispatch are supported by both
the VM and native backend. `cursor.fos` and `cursor_dispatch.fos` verify UTF-8
boundaries, reader state, and inherited iterator operations with optimization
enabled and disabled.
