# Native runtime

This crate contains the stable native ABI implementation as ordinary Rust source.
It is a workspace member, so workspace checks, Clippy, and unit tests inspect the
runtime directly. Shared filesystem-path, clock, and TCP context behavior lives
in `../host`; the VM and native ABI adapter use that same implementation.

The compiler embeds these source files for standalone distribution. At native
build time it assembles a self-contained source file, appends generated ABI
signature assertions, and compiles a cached rlib. The cache identity includes the
complete assembled source and Rust toolchain/options. Program-specific entry
points and string-storage hooks are supplied by generated Foster object code.
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

Native code generation still requires Rust and the platform linker. This change
moves runtime algorithms out of source-string templates; it does not introduce a
new runtime dependency into standalone Foster executables.
