# Foster Language Design

This document is an inventory of the Foster language implemented by this repository. It describes
the syntax and semantics accepted by the bootstrap compiler, the behavior of its VM, and the core
library shipped with it. It is descriptive rather than a roadmap: unimplemented ideas belong in
focused design notes or issues, not in this language summary.

## Language snapshot

Foster is a statically typed, general-purpose language. It uses compile-time duck typing: a value
conforms to a type when its accessible contract matches, without requiring nominal inheritance or
an explicit `implements` declaration.
Its memory-safety model uses single ownership with group-parameterized references:
references describe the set of locations they may target, while mutation is expressed as a
function effect.

## Source files and modules

Filesystem structure determines module structure.

- A directory implicitly defines an empty module.
- A `.fos` file defines the body of its corresponding module.
- A same-named file and directory describe one module: the file contains its declarations and the
  directory contains its children.
- Module components must be portable identifiers and may not differ only by case.
- The package source root is implicit and is not itself a named module.

```text
json.fos          json, with declarations
json/
  parser.fos      json.parser
tools/               tools, implicit and empty
  text/              tools.text, implicit and empty
    trim.fos      tools.text.trim
```

Imports use canonical dotted names and may bind an alias:

```foster
import json
import json.parser as parser

func decode(source: String) {
    parser.parse(source)
}
```

An import makes the module's public declarations available directly and also binds its final path
component as a module qualifier. Thus `import core.option` permits both `Option<T>` and
`option.Option<T>`. Same-module declarations take precedence. If multiple imported modules expose
the same unqualified name, Foster requires a module-qualified use at that point; importing the
modules themselves remains valid.

Modules and declarations occupy one logical name system. Modules established by a `.fos` file,
a directory, or both are implicitly public and may always be addressed by canonical path. Every
declaration inside a module is private unless explicitly marked `pub`:

```foster
func helper() { }       // visible only within this module
pub func parse() { }    // visible through the module's canonical path
const LIMIT = 100        // module-private compile-time value
pub const VERSION = "1" // visible to importing modules
```

This default applies to function and type declarations. A public declaration may not expose a
private declaration in its public signature.

## Functions and evaluation

`func` introduces a function. Type annotations may state parameter and result types; otherwise the
compiler infers them. Local values use inference. The last expression in a function is its result.

`test` introduces a private, zero-argument `Unit` test declaration identified by a non-empty string:

```foster
test "decoding preserves text" {
    let decoded = decode("Foster".utf8)
    println(decoded)
}
```

Tests use the ordinary function type, effect, ownership, bytecode, and runtime semantics, but do not
enter the module namespace and cannot be called or imported. `foster test` discovers them across a
source package. See [Testing Foster programs](testing.md).

```foster
func double(value: Int) -> Int {
    value * 2
}
```

### Command entry arguments

An executable entry may remain `func main()`, or take exactly one command-argument record from
`std.process`:

```foster
import std.process

func main(arguments: Arguments) -> String {
    return arguments.executable if arguments.values.empty?
    arguments.values[0]
}
```

`Arguments.executable: String` is the path or command used to invoke the program.
`Arguments.values: List<String>` contains the remaining values and does not repeat the executable.
`foster run` accepts these after an explicit `--`; a native executable receives its operating
system command line directly. A `main` with any other parameter list is rejected with `E0901`.
Command arguments must be valid Unicode because Foster `String` preserves valid UTF-8.

Explicit `return` performs an early return. Control transfers may have a postfix `if` guard:

```foster
func first(values: List<String>) -> String {
    return "" if values.empty?
    values.head
}
```

The value is evaluated and returned only when the guard is `true`; execution continues with the
next statement when it is `false`. The guard must have type `Bool`.

`if` is deliberately not a general conditional statement or expression. It may only follow a
control-transfer statement, so `write(value) if ready` and `value = next() if ready` are invalid.
Use `branch` when choosing whether to evaluate a value-producing operation.

`return` is currently Foster's only control-transfer statement. Future transfers such as `break`
and `continue` will use the same postfix form if they are added. Foster has no `throw` statement;
recoverable errors remain ordinary typed `Result` values.

Identifiers may end in `?`, conventionally marking Boolean observations such as `empty?` and
`whitespace?`. Commas separate arguments and generic parameters. Newlines separate statements.

