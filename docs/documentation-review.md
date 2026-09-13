# Documentation review — 2026-09-12

## Scope and assessment

Reviewed the documentation entry points, library documentation standard, generated
page renderer, and representative source comments for lists, strings, iterators,
streams, time zones, and host providers. Checked local Markdown file destinations
across the repository README, library guide/standard, and existing `docs` guides.
This is a usability and presentation review, not an exhaustive verification of
every API claim or executable example.

The existing level of library detail is appropriate: summaries explain purpose,
while ownership, bounds, units, and typed failures receive additional detail where
they change how callers use an operation. In particular, list copying failures,
Unicode position units, partial stream progress, and advisory readiness should
remain explicit. The library documentation standard already calls for this level
of detail without requiring repetitive sections for simple operations.

## Changes made

- Render every overload's signature, visibility, and documentation under one
  function navigation entry. Previously only the first overload was rendered,
  hiding valid call forms and their contracts. Public type summaries also select
  a public overload when a private overload is declared first.
- Add a collapsible reading guide to module pages. It explains member visibility,
  field access versus method calls, ownership/effects, and optional/typed failures.
  It explicitly identifies resolved signatures as reference notation, not source
  declarations to paste into a program.
- Correct record signatures to show `=` and composition separators.
- Add `std.host` to library API selection and link its provider guide.
- Add a documentation entry map separating language/library use from compiler
  internals, and clarify overload grouping/counts in the repository README.

## Maintenance priorities

Keep preconditions and failure behavior in source API comments, with broader
examples in guides. Keep embedding setup in the host-provider guide rather than
repeating Rust configuration in every Foster method comment. Preserve explicit
private/public labels instead of suggesting that a public type exposes all fields.

Documentation presence checks cannot establish semantic accuracy. Boundary claims
and examples still require implementation review and appropriate execution tests
when their APIs change. Local-file link checks do not validate remote URLs or
Markdown heading anchors. Field-level documentation remains unsupported by the
parser; describe field units and invariants in the owning type comment.

## Validation

All eight documentation-renderer tests passed, including overload visibility and
description coverage, field labels, and generated type-link destinations. The
standard-library documentation-coverage test passed. A local-file link scan of
33 Markdown files, including the new guides, found no missing destinations.
