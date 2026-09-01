# Compatibility policy

Foster versions three different surfaces independently:

- The package version in `Cargo.toml` versions compiler releases.
- `ownership::LANGUAGE_VERSION` versions breaking source-language and type-system changes.
- `ownership::MODEL_VERSION` versions the normative ownership contract.
- The bytecode format version separately versions serialized executable compatibility.

The language or ownership-model version must increase when an intentional change rejects a
previously accepted safe program, accepts a program that violated a former guarantee, changes group
or lifetime meaning, or reassigns a stable diagnostic code. Increased precision that only accepts
programs previously rejected conservatively may retain the model version when it does not weaken a
guarantee; it must add compatibility witnesses explaining the change.

Every version change requires:

1. Updating the constants and ownership dump golden expectations.
2. Adding old/new compile witnesses to `tests/ownership_soundness.rs`.
3. Updating `docs/ownership.md`, the diagnostic catalog when relevant, and release notes.
4. Stating whether automated source migration is necessary.
5. Preserving the previous compiler release for projects pinned to the older model.

Stable ownership diagnostic codes identify semantic categories, not exact prose. Public group and
effect signatures are source compatibility contracts. Inferred regions and raw ownership MIR are
not stable APIs, but their deterministic debug representation is snapshot-tested so accidental
changes receive review.

Foster is currently pre-1.0, so compatibility may change deliberately. It must nevertheless change
through these gates rather than silently.

## Bytecode format version 16

Bytecode format version 16 appends the `random.bytes` builtin used by `std.random.SystemRandom`
and `std.random.secure`. Existing builtin tags and instruction opcodes are unchanged. Source code
requires no migration, but serialized development bytecode from version 15 must be rebuilt.

## Bytecode format version 15

Bytecode format version 15 appends the `time.wall_now` and `time.monotonic_now` builtins used by
`std.time` clocks. Existing builtin tags and instruction opcodes are unchanged. Source code
requires no migration, but serialized development bytecode from version 14 must be rebuilt.

## Bytecode format version 14

Bytecode format version 14 appends the `io.read_range`, `io.append_bytes`, and `io.file_length`
builtins used by bounded binary streaming. Existing builtin tags and instruction opcodes are
unchanged. Source code requires no migration, but serialized development bytecode from version 13
must be rebuilt.

## Ownership-model version 3

Ownership-model version 3 preserves loan provenance at record fields, constant list indices,
non-copy branch-result temporaries, and enum pattern payload bindings. Different constant list
indices are disjoint. Dynamic indices remain conservative unless a live comparison fact proves
their stable operands unequal.

This precision accepts programs that invalidate a borrower stored at one constant list index and
later use only a different index. It also rejects formerly accepted programs that extract a
borrower through a nested branch or enum payload, invalidate its origin, and then use the extracted
value. Safe source requires no automated migration. A newly rejected program must use the borrower
before invalidation, reacquire it afterward, or move owned data into the aggregate. The bytecode
format remains version 13 because this change affects ownership MIR and analysis only.

The same model version later gained bounded correlation for repeated tests of unchanged boolean and
enum places and direct scalar comparisons. This accepts safe programs whose invalidation and later
borrower use are guarded by contradictory facts, and lets a live inequality distinguish stable
dynamic indices. Mutating any predicate operand discards the correlation. This is a precision-only
extension, requires no migration, and does not change bytecode format 13.

## Ownership-model version 2

Ownership-model version 2 gives borrowed non-place expressions explicit full-expression
temporaries. A temporary remains alive through the call or expression that borrows it, is destroyed
in reverse creation order afterward, and is also destroyed on `return`, `try`, `break`, and
`continue` edges that leave the expression. Borrowers retained beyond that boundary are rejected
with the existing invalidated-loan diagnostic.

The same model version makes assertion failure an explicit ownership-MIR terminal edge, includes
consumed parameters in function destruction, and specifies callee-first, reverse-register VM frame
teardown for runtime failures. These additions strengthen cleanup guarantees without requiring a
source migration.

Code such as `observe(ref (make()))` is now executable instead of failing during VM lowering. Code
that stores and later uses a reference derived from `make()` must bind the owned result first and
borrow that binding, or return an owned value. No automated migration is necessary. Serialized
programs use bytecode format version 13, which adds `MakeWholeReference`; older development
bytecode must be rebuilt.

## Language version 2

Language version 2 reserves `assert` and introduces immediate assertion failures. Source that
previously declared an `assert` function must rename that declaration; no automated migration is
needed for other programs. The ownership-model version remains 1 because assertions add a failure
edge without changing ownership, group, or lifetime guarantees. Serialized bytecode containing the
new instruction uses format version 7.

## Language version 3

Language version 3 reserves `loop`, `break`, and `continue` and introduces statement loops with
nearest-enclosing-loop control transfers. Source that previously used one of those words as an
identifier must rename it; no other automated migration is necessary. The ownership-model version
remains 1 because loop back-edges and exits use the existing ownership control-flow model without
changing ownership, group, or lifetime guarantees. The bytecode format remains version 7 because
the compiler lowers all three statements to existing jumps.

Multiline branch-arm blocks were added without another language-version increase because they
accept syntax that version 3 rejected and reserve no additional identifiers.

## Language version 4

