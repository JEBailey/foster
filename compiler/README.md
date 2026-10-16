# Foster compiler

`foster-compiler` owns parsing, name resolution, type/effect/ownership checking,
compiled libraries, shared optimization, and VM/native code generation. Executable
contracts are shared with [bytecode](../bytecode/README.md); platform contracts
come from [host](../host/README.md). VM execution is a development dependency.

`bootstrap/` builds the compiler used to produce the embedded standard library.
Keep its dependencies aligned with this crate when changing the compiler.

From the repository root:

```text
cargo test -p foster-compiler --lib
python tools/integration_tests.py --suite language --jobs 2
cargo test --test backend_parity
```

Start with the [workspace boundaries](../documentation/workspace.md) and
[development policy](../documentation/development-policy.md). Compiler and shared
contract changes run all component suites in CI.
