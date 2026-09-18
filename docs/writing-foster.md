# Writing Foster: a guide for coding agents

Use this guide when generating or editing `.fos` source. It teaches the common
forms; [language design](language-design.md) is the full syntax reference and
[semantics](semantics.md) defines behavior. Every `foster` code block below is a
complete program that returns `42`, checked by
[agent documentation tests](../tests/agent_documentation.rs).

## Constrained implementation parameters

Requirements on implementation parameters are structural. Here `T` must provide the
method required by `Copy`; the copied result still has the original concrete type `T`.
Import the requirement's module, and join multiple requirements with `&`.

```foster
import core.copy

type Box<T> = { value: T }

impl Box<T & Copy> {
    func copied(self) -> T [read self] { self.value.copy() }
}

func main() -> Int { Box { value: 42 }.copied() }
```

## Record destructuring

Select fields by name, use `field: local` to rename a binding, and omit fields you
do not need. Nested record patterns work in both `let` bindings and enum branches.
The source expression is evaluated once. A `let` destructuring binding follows
ordinary field-binding ownership: scalar fields copy and managed fields move;
omitted fields remain available. It does not call `copy()` implicitly.

```foster
type Parsed = { value: Int, input: Int, ignored: Int }
enum Outcome = Match(Parsed) | Failed

func main() -> Int {
    let parsed = Parsed { value: 20, input: 22, ignored: 0 }
    let { value, input: rest } = parsed
    let result = Outcome.Match(Parsed { value, input: rest, ignored: 1 })
    branch result {
        Outcome.Match({ value: answer, input }) -> answer + input
        Outcome.Failed -> 0
    }
}
```

Binding patterns accept field bindings, `_`, and nested records. Literal and enum
case tests belong in `branch` patterns. Field privacy still applies; destructuring
selects stored fields, not methods or computed properties. Function parameters
continue to use ordinary named parameters.

## Find library declarations

Read the [library guide](../library/README.md) to choose a module. Search its `.fos`
source for the exact type, method, parameter order, and ownership contract. Names
borrowed from other languages are not evidence that an API exists in Foster.
The compiler embeds the library: use `cargo run --bin foster -- ...` from this
checkout when checking changes to library source or the language itself.

Source lives in `.fos` files. A project manifest can select its source directory
with `[package]`, `name`, and `source` (normally `src`). Filesystem paths define
modules. `import std.net.tcp` imports that module; `tcp::connect(...)` calls a
module function. Importing a module also exposes its public names directly.
Import the modules whose APIs you use; there is no general library prelude.

## Variables, records, methods, and control flow

`let` introduces a mutable binding. Assign with `=` afterward; do not redeclare
the same binding to update it. Local types are inferred from expressions.
Newlines separate statements. A block's final statement determines its result;
use a final `()` when a function should return unit after an assignment.

Records use `type Name = { ... }`, construction uses `Name { field: value }`,
and implementations use `impl Name { ... }`. Use `pub` on declarations and fields
that other modules must access. Prefer inferred `self` in implementation methods;
put shared generic parameters on the `impl` header. Keep explicit receiver types
for references and specialized receivers such as `List<String>`. Structural
contracts match accessible members; there is no nominal `implements` requirement.

Use field shorthand when a field and its source variable have the same name:
`Frame { rule, position }` means `Frame { rule: rule, position: position }`.
Keep explicit initializers for expressions and ownership markers such as `move`.

```foster
type Counter = { value: Int }

impl Counter {
    func increment(self) -> () [mut self] {
        self.value = self.value + 1
        ()
    }
}

func main() -> Int {
    let counter = Counter { value: 0 }
    let total = 0
    for item in [10, 20, 30] {
        continue if item == 20
        total = total + item
    }
    while counter.value < 2 {
        counter.increment()
    }
    branch {
        total == 40 -> total + counter.value
        _ -> 0
    }
}
```

