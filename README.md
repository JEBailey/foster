# Foster

Foster is an experimental statically typed, general-purpose programming language with compile-time
duck typing: a type conforms when its accessible contract matches, without nominal inheritance or
runtime member lookup. Its defining direction is single ownership with group-parameterized
references, inferred effect contracts, structurally adaptable records, and lightweight remote
objects running on virtual threads. The bootstrap compiler and register VM are written in Rust.

## Try it

```powershell
cargo run --bin foster -- run examples/live_inventory_pipeline.fos
cargo run --bin foster -- check examples/json_parser
cargo run --bin foster -- check tests/fixtures/modules
cargo run --bin foster -- fmt examples
cargo run --bin foster -- fmt examples --check
cargo run --bin foster -- test tests/foster
cargo run --bin foster -- test library
cargo run --bin foster -- run tests/fixtures/modules --no-optimize
cargo run --bin foster -- run examples/arguments.fos -- --about
cargo run --bin foster -- build benchmarks/fibonacci.fos --native -o fibonacci.exe
cargo run --bin foster -- build benchmarks/fibonacci.fos --native --emit native-ir
cargo run --bin foster -- pack examples/json_parser -o json-parser.fpk
cargo run --bin foster -- run json-parser.fpk
cargo run --bin foster -- docs library
cargo run --bin foster -- docs library --serve
cargo test
```

## Projects and `foster.toml`

Create a conventional Foster project with:

```powershell
foster init hello-foster
cd hello-foster
foster run
```

`foster init` creates this layout without overwriting an existing manifest or source file:

```text
hello-foster/
  foster.toml
  src/
    main.fos
```

The manifest identifies the directory where filesystem module discovery starts:

```toml
[package]
name = "hello-foster"
source = "src"
```

`package.name` is required. `package.source` defaults to `src` when omitted and must be a relative
path contained by the project. `run`, `check`, `build`, `pack`, `test`, `fmt`, and `docs` accept a
project directory or its `foster.toml` file. When their path is omitted, Foster searches the
current directory and its parents for the nearest manifest; `fmt` and `docs` fall back to their
existing current-directory behavior when no manifest exists. Explicit `.fos` files and legacy
directories whose `main.fos` sits directly at the source root remain supported.

Projects can depend on other Foster projects by relative path:

```toml
[dependencies]
collections = { path = "../foster-collections" }
```

The dependency key is its module namespace. The dependency's `src/main.fos` is mounted as
`collections`, `src/map.fos` as `collections.map`, and `src/tree/set.fos` as
`collections.tree.set`. Imports between modules in that dependency are rebased automatically, so
its own `import map` resolves to `collections.map`. Imports of `core`, `std`, and dependencies
declared by that project retain their declared names. Path dependencies are resolved transitively,
compiled from source with the application, and included by `check`, `run`, `build`, `pack`, `test`,
`docs`, and the language server. Dependency names must be portable module identifiers other than
`core` or `std`; cycles, conflicting transitive names, missing projects, and duplicate mounted
modules are errors.

`run` invokes `main`. It may take no parameters, or one `std.process.Arguments` value containing
the executable name and following command-line values. Pass program arguments after `--`, for
example `foster run app.fos -- input.txt --verbose`. A file is treated as a one-module package; a
directory is discovered as a filesystem module tree whose entry point is `main.fos`. Optimization
is enabled by default and can be selected with `--optimize` or `--no-optimize`.

`foster fmt [file-or-directory]` formats `.fos` source in place. It preserves comments and literal
contents while normalizing indentation, line endings, trailing whitespace, blank lines, and the
final newline. Enum declarations keep their first case after `=` and align later `|` cases on
indented lines. `foster fmt --check` reports files that differ without writing them, making it
suitable for CI. The current directory is used when no path is supplied.

## Generated documentation

`foster docs [file-or-directory]` type-checks the package and generates a static API site in a
`documentation/` directory within the selected package. The site is built from resolved HIR, so signatures include
inferred types and effects. It includes public and private declarations, their visibility, and all
attached Markdown documentation comments. Module pages summarize the public types they provide,
including fields, enum cases, required methods, and linked functions or methods.

