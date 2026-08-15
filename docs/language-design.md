# Foster Language Design

This document records Foster's current design. It is a living specification: **settled** items are
intentional foundations, **provisional** items are implemented or strongly preferred but may still
change, and **open** items require design work before they become language guarantees.

## Goals

Foster is a statically typed, general-purpose language intended to span application and systems
programming. Its defining memory-safety direction is single ownership with group-parameterized
references: references describe the set of locations they may target, while mutation is expressed
as a function effect.

The language should be approachable, predictable from source, explicit at API boundaries, and
capable of zero-cost native execution without requiring garbage collection.

## Source files and modules — settled

Filesystem structure determines module structure.

- A directory implicitly defines an empty module.
- A `.foster` file defines the body of its corresponding module.
- A same-named file and directory describe one module: the file contains its declarations and the
  directory contains its children.
- Module components must be portable identifiers and may not differ only by case.
- The package source root is implicit and is not itself a named module.

```text
json.foster          json, with declarations
json/
  parser.foster      json.parser
tools/               tools, implicit and empty
  text/              tools.text, implicit and empty
    trim.foster      tools.text.trim
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
component as a module qualifier. Thus `import core.option` permits both `Option[T]` and
`option.Option[T]`. Same-module declarations take precedence. If multiple imported modules expose
the same unqualified name, Foster requires a module-qualified use at that point; importing the
modules themselves remains valid.

Modules and declarations occupy one logical name system. Modules established by a `.foster` file,
a directory, or both are implicitly public and may always be addressed by canonical path. Every
declaration inside a module is private unless explicitly marked `pub`:

```foster
func helper() { }       // visible only within this module
pub func parse() { }    // visible through the module's canonical path
```

This default applies to the implemented function and type declarations and is intended to apply to
future declaration kinds as well. A public declaration may not expose a private declaration in its
public signature. Explicit re-export syntax remains an open design question.

## Functions and evaluation — provisional

`func` introduces a function. Parameters and public signatures are intended to be typed, while
local values use inference. The last expression in a function is its result.

```foster
func double(value: Int) -> Int {
    value * 2
}
```

Explicit `return` performs an early return. A postfix guard is supported:

```foster
func first(values: List[String]) -> String {
    return "" if values.empty?
    values.head
}
```

Identifiers may end in `?`, conventionally marking Boolean observations such as `empty?` and
`whitespace?`. Commas separate arguments and generic parameters. Newlines separate statements.

## Values — provisional

The executable prototype currently has:

- `Bool`
- `Int`
- `Float` (IEEE-754 binary64)
- `String`
- `CodePoint`, with literals such as `'F'`, `'λ'`, and `'\n'`
- symbols such as `:json_error`
- homogeneous lists, enforced by the type checker
- `Sequence[T]`, implemented without conversion by `List[T]` and by `String` as
  `Sequence[CodePoint]`
- unit

Foster will not have a universally nullable reference. Absence is represented by `Option[T]`:

```foster
type Option[T] =
    | Some(T)
    | None
```

`Sequence[T]` is a read-oriented structural view, not a storage representation. Passing a list or
string to a sequence parameter retains the original runtime value and ownership. Its common
members are `empty?`, `length`, `head`, and `rest`. For strings, `head` returns a `CodePoint` and
`rest` remains a `String` when accessed directly; through a `Sequence[CodePoint]` parameter,
`rest` has sequence type. A code point exposes `.value`, `.string`, and `.whitespace?`.
The bootstrap compiler currently supplies the `String` and `List[T]` conformances. Syntax for
user-defined sequence implementations belongs to the general protocol design and is not yet
available.

## Records — implemented foundation

Records have nominal constructors and may have generic type parameters. Their accessible fields
also form structural contracts, as described below. Types and fields are private by default;
filesystem modules remain implicitly public:

```foster
pub type Person {
    pub name: String
    pub age: Int
    internal_id: Int
}

person = Person { name age internal_id }
```

Construction initializes every field exactly once. A record with any private field can only be
constructed inside its defining module. Field mutation is controlled by ownership and group access,
not by a `var` marker on the field. Generic records such as `Parsed[T]` participate in ordinary
constraint inference. Functional `..record` updates and record patterns are deferred.

Functions may be associated with a record's type namespace by qualifying their declarations. They
do not receive an instance and are called through the type:

```foster
pub type Map[K, V] {
    entries: List[Entry[K, V]]
}

pub func Map.empty[K, V]() -> Map[K, V] {
    Map { entries: [] }
}

