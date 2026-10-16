# Testing Foster programs

Foster has first-class top-level test declarations:

```foster
test "list length can be observed" {
    let values = [1]
    assert(values.length == 1)
}
```

A test description must be a non-empty string and must be unique within its module. Tests cannot be
`pub`, generic, parameterized, explicitly invoked, or imported. They are deliberately absent from
the module's callable namespace.

Internally, each test is compiled through the ordinary function pipeline as an isolated,
zero-argument function returning `()`. Its body receives normal name resolution, type and effect
inference, ownership checking, shared-SSA sealing/de-SSA, optimization, bytecode verification, and
VM execution. A final expression whose type is not `()` is a type error.

Run every test in a file or package with:

```text
foster test path/to/file.fos
foster test path/to/package
foster test path/to/package --no-optimize
foster test # discovers foster.toml from the current directory
```

Discovery is deterministic: tests are ordered by module and then description. Each invocation uses
a fresh VM call stack. A failed assertion or other runtime failure marks that test failed, does not
prevent remaining tests from running, and makes the command exit unsuccessfully. Assertions stop
their current test immediately and may include a message:

```foster
test "parsed values retain their name" {
    let name = "Foster"
    assert(name.length == 6)
    assert(name == "Foster", "name changed during parsing")
}
```

Compiled `.fbc` files currently omit discovery metadata, so `foster test` operates on Foster source
files and packages. Equality-aware assertion rendering, filtering, and captured test output are the
next testing-layer features.

## Repository test architecture

Portable runtime behavior belongs in Foster `test` declarations. The repository keeps the main
language suite under `tests/foster/`, while standard-library tests live beside their implementation
under `library/`. Run both suites directly with:

```text
foster test tests/foster
foster test tests/foster --no-optimize
foster test library
foster test library --no-optimize
```

`cargo test` also runs both Foster suites in optimized and unoptimized modes, so they remain part of
the ordinary Rust and CI quality gates. Rust tests are reserved for behavior that Foster tests
cannot express reliably: rejected programs and diagnostic structure, HIR/MIR, shared-SSA/de-SSA,
and bytecode invariants, malformed artifacts, host filesystem and network setup, native
compilation, and CLI process behavior.

Files under `examples/` demonstrate programs for readers and are intentionally not used as test
fixtures. A behavior needed by a test belongs in `tests/foster/`, `library/`, or a dedicated file
under `tests/fixtures/`.

## Semantic specification checks

The [semantic specification](semantics.md) records stable rules and links them to behavioral
witnesses. `cargo test --test semantic_specification` checks its version baseline, rule identifiers,
and local link paths. These are editorial integrity checks, not a proof of the rules or a
replacement for compile-pass/compile-fail, runtime, and backend-parity coverage.

## Native text ownership

`cargo test -p foster-compiler --lib managed_text_and_arguments_release_all_native_allocations` builds native
executables with test-only allocation accounting. It exercises argument import, string/byte
storage, records, symbols, closures, remote results, and branch cleanup in both optimization modes, then checks
that all tracked native allocations have been released after the entry result is destroyed.
This covers normal completion, not process-fatal or remote failure cleanup.

## Component suites and local execution

Rust integration targets are grouped under `tests/language`, `tests/runtime`,
`tests/library`, `tests/tooling`, and `tests/cbridge`. The
[test index](../tests/README.md) explains registration and focused commands.
[CI selection](../tools/ci/README.md) maps changes to these suites and retains
broad gates for shared contracts. Performance timing runs are separate from correctness.

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

CI uses this runner with two concurrent integration targets for the selected suites
on Linux and Windows.
Unit tests, binary/example tests, documentation tests, and release backend parity
run in separate steps. The broad CI plan covers the complete Cargo test suite.

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
