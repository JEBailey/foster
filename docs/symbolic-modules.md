# Symbolic modules and callable descriptors

Descriptor version 1, carried by bytecode version 27. Defined in `foster::symbols`.

Foster groups compiled functions and nominal type bindings by package and module. Symbol resolution
happens before execution; the VM and native backend continue to use program-local function IDs.

Inspect a compiled program's module table with:

```text
foster build path/to/project --emit symbols
foster build main.fos --emit symbols --no-optimize
```

This prints JSON without writing an executable. Normal `.fbc` builds and `.fpk` packages retain
the same metadata. The `.fpk` container version remains 1.

## Identity and grouping

Each module contains its package-qualified name, nominal type bindings, function definitions, and
symbolic imports for direct calls and closure references to other modules. Definitions distinguish
public exports from private implementation functions. Internal helpers can be referenced by the
already-checked executable; external `Table::resolve` requests require a public export.

A function's identity is:

```text
package / module / declaration name / receiver flag / parameter overload key
```

The overload key includes parameter types and borrow/consume modes. It excludes top-level return
types, effects, group names, suspension, and result dependencies. Those are checked after lookup.
Nested callable parameter types retain their argument/result shapes, but omit effect restrictions
from the lookup key. No overload search is repeated at link time.

Project identities come from `foster.toml` package names. Dependency mount aliases are removed from
their module paths, so importing the same package under a different alias does not rename its
exports. Embedded library modules use package `foster`. Standalone source without a manifest uses
package `local`; API callers can supply explicit identities in `Package::symbol_modules`. Package
names identify a namespace, not a registry coordinate, version constraint, or cryptographic identity.

Generic parameters are represented by indices; parameter roots use `p0`, `p1`, and so on. Explicit
reference groups use `g0`, `g1`, and so on. Renaming a generic, parameter, or group does not change
the descriptor. An instance receiver occupies parameter zero. Synthetic closures and partial
applications have private ordinal names; those helper identities are not a library compatibility
promise. Executable-local function/type IDs and generic implementation spellings are kept separately
from symbolic identity.

## Semantic descriptors

Descriptors retain checked parameter and result types, generic arity, receiver presence,
borrow/consume modes, group element constraints, effects with projected paths, suspension, and
ownership-MIR result dependencies. Nominal references are package-qualified. Function-valued types
retain their own callable contract. Descriptors do not use the VM verifier's erased `Unknown` as a
substitute for semantic types.

Compatibility is deliberately conservative:

- Parameter/result types, generic arity, receiver status, group constraints, and ownership modes
  must match.
- An implementation may remove effects or narrow a projected effect path. Effect kinds otherwise
  match exactly; the linker does not implement a general effect-subtyping algorithm.
- An implementation may stop suspending, but cannot start suspending without caller permission.
- Result dependencies may shrink. A promised fresh result must remain fresh.

Changing a result type therefore reports an incompatible descriptor rather than changing the
function's lookup key. Restricting effects can preserve compatibility with an existing caller.
Compatibility here describes the callable contract, not unchanged program behavior.

## Link and validation

`symbols::link(&mut program)` resolves each module import by symbolic identity, checks its required
descriptor, and patches calls and closure references to the selected implementation ID. Generic
specialization keys are translated from the import's spellings to the implementation's spellings.
Rebinding is transactional: an unresolved symbol, incompatible contract, invalid binding, or failed
bytecode verification leaves the input unchanged. Already-resolved programs avoid a code copy.

The compiler and bytecode decoder run this linkage step. Validation rejects duplicate module/symbol
or type bindings, missing implementations, missing direct-call imports, mismatched parameter modes,
out-of-range result dependencies, and descriptor types inconsistent with concrete bytecode signature
types. The ordinary VM verifier still checks instruction-level types and ownership. Erased structural
conformance, declared effects, and ownership summaries remain checked-compiler facts; this metadata
is not a proof system for arbitrary untrusted module implementations.

## Independently compiled inputs

[Compiled libraries](compiled-libraries.md) package these descriptors with complete declaration
interfaces and portable generic code. The `.flib` linker relocates program-local IDs before the
normal executable linkage and backend lowering. Runtime module loading and a stable native
library ABI are not provided.