Language version 4 reserves `continue` exclusively for loops. A `continue` inside a branch arm now
targets an enclosing loop, if one exists; otherwise it is a compile error. Branch arms no longer
fall through to later pattern or condition tests. Migrate branch-local continuation by expressing
the selection as guarded conditions or by moving the repeated decision into a loop. The ownership
model and bytecode format remain unchanged.

## Language version 5

Language version 5 reserves `try` and introduces prefix propagation for `Result<T, E>` values.
Source that previously used `try` as a declaration or local name must rename that identifier; no
other automated migration is necessary. A `try expression` evaluates its operand once, yields the
payload of `Result.Ok`, or immediately returns the same `Result.Error` from the enclosing function.
The enclosing function may use a different success type, but its error type must match exactly;
`try` performs no error conversion.

The ownership-model version remains 1 because the success and error edges use the existing
consumption, scope-destruction, and return rules. The bytecode format remains version 7 because
the compiler lowers `try` to existing pattern tests, jumps, and return instructions.

## Language version 6

Language version 6 separates module qualification from access through a type or runtime value.
Module declarations, module-qualified constants, functions, and type names use `::`. Associated
function declarations, type accessors, enum cases, fields, and instance members use `.`. Module
identities in import declarations remain dotted, so `import core.result` is unchanged.

Migrate a module function such as `toml.parse(source)` to `toml::parse(source)` and a qualified type
such as `model.User` to `model::User`.
Type spellings such as `Result.Ok` and `func Box.make` remain dotted, as do runtime expressions such
as `user.name` and `user.rename()`. The compiler reports actionable replacement hints for former
dotted module qualification and accidental `::` type access; no data or ABI migration is needed.

The ownership-model version remains 1 because this is a parse and name-resolution distinction with
no ownership, group, lifetime, or effect change. The bytecode format remains version 7 because
qualification is completely resolved before bytecode lowering.

## Language version 7

Language version 7 reserves `not` as a prefix logical-negation operator equivalent to `!`.
Source that previously used `not` as a bare declaration, parameter, or local name must rename that
identifier; no automated migration is needed for other programs. `not` is contextual after `.`, so
members named `not` remain legal. The ownership-model version remains 1 because
negation retains its existing type and ownership behavior. The bytecode format remains version 7
because both spellings lower to the existing unary-not instruction.

## Core-library receiver API

The pre-1.0 core library now exposes operations with one natural nominal receiver as instance
methods. Migrate calls such as `list::map(values, transform)`, `option::unwrap_or(value, fallback)`,
`int::power(base, exponent)`, and `toml::get(document, key)` to `values.map(transform)`,
`value.unwrap_or(fallback)`, `base.power(exponent)`, and `document.get(key)`. List and string
`empty?` and `length` use their existing properties. Namespace
operations and algorithms over structural contracts remain module functions, including
`toml::parse`, `sequence::map`, `io::copy`, filesystem operations, and path operations. This is a
source-library migration and does not change the language, ownership-model, or bytecode versions.

## Function and method overloads

Foster accepts functions, associated functions, instance methods, and contract requirements with
the same name when their parameter signatures differ. This is additive and therefore does not
advance the source-language version. Existing duplicate declarations remain errors: return types,
ownership modes, effects, and suspension do not distinguish overloads.

Calls resolve by argument count and compatible parameter types. Exact matches are preferred over
matches requiring a lossless conversion; equally ranked matches are rejected as ambiguous.
Overloaded declarations require explicit parameter types, and an overload set cannot be used as a
bare function value because no arguments are available to select one declaration. Contract
composition merges identical requirements and preserves distinct parameter signatures for dynamic
dispatch. The ownership-model version remains unchanged. Type checking assigns program-local
dispatch slots and emits finalized concrete implementation tables; while Foster remains unreleased,
only the current bytecode format is accepted.

Composition now validates implementation availability when a record is instantiated rather than
when its contract is declared. `type C = & A & B` and `type C = & A & B & {}` therefore describe
the same effective contract without repeating inherited methods. An uninstantiated contract may
remain abstract, while constructing a named record still requires compatible implementations for
its complete composed surface. This corrects declaration-time validation and does not change the
source-language, ownership-model, or bytecode versions.

## Typed-resource library API

The pre-1.0 standard library separates resource identity, provider association, and authority.
`std.resource` now defines the explicit `ResourceIdentifier` contract, the generic `Resource<L>`
association, and independent capability contracts such as `Readable<E>`, `Writable<E>`,
`PositionedReadable<E>`, `Appendable<E>`, `Sized<E>`, `Closable<E>`, and `Accepting<C, E>`.
`ReadWrite<E>` is the combined whole-resource read/write contract.

This replaces the earlier erased resource contracts. Migrate `ResourceLocation` to
`ResourceIdentifier`, `Resource` to `Resource<L>`, and each `*Resource<E>` capability to its
shorter independent spelling. Implement `resource_id()` rather than relying on a ubiquitous
`as_string()` member for structural identification. `File.at` now accepts `Path`, while TCP
connections and listeners expose `Resource<TcpEndpoint>`; a parsed `Uri` remains an identifier and
cannot silently become either provider. These pre-1.0 library changes do not advance the language,
ownership-model, or bytecode versions.