scores = Map.empty()
```

Associated functions are declared in the record's defining module, so they may construct records
whose representation contains private fields. The qualifier must name a record in that module.
An associated declaration cannot have a `self` parameter; instance methods retain the existing
`func get(self: Map[K, V], key: K)` form. Both directly imported `Map.empty()` and explicitly
module-qualified `map.Map.empty()` calls resolve to the same function.

## Closed variants — implemented foundation

Closed variants use alternatives and are consumed with exhaustive pattern branching:

```foster
type Result[T, E] =
    | Ok(T)
    | Error(E)
```

Alternatives may have zero or more positional payload values. Constructors are qualified by their
type, such as `Result.Ok(42)` and `Option.None`. Generic arguments are inferred from constructors,
function calls, and branch patterns.

An alternative may be written without its type qualifier when its name uniquely identifies an
alternative in the current module. This applies to both constructors and patterns, allowing
`Ok(value)` and `Error(error)` in code centered on one result type. If two variant types declare the
same alternative name, Foster requires the qualified spelling.

## Branch expressions — implemented

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
top-level binding or `_` is a catch-all. Record and list patterns, guards, and refined exhaustiveness
for non-variant literal domains are deferred.

## Remote objects and virtual threads — implemented

`remote` transfers a record into an isolated virtual thread. A function declared in the record's
module whose first parameter is named `self` is an instance method. Calling that method through a
`Remote[T]` handle sends a FIFO mailbox message and returns `Future[R]`; `await` parks the current
virtual thread until the reply arrives.

```foster
func increment(self: Counter, amount: Int) -> Int {
    self.value = self.value + amount
    self.value
}

counter = remote Counter { value: 0 }
updated = await counter.increment(1)
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
catalog = Catalog { entries: [] }
reader = remote ref catalog
catalog.add("Foster")
found = await reader.contains("Foster")
```

The resulting type retains the borrowed group as `Remote[ref[group] Catalog]`. Read-only describes
the handle's capability, not permanent immutability of the underlying value.

Borrow-mode remote method arguments use the same mechanism for a shorter lifetime. Because object
parameters borrow by default, `worker.inspect(document)` sends a live read-only capability;
`worker.submit(move document)` transfers ownership only when the parameter consumes it. The
temporary loan begins when the worker starts the invocation and ends when that invocation returns,
independently of when its future is awaited. Borrowed arguments cannot be mutated, consumed, stored
in actor state, or returned across the mailbox boundary.

## Static types — implemented foundation and provisional design

Foster uses local type inference with explicit package API signatures. Implemented foundations are
nominally constructed records with structural adaptation, closed variants, explicit parametric
generics using `Type[Argument]`, function and intersection types, and no implicit numeric or
nullable conversions. Traits, transparent aliases, distinct wrapper declarations, and typed error
effects remain design work.

Types, traits, and functions may be qualified by modules. The HIR resolves every source-level name
to a local binding, function, module, builtin, or later a type-level definition before type checking.

The bootstrap compiler resolves `Unit`, `Bool`, `Int`, `Float`, `String`, `CodePoint`, `Symbol`,
`List[T]`, `Sequence[T]`, `Remote[T]`, `Future[T]`, concrete and erased function types, records,
variants, generics, and record intersections. Decimal and scientific-notation literals produce
`Float`; there are no implicit conversions between `Int` and `Float`.
Representation-level operations such as functional `List.append`, `code_point(CodePoint)`,
`from_code_point(Int)`, and `parse_float(String)` form the narrow primitive boundary beneath the
Foster-written core library.
It performs constraint inference across function calls and records a canonical type for every HIR
expression, local, and function signature. Explicit generic functions use
`func identity[T](value: T) -> T`; their parameters are rigid while checking the body and freshly
instantiated at each call. It checks operators, calls, branch results,
returns, list construction, and the implemented standard members. An unconstrained type is an error
and asks for an annotation.

This is not Hindley–Milner generalization: an unannotated function receives one inferred type within
a compilation rather than becoming implicitly polymorphic. Polymorphism is always explicit.
Type and group parameters share a bracketed function-parameter section but occupy distinct
namespaces syntactically: `T` declares a type parameter, while `items: group T` declares a group.
A function may not declare either category twice or reuse one name across both categories.

## Core library — explicit imports

Foster has no prelude. The compiler embeds Foster-written `core.option`, `core.result`,
`core.ordering`, `core.sequence`, `core.list`, `core.map`, `core.character`, `core.string`,
`core.bool`, `core.int`,
`core.float`, `core.io`, and `core.net.tcp` modules so tools can resolve them consistently, but no
declaration is injected into user scope. Programs import every module they use. The supported
surface and runtime boundary are documented in `docs/core-library.md`.

## Comments and documentation

`//` starts an ordinary line comment. `/* ... */` is a block comment and may be nested. Ordinary
comments do not enter the AST and have no effect on compilation.

