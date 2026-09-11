# Native runtime

This crate contains the stable native ABI implementation as ordinary Rust source.
It is a workspace member, so workspace checks, Clippy, and unit tests inspect the
runtime directly. Shared filesystem-path, clock, and TCP context behavior lives
in `../host`; the VM and native ABI adapter use that same implementation.

The compiler embeds these source files for standalone distribution. At native
build time it assembles a self-contained source file, appends generated ABI
signature assertions, and uses Cargo to compile a cached rlib with May. The cache
identity includes the assembled source, embedded `native-build.toml` and
`native-build.lock`, and Rust toolchain/options. Program-specific entry points
and string-storage hooks are supplied by generated Foster object code.
The runtime crate therefore is not linked into the compiler itself.

`version.rs` is the single source for the ABI version. Tests supply explicitly
scoped string-hook implementations; compiler native allocation/cleanup tests
exercise the real generated hooks.

Run from the repository root:

```text
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets
cargo test -p foster-host -p foster-native-runtime
cargo test --release --test backend_parity
```

Native code generation requires Rust, Cargo, and the platform linker. Generated
executables remain standalone and do not require a separate May installation.

Actors run on May stackful coroutines. Actor waits suspend rather than block the
worker, and error/cancellation state follows the coroutine. Blocking host work is
offloaded to temporary OS threads with owned inputs. The executable links May
statically; a cold runtime build requires its locked dependencies in Cargo's cache
or access to download them.
