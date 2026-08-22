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
