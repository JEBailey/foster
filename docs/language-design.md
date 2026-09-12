# Foster Language Design

For the backend-independent contract, stable rule IDs, and unresolved semantic questions, see the
[semantic specification](semantics.md). This document remains the implemented syntax inventory.

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

A project may select that source root with `package.source` in a `foster.toml` manifest. The path
is relative to the project directory and defaults to `src`. Without a manifest, a directory passed
directly to a Foster command remains the source root.

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
    parser::parse(source)
}
```

An import makes the module's public declarations available directly and also binds its final path
component as a module qualifier. Thus `import core.option` permits both `Option<T>` and
`option::Option<T>`. Same-module declarations take precedence. If multiple imported modules expose
the same unqualified name, Foster requires a module-qualified use at that point; importing the
modules themselves remains valid.

Module qualification uses `::`, while type accessors and runtime value access use `.`. Module
functions, qualified constants, and qualified type names therefore use `::`; associated functions,
enum cases, fields, and instance methods use `.`:

```foster
let decoded = parser::parse(source)
let outcome = Result.Ok(decoded)
let name = user.name
user.rename("Ada")
```

Library operations with one natural nominal receiver are instance methods, such as
`values.map(transform)`, `outcome.map(transform)`, and `document.get(key)`. A module function is
reserved for construction or an algorithm without one nominal receiver, such as
`toml::parse(source)`, `sequence::map(values, transform)`, or `io::copy(reader, writer)`.

Canonical module identities in imports remain dotted, such as `import std.net.tcp`. This keeps
filesystem module paths visually distinct from selecting a declaration through a module.

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
compiler infers them. Local values use inference. The final statement's value is the function
result: expressions provide their value, bindings and assignments provide the bound or assigned
value, and nested function declarations provide the declared function. Assertions and completed
loops provide `()`. Empty function, method, closure, and test bodies also produce `()`.
A guarded return that does not transfer control and has no following result produces `()`;
use `return ()` for an explicit early unit return. Declared result types must match these values.

Assignment evaluates its complete right-hand expression before evaluating the left-hand place.
The destination is then evaluated exactly once and replaced. For example,
`values[index()] = replacement()` calls `replacement` before `index`.

Other multi-operand expressions evaluate from left to right: callable before arguments, method
receiver before arguments, collection before index, and aggregate initializers in source order.
Failure or control transfer skips operands that have not yet begun.

Locals, declared stored fields rooted in places, and indexed elements rooted in places designate
storage. Compiler-provided computed members produce values even when their implementation shares
storage. They cannot be assigned to. Moving from a stored place invalidates that place; applying
`move` to an already-produced owned value only makes the transfer explicit. Foster currently uses
methods for user-defined computation rather than getter/setter declarations.

`test` introduces a private test declaration identified by a non-empty string. Tests take no
arguments and return `()`:

```foster
test "decoding preserves text" {
    let decoded = decode("Foster".bytes)
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

### Function and method overloads

Functions, associated functions, and instance methods may share a name when their parameter
signatures differ. Every parameter of an overloaded declaration must have an explicit type.
Resolution first filters by argument count and then by compatible parameter types:

```foster
func render(value: Int) -> String { "integer" }
func render(value: CodePoint) -> String { "code point" }
func render(left: Int, right: Int) -> String { "pair" }
```

An exact parameter match is preferred over a match requiring conversion, so `render('x')` selects
the `CodePoint` overload rather than widening the argument to `Int`. If equally specific candidates
remain, the call is ambiguous and compilation fails. Return types, ownership/effect declarations,
and suspension do not distinguish overloads; declarations that differ only in those properties are
duplicate parameter signatures. Referring to an overload set without calling it is also ambiguous.

Contract composition merges repeated method requirements with identical complete signatures.
Requirements with the same name but different parameter signatures remain overloads. The call's
arguments select a signature statically, and contract dispatch invokes that signature on the
concrete receiver at runtime.

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

The `executable: String` field of `Arguments` is the path or command used to invoke the program.
Its `values: List<String>` field contains the remaining values and does not repeat the executable.
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

`assert` requires a `Bool` condition and may include a `String` message:

```foster
assert(index < values.length)
assert(response.success?, "request should succeed")
```

When the condition is `true`, execution continues and the assertion contributes `()`. When it is
`false`, the current invocation stops immediately with a runtime assertion error. The error
propagates through ordinary calls; a remote invocation delivers it through its future, and a test
runner marks only the current test as failed before continuing with the remaining tests. Assertions
are statements rather than recoverable `Result` values and cannot be guarded with postfix `if`.

`if` is deliberately not a general conditional statement or expression. It may only follow a
control-transfer statement, so `write(value) if ready` and `value = next() if ready` are invalid.
Use `branch` when choosing whether to evaluate a value-producing operation.

`while condition { ... }` checks a Boolean condition before each iteration. It is syntax sugar for:

```foster
loop {
    break if !condition
    // body
}
```

A false initial condition skips the body; `continue` returns to the condition check. The condition
is evaluated once per iteration, and the loop produces `()` when it exits.
Body declarations are local to the loop, for both `while` and `loop`; assignments to
existing outer bindings remain visible after the loop.
Record literals in a loop header may appear inside call arguments, parentheses, lists,
and index expressions. Parenthesize a record literal used directly as the header expression
to distinguish its braces from the loop body.

`for item in collection { ... }` iterates a collection through its `.iterator()` method:

```foster
for item in [1, 2, 3] {
    continue if item == 2
    println(item)
}
```

The collection expression and `.iterator()` call are each evaluated once. A scoped cursor calls
`.next()` before each iteration: `core.option.Option.Some(item)` runs the body and `Option.None`
ends the loop. Built-in collections need no explicit iteration import. Custom collections must
provide `.iterator()` returning a cursor whose `.next()` returns `Option<T>`; a bare iterator
without `.iterator()` is not an iterable.

The item binding exists only inside the body and may shadow an outer name; `_` discards the item.
Conceptually, the syntax expands to the following, with a compiler-generated cursor name and
an enclosing scope that keeps the cursor private:

```foster
let cursor = collection.iterator()
loop {
    branch cursor.next() {
        Option.None -> { break }
        Option.Some(item) -> {
            // body
            ()
        }
    }
}
```

`next()` is evaluated once per iteration. The `Some` pattern unwraps the yielded value, so
`item` has the element type rather than `Option<T>`. `None` exits before the body runs.
The generated patterns resolve to `core.option.Option` even if a user declares another `Option`.

The body value is discarded and the loop produces `()`. `break` exits the loop, `continue`
advances to the next item, and `return` leaves the enclosing function. Nested loops use the nearest
enclosing loop for `break` and `continue`.

`loop` repeats a statement block until control leaves it. `break` exits the nearest enclosing loop,
and `continue` starts its next iteration. Both transfers
may use the same postfix `if` guard as `return`:

```foster
loop {
    item = next()
    continue if item.ignored?
    process(item)
    break if item.last?
}
```

`break` and `continue` do not carry values. A loop that exits contributes `()`, keeping
value-producing selection in `branch`. Their guards must be `Bool`, and using either transfer
outside an enclosing loop is a compile error. Foster has no `throw` statement;
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
  `Sequence<String>`
- unit, written `()`

There is no universally nullable reference type. The core library represents absence with
`Option<T>`:

```foster
enum Option<T> = Some(T)
    | None
```

`Sequence<T>` is a read-oriented structural view, not a storage representation. Passing a list or
string to a sequence parameter retains the original runtime value and ownership. Its common
members are `empty?`, `length`, `head`, and `rest`. For strings, `head` returns one extended grapheme cluster as a `String` and
`rest` remains a `String` when accessed directly; through a `Sequence<String>` parameter,
`rest` has sequence type. A code point has the primitive `.whitespace?` query. Importing
`core.code_point` adds the public `.as_string()` and `.as_int()` methods. `CodePoint` is a
bounded integer-like primitive: it widens losslessly where `Int` is expected, integer arithmetic
and comparisons promote its Unicode scalar value, and arithmetic produces `Int`. This permits
direct arguments and results such as `func ordinal(character: Int) -> Int { character }` called as
`ordinal('A')`, as well as expressions such as `'9' - '0'` and `character < 32`. Conversion from an
arbitrary `Int` remains checked because surrogate values and values above `0x10FFFF` are not Unicode
scalar values. The `core.code_point` module attaches owner-qualified operations to the primitive,
making imported operations available through calls such as `character.as_int()` and
`character.as_string()`, with checked construction available through `CodePoint.from(value)`.
The bootstrap compiler supplies the `String` and `List<T>` conformances. A type definition begins
after `=`, and each composed contract is aligned with `&` on the right-hand side:

```foster
type Foo = & Sequence<String> & {
    source: String
}
```

This is compile-time contract composition. The composed contract's accessible
members become part of `Foo`'s effective contract, but behavioral requirements do not become stored
fields. `Foo` therefore supplies compatible `empty?`, `length`, `head`, and `rest` instance
functions. Composed types can also supply reusable default bodies, as described under
[ordered default implementations](#ordered-default-implementations). Methods always use call
syntax, including zero-argument methods such as `value.head()`.
This keeps method invocation distinct from stored-field access. `Foo` constructors only initialize
fields written in its effective stored-field contract.

Required callable members can be declared directly in a structural type:

```foster
type Identified = {
    pub func id(self) -> Int [read self]
    pub func offset(self, amount: Int) -> Int [read self]
}
```

A composing type implements these requirements with receiver functions inside its `impl` block. The
compiler checks parameter ownership modes, result types, effects, suspension, and visibility.
Naming a contract is not required for conformance: another type with matching accessible fields and
methods is accepted structurally.

Method result annotations may use `self`, including inside a generic result such as
`Option<self>` or `Option<QueueItem<T, self>>`. It denotes the implementing receiver type;
adapting a value to a structural contract presents that result through the contract view.

The declarations inside `type` define what is required; the bodies inside `impl` define what is
implemented. An additional public function written only in `impl` is available on that concrete
type, but does not become a requirement when another type composes its contract. Associated
constructors likewise remain in `impl`. Composition carries requirements and eligible public
instance-method bodies under the [ordered default implementation rules](#ordered-default-implementations).

Required functions can introduce their own type parameters, independently of the enclosing type:

```foster
type Transform<T> = {
    pub func map<U>(self, transform: func(T) -> U) -> List<U>
}
```

An implementation must satisfy this requirement for every `U`; a function specialized to one
result type is insufficient. Each call instantiates method type parameters independently. Required
method type parameters must not shadow the enclosing type's parameters. Method-level ownership
group parameters are not supported yet.

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

Methods and associated functions are declared inside `impl Type { ... }` blocks. The block
supplies the owning type; member names are unqualified. A first `self` parameter makes a member
an instance method. Its type may be omitted when the block supplies the complete receiver type.
A record member without `self` is an associated function, called through the type:

```foster
pub type Box<T> = { value: T }

impl Box<T> {
    pub func new(value: T) -> Box<T> { Box { value } }
    pub func get(self) -> T { self.value }
    pub func map<U>(self, transform: func(T) -> U) -> Box<U> {
        Box { value: transform(self.value) }
    }
}

func example() -> Int { Box.new(42).get() }
```

Block type parameters are available to every member and precede member-specific type parameters.
A member cannot redeclare a block parameter. A block may also omit type parameters, with generic
members declaring their own parameters and explicitly annotating `self`, such as
`impl Box { func get<T>(self: Box<T>) -> T { self.value } }`.

Multiple blocks may group different operations for the same type. They share the type's member
namespace and the ordinary overload and duplicate-declaration rules. Visibility and documentation
belong to individual members; a block does not change access or structural conformance. Blocks
contain function declarations only, including intrinsic bindings, and appear at module scope.
Ordinary module functions remain outside blocks. Qualified declarations such as `func Box.get`
are rejected.

Associated functions are declared in the record's defining module, so they may construct records
whose representation contains private fields. Existing method ownership and import rules also
apply inside blocks. Calls retain their existing spelling: `Box.new(42)`, `value.get()`, or
`module::Box.new(42)`. Callable requirements remain in type declarations.

## Type aliases and enums

A `type` definition describes one type: a record, a composed contract using `&`, or a
transparent alias. It cannot declare alternatives with `|`, `or`, or `||`.

```foster
type Text = String
type Items<T> = List<T>
```

Aliases preserve the target type's operations and runtime representation. Alternatives belong
to enums and require explicit case construction and matching. For example:

```foster
enum TextOrBytes = Text(String) | Binary(Bytes)

func length(value: TextOrBytes) -> Int {
    branch value {
        TextOrBytes.Text(text) -> text.length
        TextOrBytes.Binary(bytes) -> bytes.length
    }
}
```

Tagged cases use a distinct `enum` declaration. Each case has a label and optionally carries one
explicit payload type:

```foster
enum Result<T, E> = Ok(T)
    | Error(E)
```

The labels are scoped enum cases, not names of other types. `Ok(T)` declares a case named `Ok`
whose payload is a `T`; `None` declares a payloadless case. Multiple related fields are grouped in
a record and carried as that one record value. Enums synthesize explicit
constructors such as `Result.Ok(value)` and `Result.Error(error)`. Constructors insert a runtime
tag, and subject branches may match those tags exhaustively. Enum values are nominal, and generic
arguments are inferred from construction, calls, and branch patterns.

An enum may place shared contract clauses after its cases:

```foster
enum Foo = Bar(String)
    | What
    & SomeContract
    & {
        pub func describe(self) -> String
    }
```

The trailing intersection applies to the enum value itself, independent of which case constructed
it. Consequently, every `Foo` value satisfies `SomeContract` and provides `describe`. The defining
module implements a shared requirement with an
ordinary instance function whose receiver is the enum type, such as
`func describe(self) -> String` inside `impl Foo { ... }`. Its body may branch on `self` when cases need
different behavior. An enum can be structurally adapted to the method-only contracts it satisfies,
and calls through such a contract dispatch to the original enum value.

The shared `{ ... }` body declares callable requirements only. Stored fields are rejected because
each case owns only its declared payload; there is no additional record storage shared by every
case.

An enum case may be written without its enum qualifier when its label uniquely identifies a case
in the current module. This applies to both explicit construction and
patterns, allowing `Ok(value)` and `Error(error)` in code centered on one result type. If two enums
declare the same case label, Foster requires the qualified spelling.

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

The implemented patterns include enum-case patterns, nested payload patterns,
bindings, `_`, and Bool, Int, Float, String, and Symbol literals. Branches over enum types are
checked for exhaustiveness. An enum case is covered only when all of its nested patterns are
irrefutable bindings or `_`; for example, `Some(value)` covers `Some(T)`, while `Some(0)` does not.
A top-level binding or `_` is a catch-all.

An arm may use a statement block after `->`. The block's final expression supplies the arm value:

```foster
branch result {
    Result.Ok(value) -> {
        log("received a value")
        normalize(value)
    }
    Result.Error(message) -> {
        log(message)
        fallback
    }
}
```

The final statement may instead be an unconditional control transfer such as `return`, or `break`
and `continue` when the branch is inside a loop:

```foster
loop {
    branch next() {
        Item(value) -> {
            continue if !acceptable?(value)
            return value
        }
        End -> { break }
    }
}
```

`continue` always targets the nearest enclosing loop; branch arms never fall through to later arms.
An arm block that completes without a final value expression produces `()`. Unlike function bodies, branch-arm blocks do not
use a trailing binding or assignment as their result. An unconditional control transfer
leaves the arm instead of producing a result.

## Logical operators

`!value` and `not value` are equivalent prefix logical-negation expressions. Both accept a `Bool`
operand, return `Bool`, and bind more tightly than binary operators.

`&&` and `||` also accept `Bool` operands and return `Bool`. They evaluate from left to right and
short-circuit: `left && right` evaluates `right` only when `left` is `true`, while `left || right`
evaluates `right` only when `left` is `false`.

```foster
let valid = index >= 0 && index < values.length
let available = cached? || load_from_disk()
let unavailable = not available
```

`&&` binds more tightly than `||`, and both bind less tightly than equality, comparison, bitwise,
shift, and arithmetic operators. Parentheses can make a different grouping explicit.

## Remote objects and virtual threads

The section below describes the current executable interface. Both runtimes implement the
[remote lifecycle contract](remote-semantics.md), including owner-scoped cancellation, terminal
failure containment, and typed error outcomes. Compile-time outstanding-request checks reject
unproven completion at owner destruction with `E0730`; the
[supported static proofs](remote-semantics.md#supported-static-proofs) describe their conservative limits.

`remote` transfers a record into an isolated virtual thread. An owner-qualified function whose
first parameter is the semantic `self` receiver is an instance method. Calling that method through a
`Remote<T>` handle sends a FIFO mailbox message and returns `Future<Result<R, RemoteError>>`; `await` parks the current
virtual thread until the reply arrives. `Result.Ok` contains the method result, while
`Result.Error(RemoteError.Failed(message))` describes an execution failure. Import `core.result`
and `core.remote_error` to name these variants. Domain Result errors remain inside the outer Ok.

```foster
impl Counter {
    func increment(self: Counter, amount: Int) -> Int {
        self.value = self.value + amount
        self.value
    }
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
declared contract composition, transparent type aliases, tagged enums, explicit parametric generics using
`Type<Argument>`, function and intersection types, callable-member contracts, two lossless integer
widenings, and no implicit nullable conversions.

Types, traits, and functions may be qualified by modules. The HIR resolves every source-level name
to a local binding, function, module, builtin, or later a type-level definition before type checking.

The unit type is written `()`. The bootstrap compiler also resolves `Bool`, `Int`, `Float`, `CodePoint`,
`List<T>`, `Sequence<T>`, `Remote<T>`, `Future<T>`, callable types with internally inferred
representation erasure, records,
enums, type aliases, generics, and record intersections. Decimal and scientific-notation literals produce
`Float`; there are no implicit conversions between `Int` and `Float`.
`Byte` and `CodePoint` widen to `Int` when an assignment, stored field, argument, branch arm, or
function result has an expected `Int` type. The compiler records the conversion in typed output and
produces an `Int` value; it does not merely reinterpret the source value. Widening is not reversed,
does not lift through containers, and does not change unconstrained generic inference. Converting an
`Int` to `Byte` or `CodePoint` remains explicit and checked because not every integer is valid.
`String`, `Symbol`, `Bytes`, `ByteBuffer`, and `List<T>` are instead always-available opaque Foster
types declared in their respective core modules. `String` contains private `Bytes`, `Symbol`
contains private `String`, `Bytes` contains private compact `RawBytes`, `List<T>` contains private
`RawList<T>`, and `ByteBuffer` contains a private `List<Byte>`. Literals and trusted constructors
lower to the nominal Foster types; raw storage types cannot be named by user modules.
`ByteBuffer` construction, mutation, snapshotting, and freezing are ordinary Foster functions over
that list. Capacity is only an allocation hint and is not observable: the list-backed implementation
may accept `with_capacity` and `reserve` without retaining spare capacity.
Representation-level operations such as `List.push` and functional `List.append` are declared as
owner-qualified intrinsics. Calls resolve their `List` owner before the stable intrinsic key selects
the VM operation, so unrelated types remain free to define `push` or `append`. Their registry
entries declare whether the receiver is read, mutated in place, or consumed, allowing projected
receivers such as `buffer.value.push(byte)` to update their original aggregate. Checked
`from_code_point(Int)` and `parse_float(String)` complete the narrow primitive boundary beneath the
Foster-written core library. Code points convert to integers through `as_int()` and also widen in
expected `Int` contexts.
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
path, environment, networking, and exact/civil/zoned time facilities. Tools can resolve both roots consistently, but no
declaration is injected into user scope. Programs import every module they use. The supported
surface and runtime boundary are documented in `docs/core-library.md`.

## Comments and documentation

`//` starts an ordinary line comment. `/* ... */` is a block comment and may be nested. Ordinary
comments do not enter the AST and have no effect on compilation.

`//!` is a module documentation comment and must appear before the module's declarations. Consecutive
module documentation comments are joined with newlines. `///` and `/** ... */` are declaration
documentation comments. Consecutive declaration documentation comments are joined
with newlines and attach to the function, record, type-alias, or enum declaration that immediately follows them:

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
type TextCursor = & Sequence<String> & {
    source: String
    offset: Int
}
```

The declaration imports the accessible `Sequence<String>` requirements into `TextCursor`'s
effective contract. The requirements are functions, not record storage, so the constructor only
initializes `source` and `offset`; the defining module supplies compatible `empty?`, `length`,
`head`, and `rest` implementations. Conformance remains structural: a different type whose fields
or accessor methods satisfy the same readable contract may be passed to the same functions without
declaring `& Sequence<String>`.

`A & B` is an intersection contract requiring the accessible fields of both record types:

```foster
func locate(value: Named & Located) -> String {
    value.name + value.location
}
```

The bootstrap implementation accepts record and `Sequence<T>` contracts in intersections.
Overlapping fields and methods must have compatible contracts. Declaration-side composition
contributes each requirement once. A declaration does not need to provide implementations merely
to describe that contract; construction rejects a record whose concrete type lacks a required
method. The same structural rules apply at calls, returns, and assignments. Structural adaptation
never exposes an inaccessible private member, so records with private representation remain
encapsulated outside their defining module. `&` does not add a wrapper or establish a nominal
subtype chain; contract method calls dispatch against the original runtime record.

### Ordered default implementations

Declaration-side composition inherits public instance-method bodies from composed types
whose representation consists entirely of public fields (including types without fields).
Types with private storage contribute their public contract; their own bodies remain tied
to their concrete representation and are not inherited as defaults.
Components are expanded from left to right, including their own composed defaults. For each
overload, the rightmost compatible implementation wins. A method supplied by the resulting
type's own `impl` block has final precedence:

```foster
pub type Bar = { pub func fighter(self) -> Int }
pub type Monkey = { pub func fighter(self) -> Int }
impl Bar { pub func fighter(self) -> Int { 1 } }
impl Monkey { pub func fighter(self) -> Int { 2 } }
type Foo = & Bar & Monkey & {}

func main() -> Int { Foo {}.fighter() } // 2
```

An additional declaration such as `pub func fighter(self) -> Int` adds a requirement without
replacing an existing body. Distinct overloads remain available; precedence selects between
implementations with the same parameter signature. Overlapping fields must retain compatible
types, and overlapping implementations must preserve return types, ownership modes, visibility,
and effect/suspension contracts. A later implementation may require fewer effects. When a
method has a declared requirement, that requirement supplies the bound even if an earlier
default happens to use fewer effects. Conflicts are compile-time errors, including conflicts
with a local implementation.

Default bodies keep their defining module's lexical scope for names and helpers. They are
specialized and checked with the resulting type as `self`, so calls to another method on
`self` observe the selected implementation. Contract-typed calls likewise dispatch to the
original concrete value's selected methods in both the VM and native backend. Composition
does not copy private representation fields or private method bodies; inherited code must
type-check against the resulting accessible structure. Associated factories are not defaults.

Structural conformance without an explicit composition clause still checks an existing value's
contract; it does not inject default methods into that value's nominal type.

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
returns `Option.None` after exhaustion. `Iterable<T>` is repeatable: its read-only `iterator()`
method creates an independent iterator. An iterable is opened with `value.iterator()`.

Both contracts use the same static duck typing and zero-conversion dispatch as other composed
types. A concrete type implements them with `type Cursor<T> = & Iterator<T> & { ... }` or
`type Collection<T> = & Iterable<T> & { ... }`. The core adapter
`Iterator.from_sequence(values)` consumes a `Sequence<T>` into an independent iterator, so lists,
strings, and user-defined sequence implementations can participate immediately.

The standard collection hierarchy is behavioral rather than representational:

```text
Iterable<T>
└── Collection<T>
    ├── Sequence<T> → List<T>, String as Sequence<String>, Range<T>
    ├── Set<T>
    ├── Queue<T> → ListQueue<T>
    ├── Deque<T> → ListDeque<T>
    └── Stack<T> → ListStack<T>

Map<K, V> & Collection<Entry<K, V>>
```

`List`, `String`, and `Sequence` expose `.iterator()` as a compiler-backed intrinsic whose public
contract is an ordinary borrowed accessor. The VM creates an independent cursor over the source's
read-only value view. Advancing the cursor mutates only cursor state, while explicit
`Iterator.from_sequence` remains the ownership-transferring form.

## Binary values

Foster separates one bounded octet, immutable binary data, and mutable construction storage:

```foster
let byte = Byte.from(255)
let data = Bytes.from_hex("89504e47")

let buffer = ByteBuffer.with_capacity(4096)
buffer.extend("Foster".bytes)
let snapshot = buffer.snapshot()
let finished = (move buffer).freeze()
```

`Byte` is a copy type in the inclusive range `0..255`. It widens to `Int` in expected-type contexts,
and ordinary arithmetic also produces `Int`; bitwise and shift operators retain `Byte`. `Bytes` is
an opaque Foster type over immutable
contiguous raw storage, implementing
the read-only `Sequence<Byte>` and `Collection<Byte>` behavior. `ByteBuffer` is mutable list-backed
storage, but deliberately has no implicit position or limit; stateful reading can be introduced
separately as a cursor contract.

Passing all three types borrows by default. A buffer mutation requires `mut` access, indexed loans
are invalidated by structural changes, and converting a buffer without copying requires an
explicit move through `(move buffer).freeze()`. `buffer.snapshot()` is the copying alternative.
Strings never convert to bytes implicitly: `.bytes` exposes immutable UTF-8 bytes, and
`String.from_utf8(move bytes)` validates and transfers byte storage into a String.

## Resource identity and capabilities

Resource identity, provider association, and authority are separate structural contracts.
`ResourceIdentifier` requires the deliberately uncommon `resource_id()` method, avoiding accidental
conformance by every displayable value. `Resource<L>` requires only `location: L` and retains the
concrete identifier kind instead of erasing it:

```foster
pub type ResourceIdentifier = {
    pub func resource_id(self) -> String [read self]
}

pub type Resource<L> = {
    pub location: L
}
```

I/O is expressed independently by `Readable<E>`, `Writable<E>`, `PositionedReadable<E>`,
`Appendable<E>`, `Sized<E>`, `Closable<E>`, and `Accepting<C, E>`. `ReadWrite<E>` combines the first
two. A parameter that needs only reading therefore accepts any value matching `Readable<E>` and
does not demand a location field. Conversely, a function can request `Resource<Path>` without
granting or assuming I/O authority.

`std.path.Path`, `std.uri.Uri`, and `std.net.tcp.TcpEndpoint` implement
`ResourceIdentifier`. `std.fs.File` is an opaque `Resource<Path>` with file capabilities;
`tcp::Connection` and `tcp::Listener` are opaque `Resource<TcpEndpoint>` providers with their own
stream, accepting, and closing capabilities. Construction remains side-effect free. A general URI
is only an identifier: an explicit protocol provider must validate and convert it before opening a
file, connection, or listener.

## Stream contracts

Stateful, positioned stream behavior remains expressed with generic structural contracts:

```foster
pub type Reader<E> = {
    pub func read(self, maximum: Int) -> Result<Bytes, E> [mut self]
}

pub type Writer<E> = {
    pub func write(self, contents: Bytes) -> Result<Int, E> [mut self]
    pub func flush(self) -> Result<(), E> [mut self]
}
```

`TextReader<E>` and `TextWriter<E>` provide the corresponding text operations. The error parameter
allows a file to expose `IoError` while a socket exposes `NetworkError`; no universal I/O error is
required. Empty bytes or text signal clean EOF. A binary write may be partial and therefore returns
the number accepted. Successful non-empty reads and writes must make progress.

Mutable parameters are borrowed places in VM call frames. Consequently, calling a generic helper
such as `stream::copy(reader, writer)` mutates the original stateful values rather than temporary
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

Every type declaration describes a contract. Composition imports inherited requirements without
requiring them to be repeated, and an empty record contributes no additional requirements. Thus
`type C = & A & B` and `type C = & A & B & {}` have the same effective contract. Adding fields to
the record body extends that contract. Implementations are checked when a named record is
instantiated and when values are structurally adapted, rather than when the contract is declared.

Implementations must preserve the usual laws: equality is reflexive, symmetric, and transitive;
`compare` returns `Ordering.Equal` exactly when `equal?` is true; and equal values produce the same
hash. Hash collisions between unequal values are valid. The compiler checks member signatures and
effects, while these semantic laws remain the implementation's responsibility. All three are
methods: zero-argument `hash` uses `value.hash()`, while `equal?` and `compare` take arguments.

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
) -> () [mut people] {
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
func enqueue(job: Job) -> () [consume job] { /* ... */ }

enqueue(move pending_job)
```

Explicit user-defined copying is available through `core.copy.Copy`, whose method is
`func copy(self) -> self`. The return type `self` means the concrete receiver type. The separate
`core.drop.Drop` contract declares `func deinit(self) -> ()`, called automatically at ownership
end before child values are released. Copies create independent owners; moves transfer ownership
and its cleanup obligation. Neither protocol changes implicit scalar copy classification.

Copy values and fresh temporaries do not require `move`. Named `ref[group] T` types are reserved
for borrows that participate in first-class references, escaping relationships, captures, or group
effects rather than routine parameter passing.

Function types carry the same contract by parameter position:

```foster
func(Job) -> ()         // borrows its argument
func(consume Job) -> () // takes ownership of its argument
```

`core.functions` provides names for the three common one-operation callable shapes:

```foster
import core.functions

Predicate<Job> // func(Job) -> Bool
Consumer<Job>  // func(consume Job) -> ()
Supplier<Job>  // func() -> Job
```

These are transparent aliases, so they preserve callable parameter ownership and runtime
representation rather than introducing wrapper values. They currently describe pure callbacks;
effect-polymorphic aliases remain future work.

## Inferred and explicit effects

Ordinary functions infer `read`, `mut`, `reshape`, `consume`, and `suspend` from their bodies and
the functions they call. Inference runs to a fixed point for recursive functions. The inferred row
is stored in typed HIR and callable bytecode information, so later ownership checks, remote calls,
and VM lowering retain the contract without source-level suffixes:

```foster
impl Inventory {
    func restock(self: Inventory, amount: Int) -> Int {
        self.count = self.count + amount
        self.count
    }
}
```

When a contract must be written because there is no concrete body, it follows the return type in a
bracketed, comma-separated clause:

```foster
func transform(x: List<Int>, y: Job, z: Inventory) -> Int
    [reshape x, consume y, reshape z.items, suspend]
```

Explicit function contracts use the same form and act as checked upper bounds. Loose tokens after
the return type are not valid syntax.

Declaration names are normalized to these positional modes before a callable is stored or erased,
so indirect calls and partial applications do not lose ownership information.

In `callable(fixed, _)`, the callable and `fixed` are evaluated and captured when the partial is
created. Supplied operands are evaluated once from left to right; only placeholder operands are
provided by each later invocation. Their copy, move, or borrow therefore begins at creation rather
than at the first call.

Foster has no surface keyword for callable erasure. `func(...) -> ...` always describes the
required callable contract. The compiler decides whether a particular value remains a direct
function, is specialized, or needs a representation-erased closure environment.

The compiler implements moves, copy/move/reference closure captures, borrowed-result escape checks,
projected-reference invalidation, group-effect derivation, and ownership-safe remote transfer. See
[Ownership and borrowing](ownership.md) for the source model, compiler passes, runtime backstops,
and implementation limits.

## Errors as values

Recoverable errors are ordinary typed values, conventionally represented with the
Foster-written `Result<T, E>` enum:

```foster
import core.result

func parse(input: String) -> Result<Json, JsonError> {
    branch parse_value(input) {
        Result.Ok(value) -> Result.Ok(value)
        Result.Error(error) -> Result.Error(error)
    }
}
```

The prefix `try` expression propagates an error while yielding a successful value:

```foster
func load(input: String) -> Result<Json, JsonError> {
    let value = try parse_value(input)
    Result.Ok(value)
}
```

`try operation()` requires `operation()` to have type `Result<T, E>` and the enclosing function
to return `Result<U, E>`. The success types `T` and `U` may differ, but the error type `E` must be
the same; `try` does not perform error conversion. The operation is evaluated exactly once. An
`Ok(value)` produces `value`, while an `Error(error)` immediately returns `Result.Error(error)`
from the enclosing function. Consequently, `try` is only valid inside a Result-returning function.

The VM host boundary follows the same rule for `std.fs`, `std.path`, `std.env`, `std.net.tcp`, the
wall and monotonic clocks in `std.time`, and operating-system entropy in `std.random`.
The language does not provide dedicated `throw` or typed error-effect syntax.
`try` is control-flow sugar over ordinary `Result` values, not an exception mechanism.

## Module initialization

Module bodies contain declarations and compile-time constants, not arbitrary runtime startup code.
Resources are created by explicit functions. This avoids observable import order and runtime module
initialization cycles.

Because modules contain no runtime initialization, declarations in different modules may refer to
one another when name and signature resolution can settle the cycle.

## Filesystem, network, clock, and entropy access

`std.fs` exposes `File` resources along with compatible string-based UTF-8, binary, and directory
operations. `std.path` provides typed `Path` values and platform path operations; `std.uri` provides
parsed URI identities without performing network I/O; and `std.env` provides process environment
queries. Fallible filesystem operations return `Result<..., IoError>`, using the shared error type
from `std.io`.
`std.net.tcp` exposes opaque listeners and connections with typed `NetworkError` results. Their
public records and wrappers are Foster code; private VM intrinsics perform the host operations.
`std.time` keeps exact durations, instants, civil values, fixed-zone resolution, and ISO/RFC text
in Foster. Private VM intrinsics provide only canonical wall-clock and host-context-relative
monotonic readings.
`std.random` defines structural source contracts, deterministic generators, unbiased range
reduction, probability distributions, sequence operations, and secure helpers in Foster. Its only
private host intrinsic obtains bytes from the operating system's secure entropy source through a
dependency-free platform shim.

These modules use the current machine's isolated host context.

## Compiler pipeline

The implemented pipeline is:

```text
source -> tokens -> AST -> resolved HIR -> type/effect inference
       -> loan/group/capture checks -> ownership MIR validation
       -> temporary register construction -> layout legalization -> shared typed SSA
            -> de-SSA bytecode -> optional optimizer -> drops -> verifier -> VM
            -> supported-subset validation -> Cranelift AOT -> host executable
```

The register VM is the complete executable semantic reference. The native backend compiles the
reachable scalar, record, and tagged-variant subset described in [Native compilation](native.md).
Temporary
register construction is sealed into shared SSA before any executable bytecode is optimized,
serialized, or run.

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
