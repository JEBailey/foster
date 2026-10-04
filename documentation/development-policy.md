# Pre-release development policy

Foster has one current language, library, compiler, and runtime contract. Before release,
changes do not need to preserve previous source syntax, APIs, runtime behavior, or executable
formats. Do not add compatibility shims, deprecated aliases, migration layers, or release histories.

Update implementations, tests, examples, language-server behavior, and documentation together.
Tests establish the current semantics and reject invalid programs; they do not freeze obsolete
behavior. The [semantic specification](semantics.md) records that contract and its implementation gaps.

Language and ownership revision identifiers describe the current compiler. They do not select
older semantics. Serialized bytecode must match the current format; retain format validation
so stale or malformed executables are rejected safely rather than misinterpreted.

Platform adapters, host interfaces, and runtime representations needed by current programs are
not backward-compatibility layers. Replace them when improving the implementation, preserving
the current semantic contract and testing the replacement before removing the working path.
