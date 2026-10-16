# Toolchain driver

The root `foster` package joins the compiler, VM, native runtime, and host APIs
into the installed command. `main.rs` dispatches commands; `project.rs` resolves
projects and dependencies; `lsp/`, `debugger/`, `documentation/`, and `foreign/`
own their respective adapters. `lib.rs` exposes orchestration APIs.

Project-management, formatting, and documentation policy are implemented in
[embedded Foster tools](../tools/driver/README.md). Their Rust adapters remain here.
The editor extension lives in [editors](../editors/README.md).

From the repository root:

```text
cargo test -p foster --lib --bins
python tools/integration_tests.py --suite tooling --jobs 2
```

`bin/foster-bench.rs` is the cross-language measurement command. Development-only
Rust profiling examples live in [tools/profiling](../tools/profiling/README.md).
See [workspace architecture](../documentation/workspace.md) for dependency boundaries.
