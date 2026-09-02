# Native compilation

Status: host-native AOT backend implemented with Cranelift; scalar, record, and tagged-variant
lowering is executable, while collection, reference, closure, and host-service coverage is still
in progress.

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
- String constants, equality, command-argument strings, and String results;
- the `executable` and `values` fields of `Arguments`, plus read-only `List<String>` indexing, `empty?`,
  `length`, and `head` operations;
- primitive constants, moves, unary operations, arithmetic, bit operations, shifts, and comparisons;
- direct function and statically resolved method calls;
- user-record construction and field reads, nested records, copy-on-write field assignment, and
  record values passed through borrowed function parameters;
- enum-case allocation, deterministic tags, scalar payloads, and enum pattern tests/bindings;
- descriptor-addressed allocation, strong retain/release, ownership transfer at calls and returns,
  and generated tag-aware recursive destructors;
- assertions, guarded returns, `loop`, guarded `break`/`continue`, jumps, conditional control
  flow, and recursion; and
- printing a result from `main` whose type is not `()`, matching `foster run` for these primitive
  values.

Only functions statically reachable from `main` are compiled. An unused function may therefore use
the complete VM language without preventing native compilation.

General lists and buffers, String concatenation and library algorithms, symbols, references,
closures, dynamic calls, intrinsics, aggregate payload bindings that require erased generic
representation, remote objects, futures, and host I/O do not yet have a complete native lowering.
If one is reachable, compilation stops
before object emission and reports the unsupported type or instruction. The diagnostic recommends
ordinary `foster build`, which emits portable `.fbc` for the complete language.

## Architecture

The frontend, type/effect/ownership checks, ownership MIR, layout legalization, and shared SSA
contract are common to the executable backends. HIR lowering temporarily constructs virtual
registers and jumps, seals them into typed basic blocks where instructions define immutable values,
and verifies definitions, dominance, types, call signatures, block arguments, and terminators.

The current bootstrap native path asks the VM backend for verified, non-optimized bytecode so each
storage home retains a stable static type, then rebuilds the reachable native subset as the same
shared SSA shape. Copyable scalars remain SSA aliases; ownership-bearing object moves become
explicit retain operations, and ordinary values are emitted as Cranelift SSA values rather than
accesses to a VM register array. This second
sealing step is an implementation detail while native coverage grows; it does not make register
bytecode the native code-generation contract.

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
runtime-backed structural values. Portable bytecode version 18 retains generic identities and
nominal arguments. Generic fields currently use an explicit opaque pointer slot, so their physical
ABI remains concrete without prematurely coupling portable bytecode to monomorphization.

After target selection, the physical layout calculator derives checked sizes, alignments, byte
offsets, and ownership-aware drop plans. Heap objects have a common descriptor-pointer, strong
reference-count, and flags header. Exact target layouts exist for records, tagged variants,
closures, place handles with structural-generation snapshots, bytes, mutable buffers, lists,
remote/future handles, callable handles, and erased boxes. Recursive aggregate members remain
pointer-sized, so layout calculation terminates without flattening recursive types.

A place handle stores its root storage pointer plus a pointer/count projection path. Each path
entry has a fixed target-aware layout containing a field-slot or collection-index operand and the
root/prefix generation snapshots needed for indexed-reference invalidation. This supports nested
field/index projections without limiting a handle to one generation snapshot.

Native object files contain a versioned, read-only `foster_layout_<id>` descriptor for every
physical layout. Descriptors include the common-header offsets, kind-specific offsets, field value
representations and pointee identities, capture ownership, and destruction metadata. Record and
variant lowering addresses these symbols directly, initializes the common header, emits typed
field/tag loads and stores, and follows the descriptor-derived drop plan. Copy-on-write is explicit
in shared IR; the current baseline copies record storage before mutation and can later add the
reference-count uniqueness fast path without changing semantics.

The object exports a C-ABI `foster_native_entry` symbol. A generated, temporary Rust entry shim
collects Unicode command arguments, supplies the supported String/List runtime operations, calls
that symbol, supplies raw zeroed allocation/deallocation, formats its result, and supplies the
platform startup pieces to the system linker. Object semantics—layout, field access, reference
counts, copy-on-write, and recursive destruction—are generated Cranelift code rather than Rust
runtime helpers.
Temporary object and shim files are removed after linking; the resulting executable does not
contain or invoke the Foster VM.

Checked integer addition, subtraction, multiplication, invalid shifts, and integer division errors
currently become native machine traps. A future native runtime ABI should turn those traps into the
same friendly runtime diagnostics produced by the VM.
