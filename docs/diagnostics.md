# Foster diagnostics

**Status:** structured compiler diagnostics and stable ownership-code catalog implemented.

Compiler diagnostics are semantic data rather than preformatted strings. A diagnostic can carry:

- a stable error or warning code;
- one primary source span;
- any number of labeled secondary spans;
- explanatory notes;
- actionable help text;
- the source module that owns its byte-offset spans.

The terminal renderer uses Ariadne. The language server consumes the same diagnostic object and
preserves its code, primary range, label explanations, notes, and help. Compiler phases should not
format source locations into their message text or discard spans by converting an error to a plain
runtime string.

Ownership diagnostics currently reserve these codes:

| Code | Meaning |
| --- | --- |
| `E0382` | A value is used after ownership moved, or before all paths initialize it. |
| `E0401` | A projected borrow is used after a `reshape` or `consume` invalidated it. |
| `E0402` | A returned value exposes a borrow that its result contract cannot carry. |
| `E0403` | A value borrowing an owner is stored back into that same owner. |
| `E0507` | A borrow-by-default parameter is consumed without a consuming contract. |
| `E0728` | A loan required after suspension is not backed by storage in the parked invocation. |

These identifiers are defined once in `src/ownership/diagnostics.rs`. Their semantic categories are
stable within an ownership-model version; wording, labels, and help may improve without changing the
code. Reassigning a code to a different category requires an ownership-model version increment.

For example, `E0401` identifies three distinct events: the invalid use as the primary label, the
binding that retained the borrow, and the operation whose effect invalidated it. Help recommends
moving the use before the reshape or reacquiring the reference afterward.

Ranges are UTF-8 byte offsets internally. Terminal rendering resolves them against module source;
the LSP boundary converts them to UTF-16 line and column positions. Tests cover both structured
fields and rendered output so changing prose or losing a label is intentional and reviewable.

The remaining compiler phases should migrate incrementally to this representation. Parser errors
already retain line and column positions and are adapted into a primary label; type, group, effect,
package, and VM-verifier errors still contain cases that need richer phase-specific spans and
codes.

`foster check <path> --dump-ownership` prints deterministic ownership MIR, loan ancestry, result
provenance, and inferred region point sets. The dump begins with language and ownership-model
versions and is suitable for golden tests and bug reports.
