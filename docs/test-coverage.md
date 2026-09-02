# Test Coverage

This matrix assigns every implemented Foster surface to an automated test layer. It covers the
features documented in [Language Design](language-design.md) and the shipped compiler, VM, CLI,
tooling, package, native, and standard-library surfaces. Roadmap items are excluded until they are
implemented.

Portable successful behavior belongs in Foster `test` declarations. Rust tests own rejected
programs, diagnostic structure, compiler IR, malformed artifacts, operating-system setup, process
boundaries, and tooling protocols. Examples are documentation and are not test fixtures.

| Implemented surface | Foster-native coverage | Rust coverage that remains necessary |
| --- | --- | --- |
| Source modules, nested modules, imports, aliases, qualification, constants, and visibility | `tests/foster/modules.fos` and its `support` modules | Package discovery, dependency graphs, ambiguity, privacy failures, source spans, and project manifests |
| Functions, recursion, generics, inference, methods, and overloads | `tests/foster/functions.fos` and `tests/foster/data.fos` | Invalid signatures, ambiguity, HIR identity, and native reachability |
| Closures, nested functions, capture modes, partial application, and callable ownership | `tests/foster/functions.fos`, `tests/foster/effects.fos`, and `library/core/functions.fos` | Capture classification, invalid escapes, invalidation, and erased callable representation |
| Command entry arguments | Dedicated `tests/fixtures/programs/arguments.fos` | Source, bytecode, native ABI, and process argument integration |
| First-class test declarations and assertions | Every Foster suite | Parser restrictions, discovery order, failure continuation, exit status, and messages |
| Guarded returns, branches, patterns, loops, `break`, and `continue` | `tests/foster/control_flow.fos` | Invalid control placement, exhaustiveness diagnostics, CFG and cleanup edges |
| Logical operators and immediate assertions | `tests/foster/main.fos` and `tests/foster/control_flow.fos` | Invalid operands, native assertion failures, and cleanup paths |
| Records, generic records, aliases, unions, enums, and enum contracts | `tests/foster/data.fos` and `tests/foster/advanced_types.fos` | Construction errors, privacy, recursive aliases, exhaustiveness, and HIR metadata |
| Structural conformance, composition, intersections, and contract dispatch | `tests/foster/data.fos` and `tests/foster/advanced_types.fos` | Missing/incompatible members, private fields, dispatch keys, and inferred structural conversions |
| Unit, Bool, Int, Float, String, CodePoint, Symbol, List, Byte, Bytes, and ByteBuffer | `tests/foster/main.fos`, `tests/foster/values.fos`, and `library/core/*` | Opaque representation, invalid conversions, intrinsic keys, bounds, and optimizer equivalence |
| Integer widening from Byte and CodePoint | `tests/foster/values.fos` | Rejected reverse widening, generic inference, and container invariance |
| Sequence, Collection, Iterable, Iterator, consumers, and lazy adaptors | `tests/foster/iteration.fos`, `library/std/sequence.fos`, and `library/std/iter*` | Contract HIR and invalid composition |
| Callable aliases and equality, ordering, and hashing contracts | `library/core/functions.fos`, `library/core/ordering.fos`, and language contract tests | Structural dispatch metadata and incompatible contracts |
| Move, borrow, reborrow, partial move, last use, and structural invalidation | `tests/foster/ownership.fos` | Compile-fail witnesses, reference model, ownership MIR, provenance, cleanup, and bounded fuzzing |
| Group-parameterized references and read/mut/reshape/consume effects | `tests/foster/effects.fos` | Effect inference, fixed points, warnings, group substitution, and invalid bounds |
| Remote objects, futures, virtual threads, borrowed messages, and persistent read loans | `tests/foster/remote.fos` | Failure propagation, forbidden mutation/messages, serialization, overload dispatch, and suspension inference |
| Result values and `try` propagation | `tests/foster/control_flow.fos` and `library/core/result.fos` | Invalid enclosing/error types and cleanup on propagation paths |
| Generic binary and text stream contracts | `library/std/io.fos` | Host file/socket adapters and invalid capability matches |
| Resource identifiers, capabilities, URI, and Path identity | `library/std/uri.fos` and `library/std/path.fos` | Filesystem provider behavior, capability separation, path host operations, and typed resource integration |
| Filesystem, environment, and TCP | None: these require operating-system resources | `tests/core_host.rs` creates isolated files, environment contexts, listeners, and connections |
| Exact, civil, offset, and zoned time taxonomy; arithmetic; local resolution; ISO/RFC parsing and formatting | `library/std/time.fos` and `library/std/time/*` | `tests/core_host.rs` verifies wall and monotonic clock integration through the generic clock contract |
| Random-source contracts, seeded and split generators, unbiased ranges, distributions, secure bytes/tokens, choice, shuffle, and sampling | `library/std/random.fos` and `library/std/random/*` | `tests/core_host.rs` verifies operating-system entropy through the public random and entropy contracts; VM unit tests verify host-boundary denial |
| TOML 1.1 parsing, typed values, rendering, and errors | `library/std/toml.fos` | Large TOML 1.1 structure integration remains in `tests/core_host.rs` |
| Formatter, generated documentation, and diagnostics | Parser/formatter behavior exercised while loading Foster suites | Rust unit/integration tests verify recovery, spans, rendering, Markdown, escaping, and CLI output |
| LSP diagnostics, navigation, references, rename, hover, completion, signatures, hints, and caching | Uses the same checked frontend and library declarations as the language suite | Dedicated LSP unit/workspace tests own protocol positions, overlays, cancellation, incremental state, and builtin-registry/tooling synchronization |
| Shared SSA, de-SSA bytecode compiler, optimizer, CFG type/ownership verifier, VM, and binary format | Every Foster suite runs optimized and unoptimized bytecode | Dominance and block arguments, edge copies, complete portable-operation sealing, malformed type/ownership/control-flow programs, deterministic encoding, optimizer reductions, and semantic equivalence |
| Native compilation | Portable scalar, record, and enum programs provide source fixtures | Object/descriptor emission, supported ABI/results, record copy-on-write and borrowed calls, enum tags/patterns, generated ownership operations, unsupported reachability, executable output, and native failures |
| Executable packages and resources | Package source is compiled through ordinary Foster semantics | Deterministic archives, validation limits, resource extraction, isolation, and cleanup |
| CLI and project workflow | Foster suites use the public `test` command | Help, init, run/check/build/pack/docs/fmt/test, optimizer flags, manifests, dependencies, and exit behavior |

`tests/foster.rs` enforces the repository-level invariants: both Foster suites run with and without
optimization, discovery cannot silently fall below the reviewed minimum, public library
implementations have Foster tests unless they are explicitly host-integrated, and Rust tests do not
depend on files under `examples/`.

When a feature is added or promoted from the roadmap, update this matrix and add its positive
runtime behavior to a Foster suite whenever the language can express the assertion. Add Rust tests
only for the parts that require compiler internals, rejected input, host setup, or process/protocol
boundaries.
