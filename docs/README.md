# Foster documentation

Choose a starting point based on what you need to do. The language and library
references describe implemented behavior; the roadmap describes proposed work.

| Goal | Start here | More detail |
| --- | --- | --- |
| Run a first program or create a project | [Repository README](../README.md) | [Examples](../examples/README.md) |
| Generate or edit Foster with a coding agent | [Agent instructions](../AGENTS.md) | [Writing Foster](writing-foster.md), with executable examples |
| Learn syntax and language rules | [Language design](language-design.md) | [Semantic contract](semantics.md) |
| Choose library APIs | [Library guide](../library/README.md) | [Library reference](core-library.md); generate the API site with `cargo run --bin foster -- docs library --serve` |
| Understand ownership and mutation | [Ownership](ownership.md) | [Closures](closures.md), [effects](effect-derivation.md), [remote objects](remote-semantics.md) |
| Work with specialized library APIs | [Time](time.md), [randomness](random.md), [hash collections](hash-collections.md) | [Host providers](host-providers.md) |
| Understand text and Unicode data | [Unicode text](unicode.md) | [Unicode table maintenance](../tools/unicode/README.md) |
| Build or distribute a program | [Native backend](native.md), [packages](package-format.md) | [Compiled libraries](compiled-libraries.md) |
| Contribute to Foster | [Development policy](development-policy.md), [testing](testing.md) | [Documentation standard](../library/DOCUMENTATION.md), [roadmap](roadmap.md) |
| Investigate compiler behavior | [Diagnostics](diagnostics.md), [VM](vm.md) | [Incremental checking](incremental-checking.md), [ownership verification](ownership-verification.md), [benchmarking](benchmarking.md), [LSP performance](lsp-performance.md) |

API comments in `.fos` source are authoritative for individual operations and feed
the generated reference and editor hovers. Guides supply context and complete
examples. Internal formats and analysis documents target compiler contributors;
they are not prerequisites for using the language.

The generated reference includes private implementation details with explicit
visibility labels. Start with public operations, and read each operation's bounds,
ownership, and failure description before relying on behavior suggested by its name.
