# Working in the Foster repository

Foster is its own language. Do not translate Rust, Python, JavaScript, or other
language syntax by resemblance. The compiler is implemented in Rust; programs
and the standard library are written in `.fos` files.

## Before writing Foster

1. Read [Writing Foster](docs/writing-foster.md). Its complete examples are checked
   by `tests/agent_documentation.rs`.
2. Find a nearby `.fos` example and read the declarations of the library methods
   you plan to use. Search `library/` for the exact method and its effects.
3. Consult [language design](docs/language-design.md) for syntax and
   [semantics](docs/semantics.md) for the behavioral contract. For moves, references,
   or mutation, also consult [ownership](docs/ownership.md).

## Rules that are easy to get wrong

- Use `func`, mutable `let`, newline-separated statements, and `()` for unit.
- Use `branch` for conditional selection. `if` only guards `return`, `break`, or
  `continue`; there is no general `if/else` statement.
- Use dotted imports, `module::function()`, `Type.factory()`, `Enum.Case(...)`,
  `value.field`, and `value.method()`. Zero-argument methods still require `()`.
- Imports expose public declarations; a public type does not make its fields public.
- Ordinary call arguments borrow by default. Consuming parameters take an existing
  owned binding with `move`. Explicit reference types use groups, not Rust lifetimes.
- Read the actual API before choosing a collection operation: consuming a collection,
  borrowing it, and copying elements have different contracts.
- Generated API signatures show resolved reference notation. Use `.fos` source and
  the writing guide for syntax to copy.

## Validate the work

From the repository root, use the checkout's compiler:

```powershell
cargo run --bin foster -- check path/to/program.fos
cargo run --bin foster -- run path/to/program.fos
cargo run --bin foster -- fmt path/to/program.fos --check
cargo test --test agent_documentation
```

Run the relevant Foster tests with `cargo run --bin foster -- test <package>` and
targeted Rust tests for compiler changes. Test foundational library modules in
their `library` package context; compiling `core/int.fos` alone changes its module
identity. Check `--help` before guessing command options.

Keep code, examples, tests, and documentation consistent. Follow the
[development policy](docs/development-policy.md): maintain the current contract,
without compatibility shims for obsolete pre-release behavior. For library API
comments follow [the documentation standard](library/DOCUMENTATION.md); update
both required-method and implementation comments when both exist.

Prefer implemented documentation and tested source over roadmap proposals. If
they disagree, reproduce the discrepancy and report it; do not invent syntax or
weaken ownership checks to make an example compile. Preserve unrelated changes.
