# Native compilation

Status: host-native AOT backend implemented with Cranelift; scalar, aggregate, concrete and erased
callable, list, string/bytes, erased-value, and local-reference lowering is executable. Remote,
suspending, and host-service coverage is still in progress.

`foster build --native` compiles the functions reachable from `main` into a native object and links
that object into a standalone executable with the installed Rust toolchain. `main` may take no
parameters or one `std.process.Arguments` value:

```powershell
foster build benchmarks/fibonacci.fos --native -o fibonacci.exe
./fibonacci.exe
```

Without `-o`, a source file produces a sibling executable with the source extension removed. A
directory package produces `main` (or `main.exe` on Windows) inside that directory. `--optimize`
is the default; `--no-optimize` disables both Cranelift and linker optimization.

Use `--emit native-ir` to print the deterministic, verified code-generation IR without linking:

```powershell
foster build benchmarks/fibonacci.fos --native --emit native-ir
```

## Implemented subset

The internal native ABI uses target-independent scalar representations: `Bool`, `Byte`, and `()`
use `i8`; `CodePoint` uses `i32`; `Int` uses `i64`; `Float` uses `f64`; and runtime-backed values
use the target pointer type. It supports:

- `()`, `Bool`, `Int`, binary64 `Float`, `CodePoint`, and `Byte` parameters and results;
- String and Symbol literals, equality, concatenation, Unicode sequence views, UTF-8 conversion,
  command-argument strings, and text results;
- the `executable` and `values` fields of `Arguments`, plus read-only `List<String>` indexing, `empty?`,
  `length`, and `head` operations;
- primitive constants, moves, unary operations, arithmetic, bit operations, shifts, and comparisons;
- direct function and statically resolved method calls;
- user-record construction and field reads, nested records, copy-on-write field assignment, and
  record values passed through borrowed function parameters;
- descriptor-backed generic list construction, indexing, copy-on-write indexed assignment,
  push/append, containment, and the `empty?`, `length`, `head`, and `rest` sequence views;
- descriptor-backed immutable bytes, including indexing and sequence views, plus compact
  list/UTF-8 bridges; byte algorithms and mutable `ByteBuffer` are implemented in Foster over
  those primitives;
- enum-case allocation, deterministic tags, aggregate payloads, and short-circuiting enum pattern
  tests/bindings;
- closed-world monomorphization of reachable generic functions, records, and tagged variants, with
  concrete signatures, layouts, destructors, and call targets cached per substitution;
- concrete closure construction and calls, capture-prefix ABIs, and specialized environment
  destructors; a uniform `(code thunk, environment, release thunk)` callable representation lets
  higher-order Foster functions accept independently shaped closure environments;
- owned erased boxes for union and other explicitly dynamic ABI boundaries, with scalar-or-pointer
  payloads and type-specific release thunks;
- whole, indexed, and field references lowered as typed addresses, including typed reference
  parameters, load/store, move-out, and mutation observed through a closure capture;
- descriptor-addressed allocation, strong retain/release, ownership transfer at calls and returns,
  and generated tag-aware recursive destructors;
- assertions, guarded returns, `loop`, guarded `break`/`continue`, jumps, conditional control
  flow, and recursion; and
- printing a result from `main` whose type is not `()`, matching `foster run` for these primitive
  values.

Only functions statically reachable from `main` are compiled. An unused function may therefore use
the complete VM language without preventing native compilation.

All declared runtime-backed categories have target-specific physical layouts, including bytes,
buffers, generic lists, places, callable handles, erased boxes, remote values, and futures. Core
list, string, byte, and byte-buffer algorithms using the supported primitives compile as ordinary
Foster functions. Remote execution, futures/suspension, host I/O, and string/symbol literal patterns
do not yet have complete native instruction lowering. If one is reachable, compilation stops
before object emission and reports the unsupported type or instruction. The diagnostic recommends
ordinary `foster build`, which emits portable `.fbc` for the complete language.

## Architecture

The frontend, type/effect/ownership checks, ownership MIR, layout legalization, and shared SSA
contract are common to the executable backends. HIR lowering temporarily constructs virtual
registers and jumps, seals them into typed basic blocks where instructions define immutable values,
and verifies definitions, dominance, types, call signatures, block arguments, and terminators.