## Values

Module constants use `const`, are private by default, and must have compile-time initializers.
The implemented initializer forms are primitive literals, other module constants, unary-negative
numeric literals, and recursively constant homogeneous lists. Their types are inferred, and their
values are embedded directly into VM bytecode rather than allocated in mutable module storage.
Constants may be referenced before their declarations, but cycles are rejected. Function-local
values are introduced with `let name = value`; later `name = value` statements reassign an existing
local. `const` is deliberately module-level only.

```foster
const RETRY_LIMIT = 3
pub const HTTP_SUCCESS = [200, 201, 204]

func retries() -> Int {
    RETRY_LIMIT
}
```

Implemented built-in and runtime types include:

- `Bool`
- `Int`
- `Float` (IEEE-754 binary64)
- `String`
- `CodePoint`, with literals such as `'F'`, `'λ'`, and `'\n'`
- symbols such as `:json_error`
- homogeneous lists, enforced by the type checker
- `Sequence<T>`, implemented without conversion by `List<T>` and by `String` as
  `Sequence<CodePoint>`
- unit

There is no universally nullable reference type. The core library represents absence with
`Option<T>`:

```foster
type Option<T> =
    | Some(T)
    | None
```

`Sequence<T>` is a read-oriented structural view, not a storage representation. Passing a list or
string to a sequence parameter retains the original runtime value and ownership. Its common
members are `empty?`, `length`, `head`, and `rest`. For strings, `head` returns a `CodePoint` and
`rest` remains a `String` when accessed directly; through a `Sequence<CodePoint>` parameter,
`rest` has sequence type. A code point exposes `.string` and `.whitespace?`. `CodePoint` is a
bounded integer-like primitive: integer arithmetic and comparisons promote its Unicode scalar
value, and arithmetic produces `Int`. This permits expressions such as `'9' - '0'` and
`character < 32` without an extraction member. Conversion from an arbitrary `Int` remains checked
because surrogate values and values above `0x10FFFF` are not Unicode scalar values.
The bootstrap compiler supplies the `String` and `List<T>` conformances. A type definition begins
after `=`, and each composed contract is aligned with `&` on the right-hand side:

```foster
type Foo = & Sequence<CodePoint> & {
    source: String
}
```

This is compile-time contract composition, not inheritance. The composed contract's accessible
members become part of `Foo`'s effective contract, but behavioral requirements do not become stored
fields. `Foo` therefore supplies compatible `empty?`, `length`, `head`, and `rest` instance
functions. Read-only zero-argument contract functions support property syntax, so callers continue
to write `value.head`; functions with arguments use ordinary call syntax. `Foo` constructors only
initialize fields written in its effective stored-field contract.

Required callable members can be declared directly in a structural type:

```foster
type Identified = {
    pub func id(self) -> Int [read self]
    pub func offset(self, amount: Int) -> Int [read self]
}
```

A composing type implements these requirements with ordinary module-level `self` functions. The
compiler checks parameter ownership modes, result types, effects, suspension, and visibility.
Naming a contract is not required for conformance: another type with matching accessible fields and
methods is accepted structurally.

## Records

Records have nominal constructors and may have generic type parameters. Their accessible fields
also form structural contracts, as described below. Types and fields are private by default;
filesystem modules remain implicitly public:

```foster
pub type Person = {
    pub name: String
    pub age: Int
    internal_id: Int
}

let person = Person { name age internal_id }
```

Construction initializes every field exactly once. A record with any private field can only be
constructed inside its defining module. Field mutation is controlled by ownership and group access,
not by a `var` marker on the field. Generic records such as `Parsed<T>` participate in ordinary
constraint inference.

Functions may be associated with a record's type namespace by qualifying their declarations. They
do not receive an instance and are called through the type:

```foster
pub type Map<K, V> = {
    entries: List<Entry<K, V>>
}

pub func Map.empty<K, V>() -> Map<K, V> {
    Map { entries: [] }
}

let scores = Map.empty()
```

Associated functions are declared in the record's defining module, so they may construct records
whose representation contains private fields. The qualifier must name a record in that module.
An associated declaration cannot have a `self` parameter; instance methods retain the existing
`func get(self: Map<K, V>, key: K)` form. Both directly imported `Map.empty()` and explicitly
module-qualified `map.Map.empty()` calls resolve to the same function.

