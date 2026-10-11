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

For bounded concurrency across integration targets, run
`python tools/integration_tests.py --jobs 2`. The runner builds all integration
targets once, runs every target even if another fails, and returns a failing exit
code if any target fails. Logs are saved under `target/integration-logs`.
Use repeated `--test NAME` options to select targets, `--target-dir PATH` to reuse
another build directory, and `--threads N` to set threads within each binary.
By default, each binary receives CPU count divided by jobs (at least one).
Start with two jobs and reduce threads if native builds exhaust memory.

CI uses this runner with two concurrent integration targets on Linux and Windows.
Unit tests, binary/example tests, documentation tests, and release backend parity
run in separate steps, preserving the coverage of the complete Cargo test suite.

Checked compilations retain baseline and optimized shared SSA separately. VM and
native lowering reuse that verified boundary without repeating sealing or shared
optimization. Backend-specific lowering and both optimization modes still run.
Native builds also identify each Rust compiler once per process. CI caches Cargo
build artifacts and the native runtime; runtime cache identities still validate
the toolchain, source, dependencies, and optimization mode before reuse.

To inspect the remaining costs, set `FOSTER_COMPILER_PROFILE=1` and run a selected
test with `--nocapture`. JSON timing reports on stderr distinguish shared IR,
VM lowering, native emission, and linking. Timing reports include nested phases;
do not add inclusive timings together.