The compiler exposes its first sealed, typed SSA graph as a reusable compilation artifact. Native
reachability and specialization consume that graph directly; they do not de-SSA it to bytecode or
reconstruct control flow from a register program. Copyable scalars remain SSA aliases,
ownership-bearing object copies become explicit retain operations, consuming calls transfer their
SSA value, and COW mutation is explicit before a field or index store. The bytecode backend remains
an independent de-SSA consumer of the same graph.

The portable, versioned bytecode remains the VM's execution and distribution format. The native
IR is a shared internal backend boundary rather than a replacement for bytecode, leaving room for
the VM and other native code generators without exposing Cranelift types to the frontend. Its
Foster scalar types map to Cranelift types only in the Cranelift emitter. All reachable functions
are declared before any is defined, allowing direct recursion and mutual recursion.

The shared boundary also has a VM de-SSA emitter. It assigns registers to immutable definitions,
splits conditional edges when their block arguments differ, and resolves parallel-copy cycles with
one temporary register. Its output is ordinary versioned bytecode and passes through the existing
ownership-aware bytecode verifier and VM.

The complete VM instruction surface is represented at this boundary: aggregates, mutation,
references, move-out, closures and capture modes, pattern bindings, dynamic calls, remote calls,
suspension, and destruction. HIR construction uses temporary virtual registers only until the
function is sealed into SSA; that unsealed form is never optimized, serialized, or executed.

Before backend-specific emission, logical layout legalization reduces values to scalars or pointers
and builds deterministic descriptions for record field slots and declared types, enum alternative
tags and payloads, closure environments and capture ownership, reference place handles, and
runtime-backed structural values. Portable bytecode version 21 retains generic identities, nominal
parameters and arguments, and sorted substitutions at statically resolved calls and closure
construction. Native
reachability is keyed by function plus substitutions; it materializes concrete signatures and
record/enum/closure and runtime-backed generic layouts before target-specific physical layout
calculation. Generic lists, callable signatures, remote/future handles, and places are cached by
their concrete verifier type. Explicit opaque slots remain only for values whose representation is
genuinely dynamic.

After target selection, the physical layout calculator derives checked sizes, alignments, byte
offsets, and ownership-aware drop plans. Heap objects have a common descriptor-pointer, strong
reference-count, and flags header. Exact target layouts exist for records, tagged variants,
closures, place handles with structural-generation snapshots, bytes, mutable buffers, lists,
remote/future handles, callable handles, and erased boxes. Callable handles carry uniform call and
release thunks around a concrete closure environment. Erased boxes carry one scalar-or-pointer
payload plus its release thunk. Recursive aggregate members remain pointer-sized, so layout
calculation terminates without flattening recursive types.

A place handle stores its root storage pointer plus a pointer/count projection path. Each path
entry has a fixed target-aware layout containing a field-slot or collection-index operand and the
root/prefix generation snapshots needed for indexed-reference invalidation. This supports nested
field/index projections without limiting a handle to one generation snapshot.

Native object files contain a versioned, read-only `foster_layout_<id>` descriptor for every
materialized physical layout. Generic schemas retain stable internal IDs but receive no descriptor
or destructor until instantiated. Descriptors include common-header offsets, kind-specific offsets, field value
representations and pointee identities, capture ownership, and destruction metadata. Record and
variant, and buffer lowering addresses these symbols directly, initializes the common header, emits typed
field/tag loads and stores, and follows the descriptor-derived drop plan. Copy-on-write is explicit
in shared IR; the current baseline copies record or buffer storage before mutation and can later add the
reference-count uniqueness fast path without changing semantics.

The object exports a C-ABI `foster_native_entry` symbol. A generated, temporary Rust entry shim
collects Unicode command arguments, supplies legacy argument/String views and typed platform
imports, calls
that symbol, supplies raw zeroed allocation/deallocation, formats its result, and supplies the
platform startup pieces to the system linker. Object semantics—layout, field access, reference
counts, copy-on-write, callable and erased-value ownership, and recursive destruction—are generated
Cranelift code rather than Rust runtime helpers. The authoritative intrinsic registry also declares
each builtin's native policy: Foster replacement, inline scalar/representation primitive, typed
runtime import, or unavailable. Native member helper selection for the legacy process/String ABI is
registry-owned rather than an ad hoc backend switch.
Temporary object and shim files are removed after linking; the resulting executable does not
contain or invoke the Foster VM.

Checked integer addition, subtraction, multiplication, invalid shifts, and integer division errors
currently become native machine traps. A future native runtime ABI should turn those traps into the
same friendly runtime diagnostics produced by the VM.