## Closed variants

Closed variants use alternatives and are consumed with exhaustive pattern branching:

```foster
type Result<T, E> =
    | Ok(T)
    | Error(E)
```

Alternatives may have zero or more positional payload values. Constructors are qualified by their
type, such as `Result.Ok(42)` and `Option.None`. Generic arguments are inferred from constructors,
function calls, and branch patterns.

A variant may place shared contract clauses after its alternatives:

```foster
type Foo =
    | Bar
    | What
    & SomeContract
    & {
        pub func describe(self) -> String
    }
```

The trailing intersection applies to every alternative. The declaration is equivalent, as a
contract, to:

```text
(Bar & SomeContract & { describe })
    | (What & SomeContract & { describe })
```

Consequently, every `Foo` value satisfies `SomeContract` and provides `describe`, regardless of
which alternative constructed it. The defining module implements a shared requirement with an
ordinary instance function whose receiver is the variant type, such as
`func describe(self: Foo) -> String`. Its body may branch on `self` when alternatives need
different behavior. A variant can be structurally adapted to the method-only contracts it
satisfies, and calls through such a contract dispatch to the original variant value.

The shared `{ ... }` body declares callable requirements only. Stored fields are rejected because
variant alternatives carry their own positional payloads; there is no additional record storage
shared by every alternative.

An alternative may be written without its type qualifier when its name uniquely identifies an
alternative in the current module. This applies to both constructors and patterns, allowing
`Ok(value)` and `Error(error)` in code centered on one result type. If two variant types declare the
same alternative name, Foster requires the qualified spelling.

## Branch expressions

`branch` is an expression. Conditional branches use `_` as their required catch-all arm.

```foster
branch {
    value < 0 -> :negative
    value > 0 -> :positive
    _ -> :zero
}
```

Supplying a subject changes the arms from conditions to patterns:

```foster
branch result {
    Result.Ok(value) -> value
    Result.Error(message) -> 0
}
```

The implemented patterns are variant patterns, recursive positional payload patterns, bindings,
`_`, and Bool, Int, Float, String, and Symbol literals. Branches over closed variants are checked
for exhaustiveness. A variant alternative is covered only when all of its payload patterns are
irrefutable bindings or `_`; for example, `Some(value)` covers `Some`, while `Some(0)` does not. A
top-level binding or `_` is a catch-all.

## Remote objects and virtual threads

`remote` transfers a record into an isolated virtual thread. A function declared in the record's
module whose first parameter is named `self` is an instance method. Calling that method through a
`Remote<T>` handle sends a FIFO mailbox message and returns `Future<R>`; `await` parks the current
virtual thread until the reply arrives.

```foster
func increment(self: Counter, amount: Int) -> Int {
    self.value = self.value + amount
    self.value
}

let counter = remote Counter { value: 0 }
let updated = await counter.increment(1)
```

The remote object retains mutations to `self` between calls. Values crossing the mailbox boundary
must be owned message values; references, closures, and futures cannot be transferred. Remote
handles can be transferred. Futures are single-consumption values and may be awaited once.

`remote ref value` creates a remote read-only loan instead of transferring ownership. The handle
retains a live view of the owner's record, so later owner mutations are visible to subsequent
remote reads. Read-only remote handles may call only methods whose `self` effects are `read`.
Multiple reads may coexist; owner mutation takes exclusive group access for the duration of the
method call, preventing a remote reader from observing a partially updated record.

```foster
let catalog = Catalog { entries: [] }
let reader = remote ref catalog
catalog.add("Foster")
let found = await reader.contains("Foster")
```

The resulting type retains the borrowed group as `Remote<ref[group] Catalog>`. Read-only describes
the handle's capability, not permanent immutability of the underlying value.

Borrow-mode remote method arguments use the same mechanism for a shorter lifetime. Because object
parameters borrow by default, `worker.inspect(document)` sends a live read-only capability;
`worker.submit(move document)` transfers ownership only when the parameter consumes it. The
temporary loan begins when the worker starts the invocation and ends when that invocation returns,
independently of when its future is awaited. Borrowed arguments cannot be mutated, consumed, stored
in actor state, or returned across the mailbox boundary.