`///` and `/** ... */` are documentation comments. Consecutive documentation comments are joined
with newlines and attach to the function, record, or variant type that immediately follows them:

```foster
/// A TCP connection owned by the runtime.
///
/// Obtain one with `connect` or `accept`.
pub type Connection {
    handle: Int
}
```

Documentation text is Markdown. The compiler retains it in AST and HIR, and the language server
includes it in hover information and completion items. A documentation comment that does not
precede a declaration is an error.

## Structural record adaptation and intersections

Records have nominal constructors but public fields form a statically checked structural contract.
When a record value is used where another record type is expected, Foster accepts it when it has
every accessible field required by the destination type with the same field type. Additional fields
remain on the value but are hidden by the destination's static view:

```foster
type Named {
    pub name: String
}

type User {
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

`A & B` is an intersection contract requiring the accessible fields of both record types:

```foster
func locate(value: Named & Located) -> String {
    value.name + value.location
}
```

Intersection members must be record types. Overlapping fields must have identical types. Structural
adaptation does not expose an inaccessible private field, so records with private representation
remain nominal outside their defining module. Methods also remain nominal: `&` composes field
contracts, not implementations or method sets.

## Ownership and groups — implemented foundation

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

The compiler must distinguish at least:

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
func(Job) -> Unit             // borrows its argument
any func(consume Job) -> Unit // takes ownership of its argument
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
any func(Inventory) -> Int [mut inventory, suspend]
```

Explicit function contracts use the same form and act as checked upper bounds. Loose tokens after
the return type are not valid syntax.

Declaration names are normalized to these positional modes before a callable is stored or erased,
so indirect calls and partial applications do not lose ownership information.

The current compiler implements moves, copy/move/reference closure captures, borrowed-result escape
checks, projected-reference invalidation, group-effect derivation, and ownership-safe remote
transfer. Richer place provenance and control-flow-aware loan analysis remain open work. See
[Ownership and borrowing](ownership.md) for the source model, compiler passes, runtime backstops,
and current limitations.

## Errors — implemented values and provisional effects

Recoverable errors are currently ordinary typed values, conventionally represented with the
Foster-written `Result[T, E]` closed variant:

```foster
import core.result

func parse(input: String) -> Result[Json, JsonError] {
    branch parse_value(input) {
        Result.Ok(value) -> Result.Ok(value)
        Result.Error(error) -> Result.Error(error)
    }
}
```

The VM host boundary follows the same rule for `core.io` and `core.net.tcp`. Dedicated `throw` and
typed error-effect syntax are not implemented; propagation syntax and its relationship to closed
variants remain open.

## Module initialization — settled direction

Module bodies contain declarations and compile-time constants, not arbitrary runtime startup code.
Resources are created by explicit functions. This avoids observable import order and runtime module
initialization cycles.

Because modules contain no runtime initialization, declarations in different modules may refer to
one another when name and signature resolution can settle the cycle. A future compile-time constant
system must preserve this initialization-free model.

## Filesystem and network access — implemented host boundary, provisional capabilities

`core.io` exposes typed UTF-8 file, directory, and path operations returning `Result[..., IoError]`.
`core.net.tcp` exposes opaque listeners and connections with typed `NetworkError` results. Their
public records and wrappers are Foster code; private VM intrinsics perform the host operations.

These modules currently use process-wide host capabilities. Explicit capability values for
production, sandboxed, and in-memory implementations remain a design goal, as do byte buffers,
socket readiness, and TLS.

## Compiler pipeline — settled

The implemented pipeline is:

```text
source -> tokens -> AST -> resolved HIR -> type/effect inference
       -> loan/group/capture checks -> ownership MIR validation
       -> structured register bytecode -> optional optimizer -> verifier -> VM
```

The register VM is the sole execution engine and the executable semantic reference. Bytecode is
currently lowered from checked HIR after ownership MIR validation. A backend-neutral lowered IR and
Cranelift JIT/object backend are intended future layers; LLVM remains optional.

Group information is normally erased before bytecode execution, but its consequences—moves,
storage identity, and valid optimization facts—are represented by checked HIR, ownership MIR, and
concrete VM operations.

## Deferred features

The initial design deliberately defers inheritance, higher-kinded types, arbitrary type-level
programming, macros, operator overloading, reflection, and a stable ABI. Foster's complexity budget
is currently reserved for a coherent ownership, group, and effect model.

## Focused design documents

- [Ownership and borrowing](ownership.md)
- [Closures and group borrowing](closures.md)
- [Effect derivation](effect-derivation.md)
- [Virtual machine](vm.md)