`branch` chooses a value or statement block. Its conditions are checked in order;
`_` is the fallback. Result-producing arms must have compatible types. A `Bool`
subject match also needs `_` for exhaustiveness in the current compiler.

`while condition { ... }` checks the condition before each iteration.
`loop { ... }` repeats until control leaves it. `for item in collection { ... }`
opens `.iterator()` once, then calls `.next()` until `Option.None`. The yielded
item is unwrapped for the body. A bare iterator without `.iterator()` is not an
iterable. `break` and `continue` target the nearest enclosing loop; completed
loops produce `()`. Loop-body locals do not escape the loop.

Use `return value if condition`, `break if condition`, or `continue if condition`
for guarded transfers. Do not write `if condition { ... }`, `else`, or a postfix
guard on an assignment. In loop headers, parenthesize a direct record literal
to distinguish its braces from the body.

## Optional results, errors, and generic functions

Enums use `enum Name = Case | Other(Payload)`. A case carries at most one payload
type; use a record to package several fields. Match cases with `branch value`.
Use `Option.Some(value)` / `Option.None` for absence and `Result.Ok(value)` /
`Result.Error(error)` for recoverable failure. `null`, exceptions, and Rust-style
`?` propagation are not replacements for these forms.

```foster
import core.result

enum InputError = Negative

func identity<T>(value: T) -> T [consume value] {
    move value
}

func nonnegative(value: Int) -> Result<Int, InputError> {
    branch {
        value < 0 -> Result.Error(InputError.Negative)
        _ -> Result.Ok(value)
    }
}

func doubled(value: Int) -> Result<Int, InputError> {
    let checked = try nonnegative(value)
    Result.Ok(checked * 2)
}

func main() -> Int {
    branch doubled(identity(21)) {
        Result.Ok(value) -> value
        Result.Error(_) -> 0
    }
}
```

`try` unwraps success or returns the error from the enclosing function. The
enclosing function must return a compatible `Result` with the same error type.
Prefer it for direct error propagation. Keep `branch` for error conversion or
recovery, and for borrowed results: `try` consumes its operand.
`assert(condition, "message")` stops execution on failure; it does not produce a
recoverable error. Generics use `<T>`; group declarations use brackets instead.

## Propagating custom outcomes

Use `try<Case>` to unwrap a selected enum case and return any other case from the
function. The return enum must accept every other case by name with the same
payload type. A case without a payload yields `()`. The operand is consumed;
use `move` for an existing owned binding. Keep branches for recovery or adding
error context.

```foster
enum Outcome<T> = Match(T) | Failed(Int)

func doubled(input: Outcome<Int>) -> Outcome<Int> [consume input] {
    let value = try<Match> move input
    Outcome.Match(value * 2)
}

func main() -> Int {
    branch doubled(Outcome.Match(21)) {
        Outcome.Match(value) -> value
        Outcome.Failed(_) -> 0
    }
}
```

## Ownership, references, and closures

Ordinary call arguments borrow by default. `[consume name]` declares that a
function takes ownership of a parameter. Pass an existing owned place with
`move`; it cannot be used again unless reinitialized. Copy scalars such as `Int`
and fresh temporaries do not need that marker. Explicit `.copy()` preserves the
source when its type supports copying; do not assume every type supports it.

Effects describe permitted access: `read` observes, `mut` changes values, and
`reshape` permits structural changes that may invalidate references into storage.
Read the [ownership guide](ownership.md) before storing or returning references,
mutating a collection with live element references, or defining group contracts.
An ordinary mutable method call does not require manually constructing a reference.

```foster
import core.string

type Counter = { value: Int }

func bump[g: group Counter](counter: ref[g] Counter) -> () [mut g] {
    counter.value = counter.value + 1
    ()
}

func consume_length(text: String) -> Int [consume text] {
    text.length
}

func main() -> Int {
    let text = "hello"
    let saved = text.copy()
    let size = consume_length(move text)
    assert(saved.length == 5)
    let counter = Counter { value: 0 }
    bump(ref counter)
    let factor = 7
    let scale = [copy factor] (value: Int) -> value * factor
    scale(size + counter.value)
}
```

