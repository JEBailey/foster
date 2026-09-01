# Foster standard library

Foster has no prelude. Embedded library modules are available to every package, but programs
explicitly import the modules they use. The `core` namespace contains foundational language types;
the `std` namespace contains general-purpose collections and host-facing facilities:

```foster
import core.list
import core.option

func first_name(names: List<String>) -> Option<String> {
    names.first()
}
```

Importing a module makes its public declarations directly available and also binds the final module
component for qualification. A public operation with one natural nominal receiver is exposed as an
instance method, such as `names.first()`, `result.map(transform)`, or `document.get(key)`. Module
functions remain for namespace operations and structural algorithms without one nominal receiver,
such as `toml::parse(source)`, `sequence::map(values, transform)`, and
`io::copy(reader, writer)`.

Every standard-library function, including private implementation helpers, has an attached Markdown
documentation comment. Public documentation is available through language-server hover and
completion details. A compiled-HIR coverage test prevents undocumented library functions from
being added accidentally.

## Modules

| Module | Purpose |
| --- | --- |
| `core.functions` | Reusable predicate, consuming consumer, and supplier callable type aliases |
| `core.option` | Optional values, mapping, chaining, eager and lazy fallbacks, flattening, and presence queries |
| `std.iter` | Stateful `Iterator<T>` and repeatable `Iterable<T>` callable contracts |
| `std.collections` | The sized, repeatable `Collection<T>` contract |
| `core.byte` | Bounded byte construction and `ByteError` |
| `core.bytes` | Immutable compact bytes, hexadecimal conversion, and UTF-8 decoding |
| `core.bytes.buffer` | Mutable growable byte storage |
| `std.io` | Generic binary/text stream contracts and binary transfer algorithms |
| `std.resource` | Typed resource association and independent I/O capability contracts |
| `std.collections.set` | Insertion-ordered `Set<T>` |
| `std.collections.queue` | First-in, first-out `Queue<T>` |
| `std.collections.deque` | Double-ended `Deque<T>` |
| `std.collections.stack` | Last-in, first-out `Stack<T>` |
| `core.range` | Generic reusable `Range<T>` sequence view |
| `core.result` | Success/error values, transformations, recovery, eager and lazy fallbacks, flattening, and queries |
| `core.ordering` | Equality, total-ordering, and hashing contracts plus `Less`, `Equal`, and `Greater` |
| `std.sequence` | Shared map, filter, fold, search, slicing, and query algorithms for strings and lists |
| `core.list` | Search, map, filter, folds, slicing, flattening, joining, and predicates |
| `std.collections.map` | Generic maps with associated construction, lookup, insertion, keys, and values |
| `core.code_point` | Unicode scalar validation and ASCII/whitespace classification |
| `core.string` | Boundary queries, slicing, splitting, joining, prefix predicates, case conversion, trimming, and characters |
| `core.bool` | Boolean composition and conditional singleton-list construction |
| `core.int` | Bounds, comparison, sign, parity, ranges, formatting, and integer powers |
| `core.float` | Bounds, comparison, sign, clamping, and round-trippable formatting |
| `std.fs` | `File` resources, typed whole-file I/O, directory operations, copying, moving, and path-kind queries |
| `std.path` | Typed `Path` values plus compatible string-based platform path operations |
| `std.uri` | Parsed URI resource locations without implicit protocol I/O |
| `std.env` | Process environment queries such as the current directory |
| `std.process` | Typed executable name and command arguments supplied to `main` |
| `std.toml` | Typed TOML parsing, table lookup, rendering, and source-positioned errors |
| `std.net.tcp` | Typed TCP listeners and connections |
| `std.time` | Exact instants and durations, intervals, and generic wall/monotonic clocks |
| `std.time.civil` | ISO civil dates and times, calendar spans, partial dates, and civil intervals |
| `std.time.zone` | UTC offsets, time-zone contracts, explicit local resolution, and zoned values |
| `std.time.format` | ISO-8601 and RFC-3339 parsing and formatting |
| `std.random` | Random-source contracts, operating-system randomness, and unbiased uniform integers |
| `std.random.generator` | Portable seeded and default fast deterministic generators |
| `std.random.distribution` | Uniform, Bernoulli, and weighted probability distributions |
| `std.random.secure` | Operating-system entropy, random bytes, and security tokens |
| `std.random.sequence` | Random choice, shuffling, and sampling without replacement |

