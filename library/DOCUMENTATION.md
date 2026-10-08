# Standard-library documentation standard

Documentation is part of the public API contract. Write it in authoritative
`.fos` source so generated pages, editor hovers, and source readers receive the
same explanation. Describe implemented behavior, not plans or a stronger
contract suggested by a familiar name.

## Module documentation

Begin each handwritten module with `//!` comments describing its purpose and how
to choose the API. Explain its relationship to adjacent modules, important
ownership or representation choices, and material limits. Include a complete
example when it teaches usage better than prose. Generated Unicode tables retain
their generator-owned provenance comments.

Use Markdown paragraphs and fenced `foster` examples. Runnable examples include
imports and `main`, handle fallible results, and demonstrate an observable outcome.
Avoid fragments requiring the reader to guess imports or ownership. Commands
belong in `powershell` or `text` fences.

## Declaration documentation

Start a public type or function comment with its purpose or result. Do not merely
expand the identifier into words. Scale detail to the contract; a scalar copy
operation does not need a template full of empty sections.

Document the applicable details:

- Inputs: valid ranges, inclusive or exclusive endpoints, units, and obligations
  such as same-source checkpoints or stable hashes.
- Ownership: what is consumed, what remains usable, and whether results are
  snapshots, independent copies, or stateful readers.
- Evaluation: callback order and count, eager or lazy work, short-circuiting, and
  whether traversal advances the source.
- Boundaries: empty inputs, absent values, negative counts, invalid offsets,
  overflow preconditions, and floating-point special values.
- Failures: error types or cases, assertions or runtime failures, and partial
  progress or external effects that remain after an error.
- Cost: allocations, traversal, and worst-case behavior when needed to choose
  an API. Do not claim constant-time or allocation-free behavior without evidence.

Type comments explain invariants and construction. Describe field units and
interpretation in the owning type's comment; field-level doc comments are not
currently accepted by the parser. Private helpers need enough
context to explain their role and preconditions.

When a method has both a contract declaration and a separate implementation,
document both consistently: generated lookup and hover may encounter either.
Do not hide public preconditions in a private helper's comment.

## Internal explanations

Use internal comments to explain invariants, preconditions, ownership-sensitive
ordering, units, and algorithm choices that are not evident from the code.
Describe what the caller has already validated and what state a helper changes.
For parsers, explain cursor units, error propagation, and whether partial results
can escape. For arithmetic, explain how boundary cases avoid overflow.

Give private helper documentation a concrete purpose. Do not use placeholder
sentences such as “Internal helper for this module.” Keep straightforward helpers
to one sentence; document shared state invariants once beside its declaration.
Use ordinary `//` comments for local implementation reasoning and `///` for the
contract attached to a declaration. Keep examples focused on public operations.

## Wording and accuracy

Use parameter names exactly as declared, including `self`; remove stale `value`,
`left`, or `base` names after converting functions to methods. Format identifiers
and literals as inline code. Prefer “Unicode scalar,” “UTF-8 byte offset,” and
“element index” over ambiguous “character” or “position.”

Distinguish assertions from typed failures, clamping from validation, expected
performance from worst-case behavior, and flushing from durability. Do not
describe a hash as unique, a partial parser as complete, or an unimplemented
provider as included functionality.

Avoid release-history phrases such as “now supports” in reference comments.
Use release notes for migration history. Include implementation details only
when they explain an observable limit or an informed API choice.

## Review and validation

Read implementations and tests before changing behavioral claims. Compile and
run new examples. Verify bounds and failure descriptions against boundary cases.
Record implementation defects separately rather than silently changing code
during an editorial pass.

Run `foster documentation library --output library/documentation` and inspect generated module/type pages for paragraphs,
code fences, and duplicate declarations. Run library tests and documentation
coverage checks as appropriate. Rebuild the compiler to distribute updated
embedded comments to installed LSP clients; HTML generation does not update an
already installed language server.

Run `cargo test --test library_documentation` after editing examples or the library
guide. This test discovers every `foster` fence in `.fos` documentation comments
and `library/README.md`, compiles each as a complete program, and executes it with
and without optimization. Use assertions for the behavior an example promises.
These examples must be deterministic and need no external files, network, or
interactive devices. Use a `text` fence for deliberately non-runnable sketches.
The test also validates the guide's CLI command names and options using `--help`.
Comment coverage alone does not verify behavioral claims; review implementations
and boundary tests when documenting failures or ownership.

The same test checks attached source comments for every public type, enum, and
callable alias and compares their names with the contract audit's inventory.
Keep that inventory and its declaration count current when adding or removing types.
Compiled coverage in `tests/core_host.rs` also checks module overviews, public
record and enum documentation, and required-method comments.
