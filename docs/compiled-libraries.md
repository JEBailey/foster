# Independently compiled libraries

Symbol descriptors retain canonical structural implementation constraints alongside
parameter types and ownership modes. Importing a constrained method preserves its
requirements; clients cannot call it with a type that lacks the required accessible
members. Interface declarations must match these constraints exactly.

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

Alternatively, declare folders to discover libraries automatically:

```toml
[package]
name = "application"
source = "src"

[discovery]
libraries = ["vendor", "../shared-libraries"]
```

Each folder is relative to `foster.toml` and must exist. Foster discovers `.flib` files directly
inside it, without searching subfolders. For example, `vendor/tzdata.flib` becomes available as
`import tzdata`. Filenames must be valid module names; `core` and `std` remain reserved.
All discovered artifacts become project dependencies, including ones not imported by source.
No folders are searched by default, and this does not change source-module discovery.

Two discovered libraries with the same name (including names differing only by case) are an error;
folder order never selects a winner. An explicit `[dependencies]` entry overrides discovery for
that exact name, allowing a particular artifact or source project to be selected. Listing the
same folder more than once is harmless. These rules also apply to source dependencies with their
own manifests. Existing duplicate-package and artifact compatibility checks still apply.

## Compilation and linking

The artifact contains declaration metadata, checked callable descriptors, and portable generic
register code. Public defaults on composable record surfaces also retain syntax-tree adaptation
templates, separate from the declaration stubs. Function bodies are compiled when the library is
built. At consumption, Foster
checks callers against explicit signatures, structural declarations, constants, ownership modes,
reference groups, effects, suspension contracts, and result-provenance summaries. It does not
parse the original source or recheck ordinary compiled calls. Inheriting a default onto a new
receiver instantiates its template and checks the resulting body for that receiver.

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
A new consumer-defined composition can inherit defaults through the artifact's adaptation
templates. These preserve the donor's lexical module, private helper calls, generic parameters,
and nested closures. Explicit effects remain checked; inferred effects are recomputed for the new
receiver. Local overrides and rightmost compatible defaults retain the source-composition rules.
Private representation methods are not exported as adaptable defaults. Structural calls from
libraries into consumer-defined implementations are supported.

## Container version 2

All integers are little endian. The file contains:

| Field | Representation |
| --- | --- |
| Magic | 8 bytes, `FOSTERLB` |
| Library format version | `u16`, currently 3 |
| Interface length | `u32` |
| Interface | UTF-8 JSON declaration and symbolic metadata |
| Code length | `u32` |
| Code | Bytecode version 29, without inserted drops |

Each section is limited to 256 MiB. Unsupported versions, truncated sections, trailing bytes,
incomplete bindings, inconsistent function declarations/descriptors, and invalid bytecode are
rejected. The interface records the Foster language and ownership-model versions. Constants are
stored as evaluated literal values; declaration stubs and ordinary compiled functions contain no
source bodies. Adaptable defaults have separate syntax-tree bodies in their function contexts;
test bodies are absent. Version 1 libraries must be rebuilt to use this format. Structural
conformance and effect summaries remain checked-compiler facts, not proofs of arbitrary
untrusted implementations.

The Rust API is `foster::library::{build, encode, decode, read}`. `.fbc` remains an executable
format and `.fpk` remains a runnable resource archive; `.flib` is a compiler dependency input.

The optional [tzdata package](../packages/tzdata/README.md) is a complete example: its `.flib`
bundles a pinned IANA dataset as constants and a Foster `TimeZoneDatabase` implementation.
Applications load it as an ordinary dependency, with no source or external database files required.
