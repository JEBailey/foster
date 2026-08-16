# Pima-to-Foster example corpus

This directory contains a Foster counterpart for every `.pima` program in
`C:\Users\jason\git\pima\examples`. All counterparts parse, type-check, and execute on the Foster
VM with and without optimization.

The translations preserve each example's useful language or algorithmic idea. They use idiomatic
Foster rather than emulating Pima syntax:

- lexical closures replace Pima's caller-environment `do` blocks;
- records and closed variants replace anonymous objects, tuples, and tagged lists;
- typed result variants replace `throw`/`attempt` where errors are part of the example;
- recursive functions replace `while` until Foster gains loop syntax;
- the map example builds a small typed persistent map from records and lists;
- file and HTTP examples currently keep their pure routing/parsing layers and in-memory fixtures;
  the VM now exposes the `core.io` and `core.net.tcp` capabilities needed to wire them to the host;
- the repository analyzer retains recursive source analysis and concurrent remote workers, using
  supplied source strings instead of unrestricted filesystem traversal.

## Conversion matrix

| Pima source | Foster counterpart | Translation focus |
|---|---|---|
| `birthday_paradox.pima` | `birthday_paradox.foster` | Float arithmetic and recursion |
| `closure.pima` | `closure.foster` | Nested and returned closures |
| `code_blocks.pima` | `code_blocks.foster` | First-class lexical report closures |
| `curried_example.pima` | `curried_example.foster` | Partial application |
| `fibonacci.pima` | `fibonacci.foster` | Recursive functions |
| `file_server.pima` | `file_server.foster` | Static route handling |
| `file_server_lib.pima` | `file_server_lib.foster` | In-memory file-server domain layer |
| `foreach.pima` | `foreach.foster` | Recursive list traversal |
| `function_test.pima` | `function_test.foster` | Callable values and Euclidean GCD |
| `http_server_lib.pima` | `http_server_lib.foster` | Pure HTTP request dispatch |
| `import_test.pima` | `import_test.foster` | Power function; package imports are also covered by `modules/` |
| `json_parser.pima` | `json_parser/` | Full typed recursive-descent JSON parser and actor pipeline package |
| `list.pima` | `list.foster` | Generic and concrete list reversal |
| `maps.pima` | `maps.foster` | Typed persistent map behavior |
| `newton.pima` | `newton.foster` | Float iteration and nested closures |
| `object_test.pima` | `object_test.foster` | Records, methods, and mutation |
| `patterns.pima` | `patterns.foster` | Variant construction and destructuring |
| `repository_analyzer.pima` | `repository_analyzer.foster` | Concurrent remote source workers |
| `repository_analyzer_lib.pima` | `repository_analyzer_lib.foster` | Recursive source metrics |
| `repository_analyzer_test.pima` | `repository_analyzer_test.foster` | Metrics conformance fixture |
| `showcase.pima` | `showcase.foster` | Records, variants, closures, lists, and methods |
| `test.pima` | `test.foster` | Minimal Fibonacci smoke test |
| `timing.pima` | `timing.foster` | Empty-program/import smoke test |
| `while.pima` | `while.foster` | 12,000-step VM-managed recursion |

`closure_ownership.foster`, `power.foster`, `records.foster`, and `recursive_counter.foster` are
additional focused Foster examples produced during the conversion.

The files under `design/` are historical design notes from before the corresponding language
features and executable ports existed. The root-level `.foster` programs are authoritative.

## Validate the corpus

```powershell
Get-ChildItem examples/pima -Filter *.foster | ForEach-Object {
    cargo run --bin foster -- check $_.FullName
    cargo run --bin foster -- run $_.FullName
    cargo run --bin foster -- run $_.FullName --no-optimize
}
```
