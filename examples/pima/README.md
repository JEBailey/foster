# Pima-to-Foster example corpus

This directory contains a Foster counterpart for every `.pima` program in
`C:\Users\jason\git\pima\examples`. All counterparts parse, type-check, and execute on the Foster
VM with and without optimization.

The translations preserve each example's useful language or algorithmic idea. They use idiomatic
Foster rather than emulating Pima syntax:

- lexical closures replace Pima's caller-environment `do` blocks;
- records, union contracts, and enums replace anonymous objects, tuples, and tagged lists;
- typed result enums replace `throw`/`attempt` where errors are part of the example;
- recursive functions replace `while` until Foster gains loop syntax, using postfix guards for
  simple exit conditions;
- iterator consumers and lazy adaptors replace hand-written collection traversal where iteration,
  rather than recursion itself, is the example;
- the map example builds a small typed persistent map from records and lists;
- file and HTTP examples currently keep their pure routing/parsing layers and in-memory fixtures;
  the VM now exposes the `std.fs` and `std.net.tcp` capabilities needed to wire them to the host;
- the repository analyzer retains recursive source analysis and concurrent remote workers, using
  supplied source strings instead of unrestricted filesystem traversal.

## Conversion matrix

| Pima source | Foster counterpart | Translation focus |
|---|---|---|
| `birthday_paradox.pima` | `birthday_paradox.fos` | Float arithmetic and recursion |
| `closure.pima` | `closure.fos` | Nested and returned closures |
| `code_blocks.pima` | `code_blocks.fos` | First-class lexical report closures |
| `curried_example.pima` | `curried_example.fos` | Partial application |
| `fibonacci.pima` | `fibonacci.fos` | Recursive functions |
| `file_server.pima` | `file_server.fos` | Static route handling |
| `file_server_lib.pima` | `file_server_lib.fos` | In-memory file-server domain layer |
| `foreach.pima` | `foreach.fos` | Foster-written iterator consumption |
| `function_test.pima` | `function_test.fos` | Callable values and Euclidean GCD |
| `http_server_lib.pima` | `http_server_lib.fos` | Pure HTTP request dispatch |
| `import_test.pima` | `import_test.fos` | Power function; package imports are also covered by `modules/` |
| `json_parser.pima` | `json_parser/` | Full typed recursive-descent JSON parser and actor pipeline package |
| `list.pima` | `list.fos` | Generic and concrete list reversal |
| `maps.pima` | `maps.fos` | Typed persistent map behavior |
| `newton.pima` | `newton.fos` | Float iteration and nested closures |
| `object_test.pima` | `object_test.fos` | Records, methods, and mutation |
| `patterns.pima` | `patterns.fos` | Enum construction and destructuring |
| `repository_analyzer.pima` | `repository_analyzer.fos` | Concurrent remote source workers |
| `repository_analyzer_lib.pima` | `repository_analyzer_lib.fos` | Recursive source metrics |
| `repository_analyzer_test.pima` | `repository_analyzer_test.fos` | Metrics conformance fixture |
| `showcase.pima` | `showcase.fos` | Records, enums, closures, lazy filtering, and methods |
| `test.pima` | `test.fos` | Minimal Fibonacci smoke test |
| `timing.pima` | `timing.fos` | Empty-program/import smoke test |
| `while.pima` | `while.fos` | 12,000-step VM-managed recursion |

`closure_ownership.fos`, `power.fos`, `records.fos`, and `recursive_counter.fos` are
additional focused Foster examples produced during the conversion.

The files under `design/` are historical design notes from before the corresponding language
features and executable ports existed. The root-level `.fos` programs are authoritative.

## Validate the corpus

```powershell
Get-ChildItem examples/pima -Filter *.fos | ForEach-Object {
    cargo run --bin foster -- check $_.FullName
    cargo run --bin foster -- run $_.FullName
    cargo run --bin foster -- run $_.FullName --no-optimize
}
```
