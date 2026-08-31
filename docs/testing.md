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
inference, ownership checking, optimization, bytecode verification, and VM execution. A final
expression whose type is not `()` is a type error.

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
cannot express reliably: rejected programs and diagnostic structure, HIR/MIR and bytecode
invariants, malformed artifacts, host filesystem and network setup, native compilation, and CLI
process behavior.

Files under `examples/` demonstrate programs for readers and are intentionally not used as test
fixtures. A behavior needed by a test belongs in `tests/foster/`, `library/`, or a dedicated file
under `tests/fixtures/`.
