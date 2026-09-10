# Independently compiled libraries

Build a project once and distribute its `.flib` file:

```sh
foster build path/to/library --library -o math.flib
```

A standalone source file or module directory needs an explicit package identity:

```sh
foster build math.fos --library --package-name math -o math.flib
```

A consumer declares the artifact in `foster.toml`:

```toml
[package]
name = "application"

[dependencies]
math = { path = "vendor/math.flib" }
```

Use `import math` in source. The library's `main` module is mounted at `math`; other modules
are mounted under `math.<original-path>`. The dependency alias does not change the package-qualified
symbol identity. `check`, `run`, `build`, native builds, and `pack` use these dependencies.
The library source and its original project directory are not needed by the consumer.

## Compilation and linking

The artifact contains declaration metadata, checked callable descriptors, and portable generic
register code. Function bodies are compiled when the library is built. At consumption, Foster
checks callers against explicit signatures, structural declarations, constants, ownership modes,
reference groups, effects, suspension contracts, and result-provenance summaries. It does not
parse or type-check the library's original function bodies.

The linker resolves package/module/name/descriptor bindings, relocates functions, nominal types,
enum cases, constant pools and structural dispatch slots, and checks nominal layouts. Private
helpers and closures are retained as implementation details. Generic code is specialized by the
normal application backend. Drop insertion and final optimization happen after linking, so `.flib`
code is not a finalized executable. Both VM bytecode and native executables use the linked code.

Source and compiled dependencies used to build a library are bundled into its artifact. A program
must have one definition of each package-qualified module; duplicate bundled modules are rejected.
The embedded `foster` library is supplied by the consuming toolchain. Rebuild libraries when the
library, language, ownership-model, or bytecode format version changes. This format does not
promise compatibility across arbitrary compiler releases or a stable C/native ABI.

Methods materialized by composition *inside* a library retain their compiled implementations.
A new consumer-defined composition must provide its own method implementations: adapting an
inherited source default body to a new receiver is not supported across the binary boundary.
Structural calls from libraries into consumer-defined implementations are supported.

## Container version 1

All integers are little endian. The file contains:

| Field | Representation |
| --- | --- |
| Magic | 8 bytes, `FOSTERLB` |
| Library format version | `u16`, currently 1 |
| Interface length | `u32` |
| Interface | UTF-8 JSON declaration and symbolic metadata |
| Code length | `u32` |
| Code | Bytecode version 26, without inserted drops |

Each section is limited to 256 MiB. Unsupported versions, truncated sections, trailing bytes,
incomplete bindings, inconsistent function declarations/descriptors, and invalid bytecode are
rejected. The interface records the Foster language and ownership-model versions. Constants are
stored as evaluated literal values; function and test source bodies are absent. Structural
conformance and effect summaries remain checked-compiler facts, not proofs of arbitrary
untrusted implementations.

The Rust API is `foster::library::{build, encode, decode, read}`. `.fbc` remains an executable
format and `.fpk` remains a runnable resource archive; `.flib` is a compiler dependency input.