Use `--output <directory>` to choose another destination. Add `--serve` to start a local server and
open the site in the system browser:

```powershell
foster docs library --serve
foster docs library --output build/api-docs
foster serve-docs documentation
```

Both serving commands accept `--port <number>` and `--no-open`. The latter is useful on headless
machines. Generated `documentation/` directories are ignored during Foster module discovery.

## Language snapshot

The current implementation includes:

- functions, recursion, explicit `let` local declarations, local inference, explicit generics,
  closures, partial application, immediate-failure assertions, logical negation through `!` or
  `not`, short-circuit `&&` and `||`, and statement loops with guarded `break` and `continue`
  transfers;
- ordinary typed `Result<T, E>` error values and single-evaluation `try` propagation with an exact
  matching error type;
- `Bool`, `Int`, binary64 `Float`, `String`, `CodePoint`, `Symbol`, `()`, homogeneous `List<T>`,
  and zero-conversion `Sequence<T>` views, with lossless `Byte` and `CodePoint` widening to `Int`
  when an assignment, argument, field, branch, or result expects `Int`;
- generic records, associated factories, and arity- and parameter-type-based function/method
  overloads, with exact matches preferred over lossless widening and ambiguous calls rejected;
- instance methods, private-by-default declarations,
  untagged union contracts, and tagged enums with exhaustive pattern branches;
- statically checked structural record adaptation, declaration-side composition such as
  `type Text = & Sequence<CodePoint> & { ... }`, and intersection contracts such as
  `Named & Located`;
- borrow-by-default calls, explicit `move`, positional consuming callable types, group references,
  closure capture modes, move/initialization checking, and structural invalidation;
- inferred or explicit `read`, `mut`, `reshape`, `consume`, and `suspend` effects;
- remote objects, virtual threads, FIFO method messages, futures, `await`, transferred messages,
  call-scoped borrowed messages, and persistent remote read loans;
- explicit core-library imports, structural resource locations and read/write capabilities, typed
  filesystem APIs, parsed URI locations, and typed TCP connections;
- line, nested block, and Markdown module (`//!`) and declaration (`///`, `/** ... */`) documentation comments;
- a package-aware LSP and VS Code extension;
- first-class `test "description" { ... }` declarations with a package-aware test runner;
- an optional optimizing register-bytecode pipeline with a verifier and iterative VM call frames;
  and
- a Cranelift AOT backend for standalone executables, including closed-world specialization of
  reachable generics, nominal aggregates, closures, structural contracts, host services, local
  remote actors, futures, and blocking `await`.

Module qualification uses `::`; type accessors and runtime value access use `.`. Imports keep their
canonical dotted module names:

```foster
import core.result

let outcome = Result.Ok(42)
let mapped = outcome.map((value: Int) -> value + 1)
let name = user.name
user.rename("Ada")
```

Use `::` only to select declarations through a module name, including module-qualified types and
constants. Use `.` for type-associated functions, enum cases, fields, and instance methods. Core
operations with a natural receiver are fluent (`values.map(transform)`, `outcome.map(transform)`);
module qualification remains for namespace operations such as `toml::parse(source)`.

Conditional `branch` expressions use `_` for their required fallback arm:

```foster
func skip_whitespace(characters: String) -> String {
    branch {
        characters.empty? -> characters
        characters.head.whitespace? -> skip_whitespace(characters.rest)
        _ -> characters
    }
}
```

Arms may contain statement blocks. Their final expression is the arm value:

```foster
branch {
    available? -> {
        let value = load()
        normalize(value)
    }
    _ -> fallback
}
```

`if` is reserved for postfix control guards. The implemented form conditionally returns early:

```foster
func clamp_positive(value: Int) -> Int {
    return 0 if value < 0
    value
}
```

Loops use the same guarded-transfer form and do not produce values:

```foster
loop {
    value = next()
    continue if value < 0
    break if value == 0
    consume(value)
}
```

It is not a prefix conditional or a guard for ordinary calls and assignments; use `branch` for
value-producing conditional logic.

Subject branches destructure enum case payloads:

