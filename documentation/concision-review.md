# Foster source concision review

## Scope

The current corpus contains 221 `.fos` files: 61 library modules, 36 example
files, 13 tool files, 12 benchmark files, two optional-package files, and 97 test
files. This review inventories that entire corpus and inspects candidates against
[Writing Foster](writing-foster.md), the [language reference](language-design.md),
[semantics](semantics.md), and [ownership](ownership.md). Embedded Foster in
Rust tests and documentation also supplies intentional syntax and rejection coverage;
it must not be mechanically rewritten to satisfy a style search.

The goal is to use implemented concepts where they clarify code while preserving
ownership, evaluation order, errors, and test intent. A valid older spelling is
not automatically an obsolete language contract.

## Applied concepts

| Concept | Application and evidence |
| --- | --- |
| `while` | Time-zone table scans, binary searches, recurrence searches, and benchmark counters express their continuation condition directly. The Unicode generator emits the same form in its conformance fixture. |
| `try<Case>` | The JSON example propagates fresh `ParseResult` failures with `try<ParseOk>`. Error-detail tests cover fractional numbers, exponents, escapes, and literals. |
| Record destructuring | The JSON example names successful `token`, `rest`, and `value` fields directly after selected propagation. Ownership still follows field-binding rules. |
| `panic` and `Never` | Unrecoverable time-zone invariants no longer fabricate offsets, dates, instants, or integers after failing. Explicit failure arms in library tests, tools, and guide examples use `panic(message)`. |
| String interpolation | The endpoint example and benchmark CSV output use triple-quoted substitutions. Substitutions retain the required conversion imports. |
| Generic implementation parameters and `self: Self` | Library collection and iterator implementations use explicit receiver annotations. Reference and specialized receivers retain their corresponding explicit types. |
| Structural constraints and type branches | The writing guide and `impl_constraints` / `type_branch` fixtures exercise concrete-type-preserving constraints and capability checks. |
| Named scopes and parameter groups | Ownership examples and the `named_scopes` / `parameter_groups` fixtures exercise lifetime boundaries and returned loans. |
| Deferred fields | The writing guide and `deferred_initialization` fixture demonstrate `??` with definite initialization. Fully available records do not benefit from artificial deferred construction. |
| Conditional execution | Production sources already use `if` for guarded actions and `branch` for selecting values. Dedicated fixtures retain both explicit and implicit unit fallbacks. |

## Deliberate retained forms

- Iterator consumers use `loop` plus `next()` because an existing cursor is not
  necessarily an iterable with `iterator()`. Changing them to `for` can restart
  traversal or change ownership. Parser state machines, retry loops, and the loop
  showcase also retain their applicable control-flow forms.
- Branches over borrowed `Result` and parser outcomes remain branches. `try` consumes
  its operand; replacing those branches would change the API's ownership contract.
  Error conversion and recovery also require explicit handling.
- Negative fixtures deliberately contain rejected programs. Syntax, ownership,
  formatter, and backend tests retain explicit receivers, unconditional assertions,
  manual loops, and other forms when those are what the test exercises.
- Generated Unicode and IANA data remain generator-owned. Update the generator and
  verify its output instead of restyling table literals or changing data provenance.
- Index traversal remains appropriate when positions, byte widths, mutation, or
  parallel arrays matter. String iteration uses graphemes; scalar cursors and byte
  scans must preserve their original text units.
- `move`, explicit copies, public signatures, and useful effect bounds remain part
  of the contract. Shorter source must not hide resource transfer or weaken checks.

## Future work is not available syntax

Record-update expressions, compound assignment, tuples or variadic parameters, and
new remaining-iterator adapters require their own designs and implementation.
Custom-case propagation and record destructuring are already implemented; they
must not be described as proposals. Consult the implemented reference before using
any spelling suggested by a roadmap.

## Validation

Use the checkout compiler for package checks and formatting. Keep foundational
library modules in the `library` package context. Run the Foster language and
library suites with and without optimization, the writing-guide and documentation
example tests, and the time-zone integration suite. The latter checks the compiled
library in VM/native modes and compares its results with independent reference data.

Check examples at their actual package roots (`examples/modules` and
`examples/json_parser`), tools using their manifests, and the Taker benchmark with
its sibling source dependency. The Unicode generator's `--check` mode verifies
both generated files against its current source. A successful style scan alone is
not evidence of successful compilation or behavioral equivalence.

The review checked 49 example, benchmark, tool, and package entry points plus the
Taker benchmark with its sibling dependency. All passed. The writing-guide and
time-guide examples, optimized/unoptimized language and library suites, JSON and
future/process integration tests, time-zone VM/native reference comparisons,
`Never`, record-destructuring, and selected-propagation tests passed. Unicode
generation and `--check` agreed, and its regenerated conformance program returned
42. Edited handwritten Foster files passed the checkout's formatter (compiled
natively for the larger files).

The public contract inventory now covers all 150 declarations. Coverage references
identify the existing future/process and JSON integration suites. The storage-free
contract audit uses module-qualified names so a private JSON `Writer` is not
mistaken for `std.io.Writer`.

The collection-contract dispatch discrepancy is fixed: reference-wrapped receivers
retain their nominal dispatch identity, and native specialization and calls preserve
the receiver's storage address. The original collection fixture now returns 42 on
both backends. Regression coverage also checks absent map keys, returning a borrowed
entry through a helper, and mutating the original entry through the shared contract.
The map APIs and ownership contract are unchanged. Borrowed enum payload references
also preserve their original storage across both backends; regression coverage includes
nested patterns, scalar payloads, existing references, and enum contract methods.