## Static types

Foster is statically typed and uses compile-time duck typing. Every expression has a type before
execution, and missing or incompatible contract members are compile errors; there is no dynamic
member lookup implied by “duck typing.” Records retain nominal construction and private
representation, while their accessible contract participates in structural conformance.

The implemented type system includes nominally constructed records with structural adaptation and
declared contract composition, closed variants, explicit parametric generics using
`Type<Argument>`, function and intersection types, callable-member contracts, and no implicit
numeric or nullable conversions.

Types, traits, and functions may be qualified by modules. The HIR resolves every source-level name
to a local binding, function, module, builtin, or later a type-level definition before type checking.

The bootstrap compiler resolves `Unit`, `Bool`, `Int`, `Float`, `CodePoint`,
`List<T>`, `Sequence<T>`, `Remote<T>`, `Future<T>`, callable types with internally inferred
representation erasure, records,
variants, generics, and record intersections. Decimal and scientific-notation literals produce
`Float`; there are no implicit conversions between `Int` and `Float`.
`String`, `Symbol`, `Bytes`, `ByteBuffer`, and `List<T>` are instead always-available opaque Foster
types declared in their respective core modules. `String` contains private `Bytes`, `Symbol`
contains private `String`, and the collection types contain private implementation-only storage:
`RawBytes`, `RawByteBuffer`, and `RawList<T>`. Literals and trusted constructors lower to the
nominal Foster types; the raw storage types cannot be named by user modules.
`ByteBuffer` construction and mutation are ordinary Foster record operations. Its private core
implementation declares its host boundary explicitly with signatures such as
`func RawByteBuffer.empty() -> RawByteBuffer = intrinsic("byte_buffer.empty")`, then calls those
operations through ordinary associated-function and method syntax. Stable intrinsic keys decouple
source names from VM dispatch. Intrinsic declarations have registered VM implementations, no
Foster body, and neither construct nor recognize the public wrapper. Host-backed storage is
declared explicitly, for example `intrinsic type RawByteBuffer`; it is opaque and cannot be
constructed with record syntax.
Representation-level operations such as functional `List.append`, checked `from_code_point(Int)`,
and `parse_float(String)` form the narrow primitive boundary beneath the Foster-written core
library. The older `code_point(CodePoint)` intrinsic is also accepted for compatibility; source
code normally uses integer operators directly.
It performs constraint inference across function calls and records a canonical type for every HIR
expression, local, and function signature. Explicit generic functions use
`func identity<T>(value: T) -> T`; their parameters are rigid while checking the body and freshly
instantiated at each call. It checks operators, calls, branch results,
returns, list construction, and the implemented standard members. An unconstrained type is an error
and asks for an annotation.

This is not Hindley–Milner generalization: an unannotated function receives one inferred type within
a compilation rather than becoming implicitly polymorphic. Polymorphism is always explicit.
Type parameters use angle brackets and group parameters use a following square-bracketed section:
`func map<T, U>(...)` declares types, while `func inspect[items: group T](...)` declares a group.
Functions needing both use `func inspect<T>[items: group T](...)`. A function may not declare
either category twice or reuse one name across both categories.

## Standard library — explicit imports

Foster has no prelude. The compiler embeds Foster-written modules under two roots: `core` contains
foundational language types, while `std` contains general-purpose collections, I/O, filesystem,
path, environment, and networking facilities. Tools can resolve both roots consistently, but no
declaration is injected into user scope. Programs import every module they use. The supported
surface and runtime boundary are documented in `docs/core-library.md`.

## Comments and documentation

`//` starts an ordinary line comment. `/* ... */` is a block comment and may be nested. Ordinary
comments do not enter the AST and have no effect on compilation.

`//!` is a module documentation comment and must appear before the module's declarations. Consecutive
module documentation comments are joined with newlines. `///` and `/** ... */` are declaration
documentation comments. Consecutive declaration documentation comments are joined
with newlines and attach to the function, record, or variant type that immediately follows them:

```foster
/// A TCP connection owned by the runtime.
///
/// Obtain one with `connect` or `accept`.
pub type Connection = {
    handle: Int
}
```