Primitive `CodePoint` operations are owner-qualified in `core.code_point`. After importing the
module, `character.as_int()` and `character.as_string()` call the corresponding `CodePoint`
instance methods, while `CodePoint.from(value)` performs checked construction. Go to Definition on
a `CodePoint` type annotation opens that module even when it has not yet been imported.

## Boundary with the runtime

The library is written in Foster wherever the language can express the operation. The bootstrap
runtime supplies representation-level primitives and capabilities that must cross the host boundary:

- sequence, list, and string `empty?`, `length`, `head`, and `rest`;
- functional list `append` and mutable list `push`;
- string concatenation;
- integer-like `CodePoint` operators, checked `from_code_point`, `parse_float`, and binary64 formatting;
- printing and remote-object runtime operations;
- filesystem, platform path, environment, and entry operations used by `std.fs`, `std.path`,
  `std.env`, and `std.process`;
- wall-clock and monotonic readings used by `std.time`;
- operating-system entropy used by `std.random` and `std.random.secure`;
- TCP socket operations used by `std.net.tcp`.

The host-facing intrinsics are private implementation details. Public APIs, opaque resource records,
and conversion into `Result` values are defined in Foster. `IoError` includes the operation, path,
and host message; `NetworkError` includes the operation and host message. TCP resources expose no
public handle field, so user code obtains them only through `listen`, `connect`, and
`listener.accept()`.

The entropy intrinsic is a dependency-free platform shim: Windows uses the system-preferred CNG
generator and Unix reads `/dev/urandom`. All bounded sampling, deterministic generators,
distributions, sequence algorithms, token encoding, validation, and typed errors are Foster code.

Foster-written library code uses `try` when it forwards an unchanged error type, including the
`std.io` transfer algorithms, TCP adapters, and TOML rendering helpers. It keeps an explicit
`branch` when an API recovers from an error, translates it, or otherwise needs to inspect it.

### Compiler intrinsics

These names form the typed boundary used by Foster-written library code. Editor navigation opens
this table for an intrinsic because it has no Foster implementation body.

| Intrinsic | Purpose |
| --- | --- |
| `print`, `println` | Write values to standard output, without or with a trailing newline |
| `code_point`, `from_code_point` | Legacy explicit widening and checked construction of `CodePoint`; ordinary widening applies in `Int` contexts and integer operators |
| `parse_float` | Parse a binary64 floating-point value from text |
| `FloatHost.format` | Format a binary64 value as round-trippable scalar text |
| `FsHost.read_text`, `FsHost.write_text`, `FsHost.read_bytes`, `FsHost.write_bytes` | Perform whole-file text and binary operations |
| `FsHost.list_directory` | List directory entries |
| `FsHost.exists`, `FsHost.is_file`, `FsHost.is_directory` | Query host filesystem paths |
| `FsHost.create_directory`, `FsHost.create_directory_all` | Create one directory or a directory tree |
| `FsHost.remove_file`, `FsHost.remove_directory` | Remove one file or one empty directory |
| `FsHost.rename`, `FsHost.copy_file` | Move or copy filesystem entries |
| `PathHost.join`, `PathHost.parent`, `PathHost.file_name`, `PathHost.extension` | Apply host path rules |
| `PathHost.canonicalize` | Resolve a host filesystem location |
| `EnvHost.current_directory` | Read the process working directory |
| `TcpHost.listen`, `TcpHost.connect`, `TcpHost.accept` | Establish TCP resources |
| `TcpHost.read`, `TcpHost.write`, `TcpHost.read_bytes`, `TcpHost.write_bytes` | Operate on TCP connections |
| `TcpHost.set_timeout` | Configure TCP connection timeouts |
| `TcpHost.close_listener`, `TcpHost.close_connection` | Close TCP resources |
| `TimeHost.wall_now` | Read canonical Unix seconds and fractional nanoseconds from the wall clock |
| `TimeHost.monotonic_now` | Read host-context-relative monotonic nanoseconds for elapsed measurement |
| `RandomHost.bytes` | Read a bounded number of bytes from the operating system's secure entropy source |

