# Library contract audit

This audit covers every public source type in `library`: records, enums, aliases, and
capability contracts themselves. Builtin scalar types and the builtin `Sequence<T>`
contract were reviewed separately. Private implementation types are outside this public API audit.

The review compares available public operations with each named contract's required
operations, parameter and result types, receiver effects, and documented semantics.
Transitive compositions count: for example, `HashMap` composes `Map`, which composes
`Collection`, which composes `Iterable`. Repeating each ancestor in every declaration is unnecessary.
Structural compatibility alone does not prove a semantic promise.

## Changes from the wider audit

- `File` composes `ReadWrite<IoError>` (covering its existing `Readable` and `Writable`
  promises) and `TextWriter<IoError>`.
- `Connection` composes `TextWriter<NetworkError>` alongside `Duplex` and `Closable`.
- `SystemRandom` composes `std.random.secure.EntropySource` alongside `RandomSource`.
- Previously added compositions cover copying, sequences, collections, equality, and hashing.

## Deliberate non-relationships

| Candidate | Decision |
| --- | --- |
| `Connection` → `TextReader` | Its host implementation decodes each bounded byte read independently (`host/src/lib.rs`, `HostContext::read`). It does not preserve UTF-8 boundaries across reads, as `TextReader` requires. Its `read_text` remains available without this promise. |
| `File` → `Reader` / `TextReader` / `Duplex` | Whole-file reads have no maximum argument or stream cursor. They are not bounded stream reads. |
| `File` → `Writer` | Each write replaces file contents. Repeated partial-transfer writes must not be advertised as a sequential stream. `TextWriter` accepts one complete string per call and is supported. |
| `Connection` → `Writable` / `ReadWrite` | Socket writes transmit a sequence; they do not replace a resource's complete contents. |
| `Writer` ↔ `Writable` | Identical method signatures do not erase their stream-versus-replacement semantic distinction. Neither contract inherits the other. |
| Collections, buffers, strings, `WeightedIndex` → resource `Sized` | Their length is an `Int`; `Sized<E>` requires a fallible byte length, `Result<Int, E>`. |
| `ByteBuffer` → `Collection` | No independent iterator operation. Length and emptiness alone are insufficient. |
| `List` / `StringBuilder` → resource `Appendable` | Their append parameter, result, and ownership behavior differ from fallible binary resource append. |
| `String` → `EntropySource` / `SplittableRandom` | UTF-8 bytes and text splitting are not random operations; signatures also differ. |
| Iterator `find` / list `find` → `TimeZoneDatabase` | Predicate search is not identifier-based lookup returning `Result<TimeZone, ZoneError>`. |
| Random sources → `Iterator` | Their `next` returns a fallible random value, not `Option<T>` signalling exhaustion. |
| `Int` / `Float` → `Ordered` | A `compare` helper is insufficient without `equal?`. In addition, float comparison deliberately lacks total-order semantics for NaN. Operator equality does not declare the named method contract. |
| Public data records and payload-free enums → `Copy` | Stored fields, immutability, or lack of payload do not supply a public `copy` method. No new operation was invented to create a relationship. |
| Arbitrary `List<T>` → `Copy` | Elements need not be copyable. There is no general shallow-copy promise. |
| `File`, paths, URI, endpoint → extra I/O capabilities | Resource identity alone does not promise access. Only implemented access contracts are composed. |

## Builtin types

`Bool`, `Byte`, `Int`, `Float`, and `CodePoint` implement the library's scalar `copy`
operation; their modules document `Copy`. They have no source record declaration to compose into.
`Sequence<T>` is compiler-provided; concrete `List<T>`, `String`, `Bytes`, and `Range<T>`
explicitly compose it with their actual element types. Operator support is not treated as
an implementation of similarly named public methods.

## Public source inventory

“None” means no explicit composition is needed for an additional supported named contract;
it does not prohibit structural use. Alias targets retain their own contracts. Contract
definitions appear in this inventory too. The table lists direct compositions; inherited
contracts are reachable through these links in generated documentation.

Reviewed **121 public type declarations** across the full library.

