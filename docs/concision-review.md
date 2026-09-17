# Foster source concision review

Review snapshot: 2026-09-17. This is an assessment, not a change to the language contract.

## Scope and evidence

Scanned all 190 tracked `.fos` files in Foster, including the library, examples,
fixtures, benchmarks, and tools, plus all 16 discovered `.fos` files in
`../taker_foster`. No `.fos` files were found under `../taker`.
Inspected representative implementations for each finding against the writing
guide, implemented language specification, ownership rules, and library declarations.
This is a corpus-wide pattern review with focused source inspection, not a claim
that every function or embedded Rust test program was semantically audited.

The following are textual occurrence counts, including tests and comments where
they match. They locate opportunities; they are not counts of safe replacements.

| Pattern | Foster | Taker Foster |
| --- | ---: | ---: |
| `loop {` | 149 | 54 |
| `for name in ...` | 11 | 0 |
| `while ...` | 38 | 1 |
| Simple `name = name + 1` | 122 | 47 |
| Repeated `field: field` spelling | 18 | 9 |
| Direct `Result.Error(error)` forwarding arms | 8 | 0 |
| Direct `Failed(error)` forwarding arms | 0 | 35 |
| `break if !(true)` | 0 | 3 |

Generated Unicode data and deliberate language/ownership fixtures should not be
rewritten simply to reduce these totals. Change generators rather than generated tables.

## Changes supported by the current language

### 1. Adopt structured iteration where its contract matches

Highest-volume opportunity: `library/core/list.fos`, `library/core/string.fos`,
`tools/unicode/src/generate.fos`, and Taker's parser loops.
Replace a loop's initial exit test with `while` when it expresses the actual
continuation condition. Remove the three always-false break guards in
`../taker_foster/src/combinators.fos`.

Use `for value in values` for sequential traversal when iterator semantics match.
This removes the index binding, bounds test, indexed read, and increment.
Keep indexing when the position matters, or when mutation requires access to
original storage. `List.read_at` explicitly copies; changing it to iteration is
not automatically equivalent. Likewise, a bare `Iterator` currently lacks the
`iterator()` operation that `for` requires.

`library/std/toml.fos` also retains recursive sequential control flow in
`parse_statements`, `skip_document_space`, and helpers such as `key_parts`.
Use `while` for repeated parser statements and consider `map` for pure element
transformations. Besides concision, this can avoid call-stack growth. Preserve
early failure, evaluation order, and ownership when migrating.

### 2. Move repeated type parameters into `impl` headers

For example, `library/std/collections/map.fos` repeatedly spells
`func length<K, V>(self: ListMap<K, V>)` and the same receiver on adjacent methods.
The current language supports:

```foster
impl ListMap<K, V> {
    pub func length(self) -> Int { self.entries.length }
}
```

Apply the same organization to `Option`, `Result`, iterator adaptors, and
collection implementations. Keep method-only parameters on the method:
`impl Option<T> { ... func map<U>(...) ... }`.
Preserve explicit reference receivers such as `List.remove`'s `ref[self] List<T>`;
omitting that annotation is not merely cosmetic.

### 3. Use existing field shorthand and simple propagation

`Frame { rule: rule, position: position }` can be `Frame { rule, position }`.
Shorthand does not insert a copy or change a move into a borrow.

In `library/std/path.fos`, `path_result` can use the existing propagation form:

```foster
let value = try move outcome
Result.Ok(Path.from(move value))
```

`TcpHost.read` in `library/std/net/tcp.fos` has the same opportunity. Keep branches
that translate errors, recover, or inspect failure details. Do not replace an
ownership-transferring branch with `Result.map` blindly: its callback contract
currently borrows its parameter.

### 4. Consolidate genuinely shared implementations

`Collection.empty?` is a candidate for a default body based on `length() == 0`.
The language already supports composed defaults; the roadmap correctly calls
for testing this across concrete collections first. Retain overrides where
length is expensive or representation-specific behavior matters.

Private helpers can rely on inferred effects where an explicit contract adds
no useful boundary. Keep public ownership/effect contracts and diagnostic fixtures.
Likewise, remove redundant `()` after a unit-producing loop, but retain it after
assignments where it determines the function's result.

### 5. Express capability-dependent application behavior with ordinary types

Use `impl Box<T & Copy>` when an operation requires copying, and
`branch value { is Copy -> ... _ -> ... }` when failure is part of the operation.
This uses the recent general type-branch work without giving `Copy` special syntax.

The low-level `can_copy_at`/`copy_at` pair in `library/core/list.fos` is a candidate
for a separate implementation experiment, not an automatic replacement.
Narrowing currently follows named locals/parameters, and binding an owned field
to a new local can move it. A shorter capability check must preserve the list,
invoke user copying code exactly once, and retain bounds-before-copyability errors.

## Language and library improvements worth designing

### 1. Propagation for custom outcome types — greatest benefit in Taker

`../taker_foster/src/taker.fos:162` nests two branches in `then`; 35 matching
failure-forwarding arms occur across Taker. Existing `try` handles `Result`, not
`ParseResult`.