## Time values

The time library separates representations that have different semantics instead of allowing a
date, offset, or zone to be inferred implicitly:

| Layer | Principal types | Meaning |
| --- | --- | --- |
| `std.time` | `Instant`, exact `Duration`, `Interval`, `Clock<T>` | Points and elapsed lengths on an exact or monotonic time line |
| `std.time.civil` | `Date`, `TimeOfDay`, `DateTime`, `YearMonth`, `MonthDay`, `Span`/`Period` | Calendar and clock values with no implied offset or time zone |
| `std.time.zone` | `Offset`, `OffsetDateTime`, `TimeZone`, `ZonedDateTime` | Exact values interpreted through fixed displacement or named-zone rules |
| `std.time.format` | `FormatError` and parse/format functions | Portable ISO-8601 and RFC-3339 text |

This adapts Temporal's exact/plain/zoned separation to Foster's structural contracts. It also keeps
Noda Time's `Interval`, `DateInterval`, `OffsetDateTime`, `Period`, clock, and zone-provider concepts,
and Jiff's distinction between an exact signed duration and a calendar-aware span. A `Duration`
therefore never contains months or days; those units belong to `Span` and acquire an elapsed length
only when applied to a civil or zoned value.

`LocalResolution` exposes `Unique`, `Ambiguous`, and `Skipped` outcomes. Converting a local value to
a zoned value requires a `Disambiguation` policy, so daylight-saving overlaps and gaps cannot be
silently discarded. `FixedOffsetZone` supplies the complete `TimeZone` contract now; regional IANA
rule data is a provider-layer roadmap item. `Calendar` and `TimeZoneDatabase` are structural
contracts so additional calendars and versioned zone databases do not require changing the value
taxonomy.

Wall-clock access produces serializable Unix `Instant` values. The monotonic `ContinuousClock`
uses an opaque `MonotonicInstant` domain and is the appropriate clock for elapsed measurements and
deadlines. Its readings are intentionally not convertible to civil time or Unix timestamps.

See [Time in Foster](time.md) for the complete type-selection guide, arithmetic semantics, clock
guidance, parsing and formatting examples, and the current time-zone limitations.

## Random values

The random library separates the source of uniform bits from transformations that consume them.
`RandomSource` is the shared fallible stateful contract; `SystemRandom` implements it with operating-
system entropy, while `LehmerRandom` and `FastRandom` provide deterministic streams. Uniform range
reduction uses rejection sampling, so ranges which do not evenly divide a source's capacity do not
receive modulo bias.

Distribution values hold validated probability models. Sequence algorithms accept an explicit
source for reproducible work and have operating-system-backed convenience forms. Security APIs are
kept in `std.random.secure`: `SecureRandom`, `random_bytes`, `token_hex`, and URL-safe `token`.
Deterministic generators must never be used for secrets.

See [Randomness in Foster](random.md) for the full taxonomy, reproducibility guarantees, range
semantics, examples, and extension contract.

## TOML documents

`std.toml` represents a document as a `TomlDocument` containing top-level `TomlEntry` values.
Nested values use the closed `TomlValue` enum: `String`, `Integer`, `Float`, `Boolean`,
`DateTime`, `Array`, or `Table`. TOML date, time, and date-time forms retain their TOML text in the
`DateTime` case. `document.get(key)` looks up a top-level key, while `value.get(key)` looks inside a
table value. Both return `Option<TomlValue>` and consume the selected aggregate because they return
an owned value.