Documentation text is Markdown. The compiler retains it in AST and HIR, and the language server
includes it in hover information and completion items. A documentation comment that does not
precede a declaration is an error.

## Structural conformance, composition, and intersections

Records have nominal constructors but public fields form a statically checked structural contract.
When a record value is used where another record type is expected, Foster accepts it when it has
every accessible field required by the destination type with the same field type. Additional fields
remain on the value but are hidden by the destination's static view:

```foster
type Named = {
    pub name: String
}

type User = {
    pub name: String
    pub email: String
}

func display(value: Named) -> Int {
    value.name.length
}

display(User { name: "Mina", email: "mina@example.com" })
```

Adaptation is resolved entirely during type checking. It performs no runtime shape test, allocation,
or field copy. Borrowed arguments borrow the original value. A consuming destination moves the
original value and narrows the fields visible through the resulting type.

This is Foster's static duck typing rule: a source type conforms to a destination type when its
accessible contract contains compatible members for everything the destination requires. A
contract includes accessible fields and callable members, including their generic parameters,
parameter ownership modes, result types, effects, and suspension behavior. Private representation
does not participate outside its defining module.

A type declaration may explicitly compose and assert contracts with right-hand-side `&` clauses:

```foster
type TextCursor = & Sequence<CodePoint> & {
    source: String
    offset: Int
}
```

The declaration imports the accessible `Sequence<CodePoint>` requirements into `TextCursor`'s
effective contract. The requirements are functions, not record storage, so the constructor only
initializes `source` and `offset`; the defining module supplies compatible `empty?`, `length`,
`head`, and `rest` implementations. Conformance remains structural: a different type whose fields
or accessor methods satisfy the same readable contract may be passed to the same functions without
declaring `& Sequence<CodePoint>`.

`A & B` is an intersection contract requiring the accessible fields of both record types:

```foster
func locate(value: Named & Located) -> String {
    value.name + value.location
}
```

The bootstrap implementation accepts record and `Sequence<T>` contracts in intersections.
Overlapping fields and methods must have compatible contracts. Declaration-side composition
contributes requirements once and rejects missing or incompatible implementations. The same
structural rules apply at calls, returns, and assignments. Structural adaptation never exposes an
inaccessible private member, so records with private representation remain encapsulated outside
their defining module. `&` does not add a wrapper or establish a nominal subtype chain; contract
method calls dispatch against the original runtime record.

## Iteration contracts

Iteration is expressed with two Foster-written callable contracts from `std.iter`, rather
than a compiler-owned protocol:

```foster
import core.option

pub type Iterator<T> = {
    pub func next(self) -> Option<T> [mut self]
}

pub type Iterable<T> = {
    pub func iterator(self) -> Iterator<T>
}
```

`Iterator<T>` is stateful. Each `next()` call has exclusive mutation access to the iterator and
returns `Option.None` after exhaustion. `Iterable<T>` is repeatable: its read-only `iterator`
accessor creates an independent iterator. Because zero-argument read-only contract functions use
property syntax, an iterable is opened with `value.iterator`; `next` remains call syntax because
it mutates its receiver.

Both contracts use the same static duck typing and zero-conversion dispatch as other composed
types. A concrete type implements them with `type Cursor<T> = & Iterator<T> & { ... }` or
`type Collection<T> = & Iterable<T> & { ... }`. The core adapter
`Iterator.from_sequence(values)` consumes a `Sequence<T>` into an independent iterator, so lists,
strings, and user-defined sequence implementations can participate immediately.

The standard collection hierarchy is behavioral rather than representational:

```text
Iterable<T>
└── Collection<T>
    ├── Sequence<T> → List<T>, String as Sequence<CodePoint>, Range<T>
    ├── Set<T>
    ├── Queue<T>
    ├── Deque<T>
    └── Stack<T>

Map<K, V> & Collection<Entry<K, V>>
```

`List`, `String`, and `Sequence` expose `.iterator` as a compiler-backed intrinsic whose public
contract is an ordinary borrowed accessor. The VM creates an independent cursor over the source's
read-only value view. Advancing the cursor mutates only cursor state, while explicit
`Iterator.from_sequence` remains the ownership-transferring form.

## Binary values

Foster separates one bounded octet, immutable binary data, and mutable construction storage:

```foster
let byte = Byte.from(255)
let data = Bytes.from_hex("89504e47")

let buffer = ByteBuffer.with_capacity(4096)
buffer.extend("Foster".utf8)
let snapshot = buffer.snapshot
let finished = (move buffer).freeze()
```

`Byte` is a copy type in the inclusive range `0..255`. Ordinary arithmetic widens it to `Int`;
bitwise and shift operators retain `Byte`. `Bytes` is an opaque Foster type over immutable
contiguous raw storage, implementing
the read-only `Sequence<Byte>` and `Collection<Byte>` behavior. `ByteBuffer` is mutable and
growable, but deliberately has no implicit position or limit; stateful reading can be introduced
separately as a cursor contract.

Passing all three types borrows by default. A buffer mutation requires `mut` access, indexed loans
are invalidated by structural changes, and converting a buffer without copying requires an
explicit move through `(move buffer).freeze()`. `buffer.snapshot` is the copying alternative.
Strings never convert to bytes implicitly: `.utf8` encodes, and `String.from_utf8` performs checked
decoding.

## Stream contracts

Stream behavior is expressed with generic structural contracts rather than a common resource base
class:

```foster
pub type Reader<E> = {
    pub func read(self, maximum: Int) -> Result<Bytes, E> [mut self]
}

pub type Writer<E> = {
    pub func write(self, contents: Bytes) -> Result<Int, E> [mut self]
    pub func flush(self) -> Result<Unit, E> [mut self]
}
```

`TextReader<E>` and `TextWriter<E>` provide the corresponding text operations. The error parameter
allows a file to expose `IoError` while a socket exposes `NetworkError`; no universal I/O error is
required. Empty bytes or text signal clean EOF. A binary write may be partial and therefore returns
the number accepted. Successful non-empty reads and writes must make progress.

Mutable parameters are borrowed places in VM call frames. Consequently, calling a generic helper
such as `stream.copy(reader, writer)` mutates the original stateful values rather than temporary
copies. Consuming parameters still transfer ownership, and read-only parameters retain ordinary
borrow behavior.

## Equality, ordering, and hashing contracts

`core.ordering` separates comparison capabilities from the `Ordering` result value:

```foster
pub type Equality<T> = {
    pub func equal?(self, other: T) -> Bool
}

pub type Ordered<T> = & Equality<T> & {
    pub func compare(self, other: T) -> Ordering
}

pub type Hashing = {
    pub func hash(self) -> Int
}
```

`Ordered<T>` composes `Equality<T>`, so a conforming concrete type must provide both `equal?` and
`compare`. `Hashing` is separate because ordered values do not necessarily need hashing and hashed
values do not need a total order. These are statically checked, structurally dispatched contracts;
they add no hidden fields or wrappers.

A type declaration containing bodyless method requirements is itself a contract, so it may compose
other contracts without supplying implementations. A concrete type has no bodyless requirements;
when it composes a contract, its module must provide all inherited functions.

Implementations must preserve the usual laws: equality is reflexive, symmetric, and transitive;
`compare` returns `Ordering.Equal` exactly when `equal?` is true; and equal values produce the same
hash. Hash collisions between unequal values are valid. The compiler checks member signatures and
effects, while these semantic laws remain the implementation's responsibility. Because `hash` is
a zero-argument read-only member it uses property syntax (`value.hash`); `equal?` and `compare`
take arguments and use call syntax.

## Ownership and groups

Every value has one owner: a local, containing value, collection, allocation, or global. Moving a
value transfers ownership and leaves the source uninitialized until it is assigned again.

A reference is parameterized by a group describing its possible target locations:

```foster
ref[people] Person
```

Reference types do not contain mutability. Mutation is an effect performed by a function:

```foster
func rename[people: group Person](
    person: ref[people] Person,
    name: String,
) -> Unit [mut people] {
    person.name = name
}
```

References in the same group may alias, including during mutation. Safety comes from tracking which
places may be invalidated, rather than enforcing aliasing-xor-mutability on individual references.

Effect groups may be projected through stable fields and dynamic-container storage:

```foster
mut entities.rings
reshape entities.rings.items
```

The compiler distinguishes:

- value mutation, which preserves storage identity;
- structural mutation, which may move or destroy child-group members;
- consumption, which uninitializes a place;
- initialization, which makes a place usable again.

