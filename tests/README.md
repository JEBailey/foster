# Correctness suites

Rust integration targets are grouped by component. Their Cargo names are
registered explicitly in the root `Cargo.toml`, so `cargo test --test native`
and other individual target commands keep the same names.

| Directory / suite | Responsibility |
| --- | --- |
| [language](language/README.md) | Syntax, types, effects, ownership, diagnostics, semantic witnesses |
| [runtime](runtime/README.md) | VM/native parity, memory, actors, host services, buffers, and collections |
| [library](library/README.md) | Library algorithms/contracts, bundled library, Foster suites, optional packages |
| [tooling](tooling/README.md) | CLI, projects, compiled libraries, debugger, documentation, workspace boundaries |
| [cbridge](cbridge/README.md) | C imports, bindings, callbacks, borrowed buffers, pointer contracts |
| `foster/` | Portable language tests written in Foster |
| `fixtures/` | Shared input programs, projects, and foreign source used by integration tests |

From the repository root, using Python 3.11+:

```text
python tools/integration_tests.py --suite language --jobs 2
python tools/integration_tests.py --suite runtime cbridge --jobs 2
python tools/integration_tests.py --test cli --test workspace
python tools/integration_tests.py --suite tooling --list
python tools/integration_tests.py --jobs 2
```

The runner builds selected targets once, then runs binaries with bounded
concurrency. `--threads N` controls threads per binary and `--target-dir PATH`
reuses a build directory. Logs go under that directory's `integration-logs/`.
Suite and individual selections are combined. A failed target does not cancel
the other selected targets.

Add a Rust integration file under its component directory and a matching
`[[test]]` name/path in `Cargo.toml`. CI rejects unregistered, duplicate, or
missing targets. Keep compile-time fixture paths relative to the test file;
runtime paths use the repository root (`CARGO_MANIFEST_DIR`).

Portable behavior belongs in Foster declarations under `foster/` or beside its
library implementation. Reader examples live under `examples/`. Timing harnesses
and application workloads live under [benchmarks](../benchmarks/README.md), and
investigation reports under [research](../research/README.md).

See [testing semantics](../documentation/testing.md) and
[CI selection](../tools/ci/README.md) for coverage rules.