```foster
import core.option
import core.result
import std.toml

func package_name(source: String) -> Option<String> {
    branch toml::parse(move source) {
        Result.Error(_) -> Option.None
        Result.Ok(document) -> branch (move document).get("package") {
            Option.None -> Option.None
            Option.Some(package) -> branch (move package).get("name") {
                Option.Some(TomlValue.String(name)) -> Option.Some(name)
                _ -> Option.None
            }
        }
    }
}
```

`toml::parse` returns `Result<TomlDocument, TomlError>`. Parse errors contain a message and one-based
line and column. `document.render()` validates a constructed document and returns deterministic
TOML text; render-time errors use zero for line and column because they do not refer to source text.

The parser and renderer implement the TOML 1.1 grammar in `std.toml` itself. Project discovery
bootstraps the same embedded Foster module before package source loading, so `foster.toml` and user
code share one parser and one set of validation rules.

`String` implements `Sequence<CodePoint>`, and `List<T>` implements `Sequence<T>`. This is a
zero-conversion view: generic sequence functions operate on the original string or list value.
Foster's settled declaration syntax composes the same contract into a user type as
`type Foo = & Sequence<CodePoint> & { }`. Sequence members are required accessor functions rather than
implied storage, so constructors do not initialize `empty?`, `length`, `head`, or `rest`. A user
type supplies compatible instance functions before such a record is instantiated. Declared
receiver functions always use call syntax,
so a sequence contract method is invoked as `value.head()`. Conformance is statically duck typed,
so matching readable fields can
also satisfy those accessor requirements without an `&` clause. Composition adds no wrapper or
conversion. Functions in `std.sequence` are generic algorithms rather than stored members: they
are not copied into `Foo`, and already accept it through its `Sequence<T>` contract.
Code-point literals use single quotes, while string literals use double quotes. Operations that
return an owned generic element, such as `sequence::first`, consume their sequence argument;
observations such as `count`, `contains?`, `any?`, and `all?` borrow it.

`std.iter` defines the stateful `Iterator<T>` and repeatable `Iterable<T>` contracts.
`Iterator.from_sequence` consumes any `Sequence<T>` into a private Foster-written cursor. Calling
`next()` mutates that cursor and returns `Option<T>`; it does not mutate the original collection.
Iterator consumers are ordinary Foster-written receiver methods. `for_each`, `fold`, `find`,
`any?`, `all?`, and `count` process the cursor's remaining elements and leave it exhausted unless a
short-circuiting query returns early. Consumer callbacks are currently pure callable contracts;
general callback effect polymorphism remains future language work.

Lazy adaptors are Foster-written iterator records in `std.iter.map`, `std.iter.filter`,
`std.iter.take`, and `std.iter.skip`. Importing the desired adaptor modules makes their public
receiver methods available as extensions, so pipelines remain fluent while each private adaptor
module can independently implement the required `next` method:

```foster
values.iterator().map(transform).filter(predicate).take(10).collect()
```

Adaptors do no element work when constructed. Their `next` implementations pull only enough input
to produce the next output, and terminal consumers in `std.iter` drive the pipeline.

`std.collections` defines `Collection<T> & Iterable<T>` with `length` and `empty?`. The collection
family has this shape:

```text
Iterable<T>
└── Collection<T>
    ├── Sequence<T>
    │   ├── List<T>
    │   ├── String as Sequence<CodePoint>
    │   └── Range<T>
    ├── Set<T>
    ├── Queue<T>
    ├── Deque<T>
    └── Stack<T>

Map<K, V> & Collection<Entry<K, V>>
```

`List<T>`, `String`, and `Sequence<T>` expose `.iterator()` directly. Creating the cursor borrows the
source at the language level; the VM materializes an independent cursor over the source's read-only
value view, so advancing it neither consumes nor mutates the collection. The explicit
`Iterator.from_sequence` adapter remains available when ownership should be transferred.

`Map<K, V>` iterates public `Entry<K, V>` values in storage order. `Set`, `Queue`, `Deque`, `Stack`,
and `Range` are implemented in Foster on top of `List`, keeping only representation primitives in
the compiler and VM.

## Binary data