Ordinary call arguments borrow by default. Ownership-taking parameters are explicit in the
function contract, and an existing source place must be transferred explicitly:

```foster
func enqueue(job: Job) -> Unit [consume job] { /* ... */ }

enqueue(move pending_job)
```

Copy values and fresh temporaries do not require `move`. Named `ref[group] T` types are reserved
for borrows that participate in first-class references, escaping relationships, captures, or group
effects rather than routine parameter passing.

Function types carry the same contract by parameter position:

```foster
func(Job) -> Unit         // borrows its argument
func(consume Job) -> Unit // takes ownership of its argument
```

## Inferred and explicit effects

Ordinary functions infer `read`, `mut`, `reshape`, `consume`, and `suspend` from their bodies and
the functions they call. Inference runs to a fixed point for recursive functions. The inferred row
is stored in typed HIR and callable bytecode information, so later ownership checks, remote calls,
and VM lowering retain the contract without source-level suffixes:

```foster
func restock(self: Inventory, amount: Int) -> Int {
    self.count = self.count + amount
    self.count
}
```

When a contract must be written because there is no concrete body, it follows the return type in a
bracketed, comma-separated clause:

```foster
func(Inventory) -> Int [mut inventory, suspend]
```

Explicit function contracts use the same form and act as checked upper bounds. Loose tokens after
the return type are not valid syntax.

Declaration names are normalized to these positional modes before a callable is stored or erased,
so indirect calls and partial applications do not lose ownership information.

Foster has no surface keyword for callable erasure. `func(...) -> ...` always describes the
required callable contract. The compiler decides whether a particular value remains a direct
function, is specialized, or needs a representation-erased closure environment.

The compiler implements moves, copy/move/reference closure captures, borrowed-result escape checks,
projected-reference invalidation, group-effect derivation, and ownership-safe remote transfer. See
[Ownership and borrowing](ownership.md) for the source model, compiler passes, runtime backstops,
and implementation limits.

## Errors as values

Recoverable errors are ordinary typed values, conventionally represented with the
Foster-written `Result<T, E>` closed variant:

```foster
import core.result

func parse(input: String) -> Result<Json, JsonError> {
    branch parse_value(input) {
        Result.Ok(value) -> Result.Ok(value)
        Result.Error(error) -> Result.Error(error)
    }
}
```

The VM host boundary follows the same rule for `std.fs`, `std.path`, `std.env`, and `std.net.tcp`. The language does not
provide dedicated `throw` or typed error-effect syntax.

## Module initialization

Module bodies contain declarations and compile-time constants, not arbitrary runtime startup code.
Resources are created by explicit functions. This avoids observable import order and runtime module
initialization cycles.

Because modules contain no runtime initialization, declarations in different modules may refer to
one another when name and signature resolution can settle the cycle.

## Filesystem and network access

`std.fs` exposes typed UTF-8 file and directory operations. `std.path` provides platform path
operations, and `std.env` provides process environment queries. Fallible operations return
`Result<..., IoError>`, using the shared error type from `std.io`.
`std.net.tcp` exposes opaque listeners and connections with typed `NetworkError` results. Their
public records and wrappers are Foster code; private VM intrinsics perform the host operations.

These modules use process-wide host capabilities.

## Compiler pipeline

The implemented pipeline is:

```text
source -> tokens -> AST -> resolved HIR -> type/effect inference
       -> loan/group/capture checks -> ownership MIR validation
       -> structured register bytecode
            -> optional optimizer -> verifier -> VM
            -> supported-subset validation -> Cranelift AOT -> host executable
```

The register VM is the complete executable semantic reference. The initial native backend compiles
the reachable primitive-value subset described in [Native compilation](native.md). Bytecode is
lowered from checked HIR after ownership MIR validation.

Group information is normally erased before bytecode execution, but its consequences—moves,
storage identity, and valid optimization facts—are represented by checked HIR, ownership MIR, and
concrete VM operations.

## Focused design documents

- [Roadmap](roadmap.md)
- [Ownership and borrowing](ownership.md)
- [Closures and group borrowing](closures.md)
- [Effect derivation](effect-derivation.md)
- [Virtual machine](vm.md)
- [Native compilation](native.md)
