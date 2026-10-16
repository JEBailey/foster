# Rust workspace

Foster has six primary Rust packages and a build-host bootstrap compiler in one
repository. The installed command is `foster`; the other primary packages expose
reusable implementation libraries.

Each implementation area has a short entry guide: [driver](../src/README.md),
[compiler](../compiler/README.md), [bytecode](../bytecode/README.md),
[VM](../vm/README.md), [runtime](../runtime/README.md), and [host](../host/README.md).
The [repository map](../README.md#working-in-the-repository) also points to optional
packages, editor integration, component tests, benchmarks, and research.

| Package | Source | Responsibility |
| --- | --- | --- |
| `foster` | `src/` | Toolchain driver: commands, project discovery and dependency resolution, formatting, executable packaging, documentation, language server, debugger, and C bridge builds |
| `foster-compiler` | `compiler/src/` | Parsing, name resolution, type/effect/ownership checking, compiled libraries, optimization, and bytecode/native generation |
| `foster-vm` | `vm/src/` | Execution state, values, builtin handlers, and bytecode execution |
| `foster-bytecode` | `bytecode/src/` | Executable identities and types, instructions, metadata, intrinsic contracts, encoding/decoding, linking, and validation |
| `foster-native-runtime` | `runtime/src/` | Runtime support linked into native Foster executables |
| `foster-host` | `host/src/` | Platform services, processes, foreign calls and callbacks, and shared remote lifecycle control |
| `foster-bootstrap` | `compiler/bootstrap/` | Build-host compiler used to generate the embedded library before the ordinary compiler builds |

## Dependency boundaries

The VM depends on bytecode and host services. Neither bytecode nor host services
has a production dependency on the compiler or toolchain driver. The native
runtime depends on host services.

The compiler depends on bytecode and host contracts. It does not depend on VM
execution in production. The toolchain driver combines compilation and execution
and exposes its orchestration API from `src/lib.rs` and `src/vm.rs`.

Bytecode also owns the executable SSA representation and logical flow engine
used by both the compiler and bytecode verifier. Keeping this shared validation
code together prevents compilation and decoding from checking different contracts.
Source AST, HIR declarations, semantic types, and ownership analysis remain in the
compiler. Executable IDs use declaration-independent markers; the compiler maps
those identities to its declaration arenas.

Intrinsic descriptions contain data, not pointers into the VM. The VM registers
its own handlers and tests their consistency with the shared execution contracts.

The project manager implements the compiler's `package::ProjectInputs` interface.
It resolves manifests and dependency locations; the compiler consumes those
locations, resolves imported names, and checks visibility and semantics. The
formatter uses the compiler parser. The debugger requests bytecode plus its local
register map through `vm::compile_debug`.

Project creation, manifest validation, library discovery, dependency traversal,
formatting policy, and documentation page generation are written in Foster under [`tools/driver`](../tools/driver/README.md).
The Rust build compiles these tools into embedded bytecode; the driver loads them once per process. Rust adapters retain
platform path ancestry, CLI parsing, parser diagnostics, and compiler-facing data
conversion. Documentation adapters supply resolved signatures, type links, and
Markdown HTML; Foster assembles and writes the site. Installed commands do not
depend on a source checkout.

Some unit tests use a **development-only** compiler dependency to produce real
executable fixtures. Bytecode tests transfer those fixtures through the serialized
format. This does not add the compiler to an ordinary VM or bytecode build.

## Build and test

From the repository root:

```powershell
cargo build --release --bin foster
cargo check -p foster-vm
cargo check -p foster-bytecode
cargo test --workspace
cargo test --test agent_documentation
python tools/integration_tests.py --suite language runtime --jobs 2
cargo fmt --all -- --check
```

The workspace default members include all six packages. Existing CLI commands,
Foster package layout, and executable formats are unchanged. Native runtime source
assembly follows the owning packages when producing its cached standalone runtime.

Integration source is grouped by component while Cargo target names remain
registered in the root manifest. See [tests](../tests/README.md) for focused runs
and [CI selection](../tools/ci/README.md) for change-based coverage. Shared
compiler/runtime/ABI/library changes run the broad correctness gates.