First compare a library migration to `Result<Matched<A>, Failure>` with keeping
the domain-specific enum and defining a structural propagation protocol. If the
latter wins, reuse `try` rather than inventing another propagation operator.
Required decisions include the success payload, residual/error conversion,
consumption, cleanup, and which enclosing closure receives an early return.
Partial/committed parse failures must still retain their explicit recovery rules.

### 2. Record updates with explicit ownership

`Input` is rebuilt in `skip`, `success`, and `failure` in
`../taker_foster/src/taker.fos:95`, and `Failure` is rebuilt to change flags or context.
This duplicates unchanged fields and makes new fields costly to maintain.

First centralize copying and checkpoint construction in ordinary helpers.
Then design a record-update expression that transfers untouched fields from an
owned base. Updating a borrowed base must require an explicit copy or fail;
it must not silently copy noncopyable members. Functional record updates are
already an open roadmap item. No spelling is assumed here.

### 3. Non-returning expressions

`taker::value` ends its failure arm with `assert(false)` followed by a recursive
dummy value. `current_char`, the Unicode generator, and core string construction
also manufacture unreachable fallback values to satisfy result typing.

A non-returning operation and proper divergence typing would eliminate these
dummy results. It must participate in branch joins and normal failure cleanup.
An `expect` convenience can build on that mechanism; it should not replace typed
error handling where recovery is intended.

### 4. Consume the remainder of an iterator concisely

`library/std/iter.fos` repeats `loop` + `branch self.next()` + `None -> break`.
Either provide an explicit remaining-items adapter usable by `for`, or define
how `for` directly accepts an `Iterator`. Keep the distinction between opening
an independent traversal and advancing an existing cursor. Pattern-binding
loop syntax is another option, but does not need to be the first solution.

### 5. Destructuring before variadics

`../taker_foster/src/apply.fos:8` through `map8` repeatedly walks nested
`Pair.first.first...` fields. Record patterns would name those components once
and clarify which fields are moved. They would not eliminate every arity-specific
function; tuples or parameter packs are a separate, substantially larger decision.

### 6. Compound assignment, with specified evaluation order

There are 169 simple increment spellings in the scanned source. `index += 1`
would help, but it saves less structural complexity than the items above.
For projected destinations, define single evaluation and the order of the
right-hand expression, destination selection, and reading its previous value.
Preserve overflow behavior and reference invalidation rules. Do not implement
it as naive textual substitution of `place = place + value`.

## Migration constraints

- String iteration yields grapheme strings; `StringCursor.next()` yields Unicode
  scalars. Taker's `starts_with?` combines those two kinds at `src/taker.fos:70`.
  Choose a consistent text unit before shortening that loop. A scalar cursor
  avoids materializing a code-point list when allocation matters.
- Ordinary references permit mutation. `borrow` is not a copying lookup and is
  not a read-only reference type. Do not shorten away needed `.copy()` or `move`.
- `is Copy` is an ordinary structural type test. Do not reintroduce special
  capability syntax during this cleanup.
- Some `get` examples describe custom types, and TOML has its own `get` operations.
  A global textual replacement would not respect their contracts.
- Keep explicit imports, public return types, and useful effect bounds. The aim is
  less repeated control flow and ownership plumbing, not fewer informative tokens.

## Suggested order

1. Migrate representative library and Taker modules to current loops, generic
   implementation headers, field shorthand, and `try`; measure actual diff savings.
2. Address Taker's text-unit/API migration issues and centralize record copying.
3. Decide custom-outcome propagation and divergence typing using Taker examples.
4. Design record updates, remaining-iterator traversal, and record patterns.
5. Consider compound assignment after loop migrations reveal how much remains.

Validation for this review: a standalone program exercising `for`, `while`,
generic constrained `impl`, omitted receiver annotations, field shorthand, and
`try move` passed the checkout compiler's `check` and returned `42` with `run`.
This validates representative current syntax, not every proposed rewrite.
Checking `../taker_foster` with the checkout compiler reproduced the
`taker.starts_with?` error at line 76: `Option<CodePoint>` is compared with
`Option<String>`. The package therefore does not currently pass checking;
additional errors may appear after that first failure is corrected.
No production Foster source or compiler behavior was changed by this review.

## Follow-up: traversal migration

The subsequent traversal migration applies `for` to suitable collection, byte,
grapheme, and Unicode-input scans, and `while` to indexed mutation, explicit-copy,
and scalar-cursor scans. TOML statement and document-space traversal use loops
instead of recursion. Stateful iterator consumers and deliberately illustrative
loop fixtures retain `loop`; no new iteration syntax or compiler behavior is required.

Taker's prefix comparison now uses two scalar cursors, resolving the type mismatch
recorded above. Its regression covers combining marks, an advanced checkpoint,
an empty prefix, and an overlong prefix. Traversal changes preserve explicit
copying and the distinction between Unicode scalars and grapheme clusters.
