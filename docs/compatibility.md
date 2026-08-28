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
existing members such as `Bool.not` remain legal. The ownership-model version remains 1 because
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