```foster
import core.result

func unwrap_or(result: Result<Int, String>, fallback: Int) -> Int {
    branch result {
        Result.Ok(value) -> value
        Result.Error(_) -> fallback
    }
}
```

Use `try` to unwrap a successful result or return its error from a function with the same error
type. The operation is evaluated once, and the enclosing function's success type may differ:

```foster
func validate() -> Result<Bool, String> {
    let value = try read_value()
    Result.Ok(value > 0)
}
```

## Filesystem modules

Directories implicitly define empty modules. A same-named `.fos` file optionally supplies the
body of that module:

```text
json.fos          json (with declarations)
json/                json (the same module, with children)
  parser.fos      json.parser
tools/               tools (implicit and empty)
  text/
    trim.fos      tools.text.trim
```

There are no `_module` or `index` files. Module components must be portable identifiers and cannot
differ only by case. Imports use canonical dotted names:

```foster
import json
import json.parser as parser
import tools.text.trim
```

Importing a module exposes its public declarations directly and binds its final component as a
qualifier. Modules are public; declarations and record fields are private unless marked `pub`.

## Standard library

Foster has no prelude. Programs explicitly import embedded Foster-written modules such as
`core.functions`, `core.option`, `std.iter`, `core.result`, `std.sequence`, `core.list`,
`std.collections.map`, `std.resource`, `std.fs`, `std.path`, `std.uri`, `std.env`, `std.toml`,
`std.net.tcp`, the `std.time` module family, and the `std.random` module family. Resource capabilities, URI parsing, the TOML 1.1
parser, time arithmetic, fixed-zone resolution, ISO/RFC formatting, validation, table building,
and rendering are Foster code; only general scalar conversion and host-dependent filesystem,
socket, clock, and operating-system entropy operations cross the VM boundary. See
[the standard library reference](docs/core-library.md).

## Compiler and VM

The executable pipeline is:

```text
source
  -> tokens and AST
  -> resolved HIR
  -> type, structural-contract, and fixed-point effect inference
  -> loan, capture, group, and ownership checks
  -> ownership MIR validation
  -> unsealed executable construction and legalized scalar/pointer layouts
  -> shared typed SSA
       -> de-SSA edge copies + register assignment -> bytecode -> optimizer -> verifier -> VM
       -> Cranelift instructions -> object -> host linker -> executable
```

The VM remains the complete executable semantic reference; there is no AST interpreter fallback.
Optimization can be disabled without changing semantics. Programs can be compiled to deterministic
`.fbc` artifacts and run without their source:

Statement blocks couple each statement to its source span. HIR analyses share one recursive
visitor and one reachability-aware arm-flow summary, while ownership MIR and bytecode lowering
consume the same authoritative semantic branch/loop CFG. This keeps branch-test order and control
edges consistent between static checking and execution.

```powershell
cargo run --bin foster -- build examples/showcase/recursion.fos -o recursion.fbc
cargo run --bin foster -- run fibonacci.fbc
```

The initial AOT backend emits host machine code with Cranelift and asks the installed Rust
toolchain to link it into a standalone executable:

```powershell
cargo run --bin foster -- build benchmarks/fibonacci.fos --native -o fibonacci.exe
./fibonacci.exe
```

Native compilation supports reachable scalar functions plus descriptor-backed user records,
tagged enums, concrete closures, uniform higher-order callables, owned erased values, generic
lists, immutable bytes, Foster-written byte buffers, local references, local remote actors, and
futures. This includes nested fields, copy-on-write mutation, aggregate payload patterns,
reference captures and parameters, direct and indirect calls, descriptor-dispatched structural
contracts, generic specialization, methods, String/Symbol literal patterns, `print`/`println`,
aggregate result formatting, friendly runtime failures, recursion, arithmetic, comparisons, and
control flow. Supported core list, string, byte, and byte-buffer algorithms compile as ordinary
Foster code. A versioned platform ABI provides filesystem and path operations, the working
directory, clocks, operating-system entropy, handle-based TCP listeners and connections, and
in-process actor workers. Native code can spawn owned or borrowed actors, queue FIFO method calls,
and consume their futures with a blocking `await`. Resumable suspension/state-machine lowering is
not yet implemented. See [native compilation](docs/native.md) for the exact boundary.