| Type | Direct contracts |
| --- | --- |
| [core.byte.ByteError](../library/core/byte.fos) | None |
| [core.bytes.buffer.ByteBuffer](../library/core/bytes/buffer.fos) | None |
| [core.bytes.Bytes](../library/core/bytes.fos) | `Copy`, `Sequence<Byte>`, `Collection<Byte>`, `Equality<Bytes>`, `Hashing` |
| [core.bytes.Utf8Error](../library/core/bytes.fos) | None |
| [core.bytes.HexError](../library/core/bytes.fos) | None |
| [core.code_point.GeneralCategory](../library/core/code_point.fos) | None |
| [core.copy.Copy](../library/core/copy.fos) | None |
| [core.drop.Drop](../library/core/drop.fos) | None |
| [core.functions.Predicate](../library/core/functions.fos) | Callable alias |
| [core.functions.Consumer](../library/core/functions.fos) | Callable alias |
| [core.functions.Supplier](../library/core/functions.fos) | Callable alias |
| [core.list.List](../library/core/list.fos) | `Sequence<T>`, `Collection<T>` |
| [core.list.ListReadError](../library/core/list.fos) | None |
| [core.option.Option](../library/core/option.fos) | None |
| [core.ordering.Ordering](../library/core/ordering.fos) | None |
| [core.ordering.Equality](../library/core/ordering.fos) | None |
| [core.ordering.Ordered](../library/core/ordering.fos) | `Equality<T>` |
| [core.ordering.Hashing](../library/core/ordering.fos) | None |
| [core.range.Range](../library/core/range.fos) | `Sequence<T>`, `Collection<T>` |
| [core.remote_error.RemoteError](../library/core/remote_error.fos) | None |
| [core.result.Result](../library/core/result.fos) | None |
| [core.string.String](../library/core/string.fos) | `Copy`, `Sequence<String>`, `Collection<String>` |
| [core.string.StringBuilder](../library/core/string.fos) | None |
| [core.string.StringCursor](../library/core/string.fos) | `Cursor<CodePoint>` |
| [core.string.GraphemeCursor](../library/core/string.fos) | `Iterator<String>` |
| [core.symbol.Symbol](../library/core/symbol.fos) | `Copy` |
| [std.collections.deque.Deque](../library/std/collections/deque.fos) | `Collection<T>` |
| [std.collections.deque.DequeItem](../library/std/collections/deque.fos) | None |
| [std.collections.hash_map.HashMap](../library/std/collections/hash_map.fos) | `Map<K, V>` |
| [std.collections.hash_set.HashSet](../library/std/collections/hash_set.fos) | `Set<T>` |
| [std.collections.map.Entry](../library/std/collections/map.fos) | None |
| [std.collections.map.Map](../library/std/collections/map.fos) | `Collection<Entry<K, V>>` |
| [std.collections.map.ListMap](../library/std/collections/map.fos) | `Map<K, V>` |
| [std.collections.queue.Queue](../library/std/collections/queue.fos) | `Collection<T>` |
| [std.collections.queue.QueueItem](../library/std/collections/queue.fos) | None |
| [std.collections.set.Set](../library/std/collections/set.fos) | `Collection<T>` |
| [std.collections.set.ListSet](../library/std/collections/set.fos) | `Set<T>` |
| [std.collections.stack.Stack](../library/std/collections/stack.fos) | `Collection<T>` |
| [std.collections.stack.StackItem](../library/std/collections/stack.fos) | None |
| [std.collections.Collection](../library/std/collections.fos) | `Iterable<T>` |
| [std.cursor.Cursor](../library/std/cursor.fos) | `Iterator<T>` |
| [std.cursor.ListCursor](../library/std/cursor.fos) | `Cursor<T>` |
| [std.cursor.BytesCursor](../library/std/cursor.fos) | `Cursor<Byte>` |
| [std.fs.File](../library/std/fs.fos) | `Resource<paths::Path>`, `ReadWrite<IoError>`, `TextWriter<IoError>`, `PositionedReadable<IoError>`, `Appendable<IoError>`, `Sized<IoError>` |
| [std.io.IoError](../library/std/io.fos) | None |
| [std.io.Reader](../library/std/io.fos) | None |
| [std.io.Writer](../library/std/io.fos) | None |
| [std.io.TextReader](../library/std/io.fos) | None |
| [std.io.TextWriter](../library/std/io.fos) | None |
| [std.io.Duplex](../library/std/io.fos) | `Reader<E>`, `Writer<E>` |
| [std.iter.Iterator](../library/std/iter.fos) | None |
| [std.iter.Iterable](../library/std/iter.fos) | None |
| [std.net.tcp.NetworkError](../library/std/net/tcp.fos) | None |
| [std.net.tcp.TcpEndpoint](../library/std/net/tcp.fos) | `ResourceIdentifier` |
| [std.net.tcp.Connection](../library/std/net/tcp.fos) | `Drop`, `Resource<TcpEndpoint>`, `Duplex<NetworkError>`, `TextWriter<NetworkError>`, `Closable<NetworkError>` |
| [std.net.tcp.Listener](../library/std/net/tcp.fos) | `Drop`, `Resource<TcpEndpoint>`, `Accepting<Connection, NetworkError>`, `Closable<NetworkError>` |
| [std.path.Path](../library/std/path.fos) | `ResourceIdentifier` |
| [std.process.Arguments](../library/std/process.fos) | None |
| [std.random.distribution.Distribution](../library/std/random/distribution.fos) | None |
| [std.random.distribution.UniformInt](../library/std/random/distribution.fos) | `Distribution<Int>` |
| [std.random.distribution.UniformFloat](../library/std/random/distribution.fos) | `Distribution<Float>` |
| [std.random.distribution.Bernoulli](../library/std/random/distribution.fos) | `Distribution<Bool>` |
| [std.random.distribution.WeightedIndex](../library/std/random/distribution.fos) | `Distribution<Int>` |
| [std.random.generator.LehmerRandom](../library/std/random/generator.fos) | `SeedableRandom`, `SplittableRandom` |
| [std.random.generator.FastRandom](../library/std/random/generator.fos) | `SeedableRandom`, `SplittableRandom` |
| [std.random.secure.EntropySource](../library/std/random/secure.fos) | None |
| [std.random.secure.SecureRandom](../library/std/random/secure.fos) | `RandomSource`, `EntropySource` |
| [std.random.RandomError](../library/std/random.fos) | None |
| [std.random.RandomSource](../library/std/random.fos) | None |
| [std.random.SeedableRandom](../library/std/random.fos) | `RandomSource` |
| [std.random.SplittableRandom](../library/std/random.fos) | `RandomSource` |
| [std.random.SystemRandom](../library/std/random.fos) | `RandomSource`, `secure::EntropySource` |
| [std.resource.ResourceIdentifier](../library/std/resource.fos) | None |
| [std.resource.Resource](../library/std/resource.fos) | None |
| [std.resource.Readable](../library/std/resource.fos) | None |
| [std.resource.Writable](../library/std/resource.fos) | None |
| [std.resource.PositionedReadable](../library/std/resource.fos) | None |
| [std.resource.Appendable](../library/std/resource.fos) | None |
| [std.resource.Sized](../library/std/resource.fos) | None |
| [std.resource.Closable](../library/std/resource.fos) | None |
| [std.resource.Accepting](../library/std/resource.fos) | None |
| [std.resource.ReadWrite](../library/std/resource.fos) | `Readable<E>`, `Writable<E>` |
| [std.time.civil.CivilError](../library/std/time/civil.fos) | None |
| [std.time.civil.Overflow](../library/std/time/civil.fos) | None |
| [std.time.civil.Weekday](../library/std/time/civil.fos) | None |
| [std.time.civil.Calendar](../library/std/time/civil.fos) | None |
| [std.time.civil.IsoCalendar](../library/std/time/civil.fos) | `Calendar` |
| [std.time.civil.Span](../library/std/time/civil.fos) | None |
| [std.time.civil.Period](../library/std/time/civil.fos) | Alias of `Span` |
| [std.time.civil.Date](../library/std/time/civil.fos) | `Ordered<Date>` |
| [std.time.civil.TimeOfDay](../library/std/time/civil.fos) | `Ordered<TimeOfDay>` |
| [std.time.civil.DateTime](../library/std/time/civil.fos) | `Ordered<DateTime>` |
| [std.time.civil.YearMonth](../library/std/time/civil.fos) | `Ordered<YearMonth>` |
| [std.time.civil.MonthDay](../library/std/time/civil.fos) | None |
| [std.time.civil.DateInterval](../library/std/time/civil.fos) | None |
| [std.time.format.FormatError](../library/std/time/format.fos) | None |
| [std.time.zone.ZoneError](../library/std/time/zone.fos) | None |
| [std.time.zone.Disambiguation](../library/std/time/zone.fos) | None |
| [std.time.zone.Offset](../library/std/time/zone.fos) | `Ordered<Offset>` |
| [std.time.zone.AmbiguousLocalTime](../library/std/time/zone.fos) | None |
| [std.time.zone.SkippedLocalTime](../library/std/time/zone.fos) | None |
| [std.time.zone.LocalResolution](../library/std/time/zone.fos) | None |
| [std.time.zone.TimeZone](../library/std/time/zone.fos) | None |
| [std.time.zone.TimeZoneDatabase](../library/std/time/zone.fos) | None |
| [std.time.zone.FixedOffsetZone](../library/std/time/zone.fos) | `TimeZone` |
| [std.time.zone.OffsetDateTime](../library/std/time/zone.fos) | `Ordered<OffsetDateTime>` |
| [std.time.zone.ZonedDateTime](../library/std/time/zone.fos) | `Ordered<ZonedDateTime>` |
| [std.time.TimeError](../library/std/time.fos) | None |
| [std.time.Duration](../library/std/time.fos) | `Ordered<Duration>` |
| [std.time.Instant](../library/std/time.fos) | `Ordered<Instant>` |
| [std.time.Interval](../library/std/time.fos) | None |
| [std.time.Clock](../library/std/time.fos) | None |
| [std.time.SystemClock](../library/std/time.fos) | `Clock<Instant>` |
| [std.time.MonotonicInstant](../library/std/time.fos) | `Ordered<MonotonicInstant>` |
| [std.time.ContinuousClock](../library/std/time.fos) | `Clock<MonotonicInstant>` |
| [std.toml.TomlDocument](../library/std/toml.fos) | None |
| [std.toml.TomlEntry](../library/std/toml.fos) | `Copy` |
| [std.toml.TomlValue](../library/std/toml.fos) | `Copy` |
| [std.toml.TomlError](../library/std/toml.fos) | None |
| [std.uri.UriError](../library/std/uri.fos) | None |
| [std.uri.Uri](../library/std/uri.fos) | `ResourceIdentifier` |
