# Foster development tools

- [driver](driver/README.md): embedded Foster implementations of project creation, project management, formatting, and documentation generation.

- [cbind](cbind/README.md): import C headers into typed Foster bindings and a C bridge.
- [lexer](lexer/README.md): experimental lexer written in Foster.
- [unicode](unicode/README.md): offline Unicode table generator written in Foster.
- [tzdata](tzdata/README.md): timezone database generation and reference checks.

## Integration test performance

`cargo test --test '*' --no-fail-fast` runs all Rust integration targets. The test
profile optimizes the Rust compiler and VM at level 2; each Foster test still chooses
its own VM/native optimization modes. The first build takes longer, while subsequent
test execution benefits from the optimized host code.

Cargo compiles and embeds the C bridge generator's bytecode once, so separate test
processes decode it instead of recompiling Foster source. Changes to `tools/cbind`
or the standard library rebuild the embedded generator.

The Windows C header integration test also reuses its native `cbind` executable in
`target/cbind-test-cache`. Its key includes the compiler executable and the tool's
Foster sources and manifest. Every header import, generated binding, and runtime
check still runs. Delete that cache directory to force a fresh native tool build.