`ref[g] T` describes a reference associated with a named group; `ref value`
borrows a place. This is not Rust's `&mut T` or lifetime syntax. Closure syntax is
`(argument: Type) -> expression` or `-> { statements }`; explicit captures include
`[copy name]`, `[move name]`, and `[ref name]`. Captures and call arguments have
different ownership rules: read [closures](closures.md) before returning a closure
that captures local state. Remote objects and suspension have additional rules in
[remote semantics](remote-semantics.md); do not assume JavaScript async semantics.

## Type and capability branches

Use `is Copy` to test copyability in generic code. The test only reads the value;
the successful arm can invoke `.copy()` and the fallback must handle failure.

```foster
import core.copy
import core.result

enum CopyError = NotCopyable

func attempt<T>(value: T) -> Result<T, CopyError> [read value] {
    branch value {
        is Copy -> Result.Ok(value.copy())
        _ -> Result.Error(CopyError.NotCopyable)
    }
}

func main() -> Int {
    branch attempt(42) {
        Result.Ok(value) -> value
        Result.Error(_) -> 0
    }
}
```

`is Type` checks whether the value satisfies that type, including user-defined
structural contracts. The matching arm keeps its original type and gains access
to the established contract. `Copy` uses the same rule; it is not a separate
pattern operation. A `_` fallback is required. Parameterized test targets are not
yet supported.
See [type patterns](language-design.md#type-patterns) for the bounds.

## Common translation mistakes

| Tempting assumption | Foster rule |
| --- | --- |
| `let` means immutable | Ordinary locals and stored fields are mutable, subject to ownership and effects |
| `if/else` or a ternary chooses a value | Use `branch` |
| `module.function()` calls a module function | Use `module::function()`; `.` is for types and values |
| A zero-argument method is a property | Call `.method()`; stored/computed members use `.member` |
| A public record exposes every field | Field visibility is independent |
| Strings count bytes or scalars with `.length` | `String.length` counts grapheme clusters; see [Unicode](unicode.md) |
| Any element can be copied out of a list | `List.at` can fail with `NotCopyable`; inspect the operation's contract |
| All collection methods preserve the collection | Some consume it; inspect source effects before reusing it |
| Familiar numeric edge cases are handled automatically | Read bounds, overflow, and division preconditions in the method comments |
| Generated signatures are ready-to-paste source | They include resolved reference notation; copy source declarations instead |

## Check the program before delivering it

From the repository root:

```powershell
cargo run --bin foster -- check path/to/program.fos
cargo run --bin foster -- run path/to/program.fos
cargo run --bin foster -- fmt path/to/program.fos --check
cargo test --test agent_documentation
```

Use `foster test <package>` for `test "description" { ... }` declarations. Keep
core library files in their package context. Use targeted Rust tests when changing
the compiler; check both VM and native behavior when a change affects execution.
Do not change the language just to make an unfamiliar generated program compile.
Resolve errors by checking declarations and the relevant specification first.

Write public API comments in `.fos` source, including ownership, units, bounds,
and failure behavior where applicable. Follow the [documentation standard](../library/DOCUMENTATION.md).
Check the [roadmap](roadmap.md) only to identify proposals, not available features.

## Non-returning paths

Use `panic(message)` for an unrecoverable failure. It has type `Never`, reports a String
message, and performs ordinary failure cleanup. Neither `try` nor `try<Case>` catches it.
A `-> Never` function cannot return a value; a branch arm that panics or transfers control
needs no dummy result.

```foster
func fail(message: String) -> Never { panic(message) }

func positive(value: Int) -> Int {
    branch {
        value > 0 -> value
        _ -> fail("expected a positive integer")
    }
}

func main() -> Int { positive(42) }
```
