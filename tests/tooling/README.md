# Toolchain integration correctness

These targets check command behavior, project/dependency discovery, compiled
libraries, debugger integration, generated documentation, agent examples, standard
input, and workspace dependency boundaries. Language-server unit tests remain in
the driver's `src/lsp/` modules.

```text
python tools/integration_tests.py --suite tooling --jobs 2
cargo test -p foster --lib
```

Run from the repository root. See [test registration](../README.md).
