# Foster

Foster is an experimental statically typed, general-purpose programming language with compile-time
duck typing: a type conforms when its accessible contract matches, without nominal inheritance or
runtime member lookup. Its defining direction is single ownership with group-parameterized
references, inferred effect contracts, structurally adaptable records, and lightweight remote
objects running on virtual threads. The bootstrap compiler and register VM are written in Rust.

## Try it

```powershell
cargo run --bin foster -- run examples/live_inventory_pipeline.fos
cargo run --bin foster -- check examples/pima/json_parser
cargo run --bin foster -- check tests/fixtures/modules
cargo run --bin foster -- run tests/fixtures/modules --no-optimize
cargo run --bin foster -- docs tests/fixtures/modules
cargo run --bin foster -- docs tests/fixtures/modules --serve
cargo test
```

`run` invokes the zero-argument `main` function. A file is treated as a one-module package; a
directory is discovered as a filesystem module tree whose entry point is `main.fos`.
Optimization is enabled by default and can be selected with `--optimize` or `--no-optimize`.

## Generated documentation

`foster docs [file-or-directory]` type-checks the package and generates a static API site in a
neighboring `documentation/` directory. The site is built from resolved HIR, so signatures include
inferred types and effects. It includes public and private declarations, their visibility, and all
attached Markdown documentation comments.

Use `--output <directory>` to choose another destination. Add `--serve` to start a local server and
open the site in the system browser:

```powershell
foster docs . --serve
foster docs . --output build/api-docs
foster serve-docs documentation
```

Both serving commands accept `--port <number>` and `--no-open`. The latter is useful on headless
machines. Generated `documentation/` directories are ignored during Foster module discovery.

## Language snapshot

The current implementation includes:

- functions, recursion, local inference, explicit generics, closures, and partial application;
- `Bool`, `Int`, binary64 `Float`, `String`, `CodePoint`, `Symbol`, `Unit`, homogeneous `List<T>`,
  and zero-conversion `Sequence<T>` views;
- generic records, associated factories, instance methods, private-by-default declarations, and
  closed variants with exhaustive pattern branches;
- statically checked structural record adaptation, declaration-side composition such as
  `type Text = & Sequence<CodePoint> & { ... }`, and intersection contracts such as
  `Named & Located`;
- borrow-by-default calls, explicit `move`, positional consuming callable types, group references,
  closure capture modes, move/initialization checking, and structural invalidation;
- inferred or explicit `read`, `mut`, `reshape`, `consume`, and `suspend` effects;
- remote objects, virtual threads, FIFO method messages, futures, `await`, transferred messages,
  call-scoped borrowed messages, and persistent remote read loans;
- explicit core-library imports, typed filesystem APIs, and typed TCP connections;
- line, nested block, and Markdown documentation comments;
- a package-aware LSP and VS Code extension; and
- an optional optimizing register-bytecode pipeline with a verifier and iterative VM call frames.

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

`if` is reserved for postfix control guards. The implemented form conditionally returns early:

```foster
func clamp_positive(value: Int) -> Int {
    return 0 if value < 0
    value
}
```

It is not a prefix conditional or a guard for ordinary calls and assignments; use `branch` for
value-producing conditional logic.

Subject branches destructure closed variants:

```foster
import core.result

func unwrap_or(result: Result<Int, String>, fallback: Int) -> Int {
    branch result {
        Result.Ok(value) -> value
        Result.Error(_) -> fallback
    }
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
`core.option`, `std.iter`, `core.result`, `std.sequence`, `core.list`, `std.collections.map`, `std.fs`, and
`std.path`, `std.env`, and `std.net.tcp`. Host-dependent filesystem and socket operations cross a narrow VM boundary; public
types, typed errors, and policy wrappers remain Foster code. See
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
  -> structured register bytecode
  -> optional optimizer
  -> liveness-driven drops
  -> verifier
  -> register VM
```

The VM is the sole execution engine; there is no AST interpreter fallback. Optimization can be
disabled without changing semantics. Programs can be compiled to deterministic `.fbc` artifacts
and run without their source:

```powershell
cargo run --bin foster -- build examples/pima/fibonacci.fos -o fibonacci.fbc
cargo run --bin foster -- run fibonacci.fbc
```

The intended native evolution is to lower a stable
backend-neutral IR to Cranelift for JIT and object-file output. That native backend is not yet
implemented.

See [the VM design](docs/vm.md), [binary format](docs/binary-format.md), and
[benchmarking guide](docs/benchmarking.md).

## Language server

Start the server over standard input/output with:

```powershell
cargo run --bin foster -- lsp
```

It supports package diagnostics with open-buffer overlays, document symbols, go-to-definition
through imports and receiver-resolved methods, references, identity-aware rename, rich Markdown
documentation hovers, call signature help, inferred type and argument-name inlay hints, and
scope-aware completion. The development VS Code extension lives in
[`editors/vscode`](editors/vscode/README.md).

## Documentation map

- [Language design and implemented syntax](docs/language-design.md)
- [Roadmap](docs/roadmap.md)
- [Ownership and borrowing](docs/ownership.md)
- [Closures and group borrowing](docs/closures.md)
- [Effect derivation](docs/effect-derivation.md)
- [Register VM](docs/vm.md)
- [Compiled bytecode format](docs/binary-format.md)
- [Core library](docs/core-library.md)
- [Optimization and benchmarks](docs/benchmarking.md)
- [Executable examples](examples/README.md)

Files under `examples/pima/design/` are explicitly historical notes. Executable `.fos` files and
the documents above describe the current implementation.
