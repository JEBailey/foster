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

Run `foster docs library` and inspect generated module/type pages for paragraphs,
code fences, and duplicate declarations. Run library tests and documentation
coverage checks as appropriate. Rebuild the compiler to distribute updated
embedded comments to installed LSP clients; HTML generation does not update an
already installed language server.