`Byte` is a copyable integer-like primitive bounded to `0..255`. `Byte.from(value)` performs checked
construction and returns `Result<Byte, ByteError>`. A byte widens losslessly where `Int` is expected,
and arithmetic with a byte produces `Int`, while
`&`, `|`, `^`, `~`, `<<`, and `>>` retain `Byte`; shift counts must be between zero and seven.

`Bytes` is immutable compact storage and structurally implements `Sequence<Byte>` and
`Collection<Byte>`. It supports indexing, slicing, concatenation, iteration, list conversion,
lowercase hexadecimal encoding, and checked hexadecimal decoding. The VM stores it as shared
contiguous byte storage rather than `List<Byte>`.

`ByteBuffer` is mutable growable storage with direct indexing, indexed replacement, `push`,
`extend`, `clear`, `truncate`, and `reserve`. `buffer.snapshot()` borrows the buffer and copies its
current contents. `(move buffer).freeze()` consumes it and transfers the contents into immutable
`Bytes`. Structural mutations invalidate outstanding element loans.

Text conversion is explicit: `text.utf8` encodes a string, while `String.from_utf8(bytes)` returns
`Result<String, Utf8Error>`. Filesystem and TCP modules provide parallel `read_bytes` and
`write_bytes` operations without changing their existing UTF-8 text APIs.

`std.io` defines `Reader<E>`, `Writer<E>`, `TextReader<E>`, and `TextWriter<E>` as generic
structural contracts. Binary reads return at most the requested number of bytes and use empty
`Bytes` for clean EOF. Binary writes report the accepted byte count, permitting partial writes;
successful non-empty operations must make progress. `read_all`, `write_all`, and `copy` implement
the retry and accumulation policy in Foster. `Duplex<E>` is the combined binary contract.

`std.resource` separates identity, provider association, and authority. `ResourceIdentifier`
requires a stable `resource_id()` representation, while `Resource<L>` associates a provider value
with a concrete identifier kind. Independent contracts describe `Readable<E>`, `Writable<E>`,
`PositionedReadable<E>`, `Appendable<E>`, `Sized<E>`, `Closable<E>`, and `Accepting<C, E>`
capabilities; `ReadWrite<E>` combines whole-resource reading and writing. Because these are
structural contracts, any value with the required public signatures conforms without a nominal
declaration or wrapper.

`std.path.Path`, `std.uri.Uri`, and `std.net.tcp.TcpEndpoint` implement
`ResourceIdentifier`, but identifiers gain no I/O authority merely by naming something.
`std.fs.File` is an opaque `Resource<Path>` with file capabilities. Constructing one does not
access the provider, so permission changes and availability failures remain typed `IoError`
results at each operation. `tcp::Connection` and `tcp::Listener` similarly retain
`Resource<TcpEndpoint>` rather than erasing their endpoint kind.

`tcp::Connection` implements `Duplex<NetworkError>`. Its `read` and `write` methods are the
contract operations, while `read_text`, `write_text`, `read_bytes`, and `write_bytes` are explicit
convenience spellings. A `File` is a whole-resource capability rather than a positioned stream;
the existing string-based filesystem functions remain available during migration to typed paths.

`core.ordering` also defines the structural contracts `Equality<T>`, `Ordered<T>`, and `Hashing`.
`Ordered<T>` composes equality and returns the existing `Ordering` enum from `compare`.
`Hashing.hash` returns a stable `Int`; equal values must hash identically, although unequal values
may collide.

TCP is deliberately a small blocking binary transport layer rather than an HTTP library; explicit
UTF-8 helpers remain available, while protocol parsing and higher-level policy belong in Foster.
Socket readiness, TLS, and explicit filesystem/network capability tokens remain future work.

Core APIs should not bypass ownership. In particular, operations that must retain an owned generic
value after invoking user code require a borrowed-callback type; they are intentionally omitted
until that contract can be expressed without weakening move checking. For the same reason,
`Map.get`, `keys`, and `values` consume the map when returning owned generic values, while
queries such as `contains_key?`, `length`, and `empty?` only borrow it.
