# Testing Foster programs

Foster has first-class top-level test declarations:

```foster
test "list length can be observed" {
    let values = [1]
    println(values.length)
}
```

A test description must be a non-empty string and must be unique within its module. Tests cannot be
`pub`, generic, parameterized, explicitly invoked, or imported. They are deliberately absent from
the module's callable namespace.

Internally, each test is compiled through the ordinary function pipeline as an isolated,
zero-argument function returning `Unit`. Its body receives normal name resolution, type and effect
inference, ownership checking, optimization, bytecode verification, and VM execution. A final
non-`Unit` expression is a type error.

Run every test in a file or package with:

```text
foster test path/to/file.fos
foster test path/to/package
foster test path/to/package --no-optimize
```

Discovery is deterministic: tests are ordered by module and then description. Each invocation uses
a fresh VM call stack. A runtime failure marks that test failed, does not prevent remaining tests
from running, and makes the command exit unsuccessfully.

Compiled `.fbc` files currently omit discovery metadata, so `foster test` operates on Foster source
files and packages. Assertion functions, equality-aware failure rendering, filtering, and captured
test output are the next testing-layer features; the declaration and runner do not introduce a
special assertion or execution model.