The de-SSA VM emitter owns critical-edge splitting, cycle-safe parallel copies, and deterministic
register assignment. Aggregate legalization separately freezes typed record slots, enum tags and
payload shapes, closure capture ownership, reference place-handle layouts, and runtime structural
values. Native target selection derives checked headers, sizes, alignments, field offsets, and drop
plans and emits versioned read-only object descriptors. HIR lowering seals its temporary virtual
registers into shared SSA and verifies dominance and block arguments. The VM branch de-SSAs that
graph to portable bytecode and runs the ownership/type verifier after optimization and drop
insertion before bytecode can be returned or executed. The Cranelift branch instead specializes
the same sealed graph directly, without a bytecode round trip. There is no direct executable bypass
around the shared IR. The Cranelift backend uses those descriptors for allocation and
field/tag/capture offsets and generates retain/release, copy-on-write, indirect calls, and recursive
tag-aware destruction directly in machine code; Rust supplies the allocator, process bootstrap,
and the explicitly versioned operating-system service boundary.

## Executable packages

`foster pack` creates a deterministic ZIP-compatible `.fpk` containing compiled bytecode and
application resources. For a directory package, a `resources/` child is included automatically;
use `--resources <directory>` to select a different resource root. Packaged programs run without
their Foster source and can read included files through `std.fs` beneath `resources/`:

```powershell
cargo run --bin foster -- pack path/to/application -o application.fpk
cargo run --bin foster -- run application.fpk
```

The runtime validates the manifest and archive paths, expands resources into an isolated temporary
working directory for the process, and removes that directory after execution. See the
[package format](docs/package-format.md) for the versioned layout and limits.

See [the VM design](docs/vm.md), [binary format](docs/binary-format.md),
[package format](docs/package-format.md), and
[benchmarking guide](docs/benchmarking.md).

## Language server

Start the server over standard input/output with:

```powershell
cargo run --bin foster -- lsp
```

It supports package diagnostics with open-buffer overlays, document symbols, go-to-definition
through imports and receiver-resolved methods, references, identity-aware rename, rich Markdown
documentation hovers, call signature help, inferred type and argument-name inlay hints, and
scope-aware completion. Diagnostics wait for a short typing pause before recompiling, while
interactive requests still compile the latest open-buffer state on demand. Package recompilation
reuses cached parsed modules and reparses only sources whose contents changed. The development VS
Code extension keeps correctly positioned inlay hints in unchanged functions even when another
function currently fails to compile. The server uses the same `compiler::check` frontend as the
CLI `check` command; executable SSA sealing, bytecode verification, and native-subset diagnostics
remain build/run concerns, so ordinary library documents do not need a `main`. The extension lives in
[`editors/vscode`](editors/vscode/README.md).

## Documentation map

- [Semantic specification and conformance gaps](docs/semantics.md)
- [Remote lifecycle, cancellation, and failure contract](docs/remote-semantics.md)
- [Language design and implemented syntax](docs/language-design.md)
- [Source and ownership compatibility policy](docs/compatibility.md)
- [Roadmap](docs/roadmap.md)
- [Ownership and borrowing](docs/ownership.md)
- [Ownership verification](docs/ownership-verification.md)
- [Closures and group borrowing](docs/closures.md)
- [Effect derivation](docs/effect-derivation.md)
- [Compiler diagnostics](docs/diagnostics.md)
- [Testing Foster programs](docs/testing.md)
- [Implemented feature test coverage](docs/test-coverage.md)
- [Register VM](docs/vm.md)
- [Native compilation](docs/native.md)
- [Compiled bytecode format](docs/binary-format.md)
- [Package archive format](docs/package-format.md)
- [Core library](docs/core-library.md)
- [Time in Foster](docs/time.md)
- [Randomness in Foster](docs/random.md)
- [Standard-library source guide](library/README.md)
- [Optimization and benchmarks](docs/benchmarking.md)
- [Executable examples](examples/README.md)

The executable programs under `examples/` demonstrate the current language and library capabilities.
