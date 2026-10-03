# Build-host compiler

This build dependency of `foster-compiler` compiles the shared `compiler/src/lib.rs`
implementation with the `foster_bootstrap` build-host configuration.
`compiler/build.rs` uses it to check and compile the bundled Foster library before
the ordinary compiler embeds that artifact. The build-host compiler reads library
source, avoiding a dependency cycle on the artifact it is producing.

Keep its dependencies aligned with `compiler/Cargo.toml`. Compiler and library
source changes rebuild the generated artifacts. Tests run against the ordinary
compiler, which consumes those artifacts.
