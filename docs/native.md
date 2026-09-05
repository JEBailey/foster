# Native compilation

Status: host-native AOT backend implemented with Cranelift; scalar, aggregate, concrete and erased
callable, list, string/bytes, erased-value, local-reference, closed-world structural-contract,
local remote-actor, and blocking-future lowering is executable. Filesystem, path, environment,
clock, entropy, and TCP host services are executable. Resumable suspension/state-machine lowering
remains in progress.

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
- direct function and statically resolved method calls, plus descriptor-dispatched structural
  contract calls with arguments, multiple reachable implementations, and generic specialization;
- user-record construction and field reads, nested records, copy-on-write field assignment, and
  record values passed through borrowed function parameters;
- descriptor-backed generic list construction, indexing, copy-on-write indexed assignment,
  push/append, containment, and the `empty?`, `length`, `head`, and `rest` sequence views;
- descriptor-backed immutable bytes, including indexing and sequence views, plus compact
  list/UTF-8 bridges; byte algorithms and mutable `ByteBuffer` are implemented in Foster over
  those primitives;
- enum-case allocation, deterministic tags, aggregate payloads, and short-circuiting enum pattern
  tests/bindings, plus String and Symbol literal patterns;
- structural record, enum-payload, list, and byte equality using initialized descriptor fields,
  rather than object addresses or allocation padding;
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
  flow, and recursion;
- filesystem byte/text access and mutation, path inspection and canonicalization, the process
  working directory, wall and monotonic clocks, and operating-system entropy through the versioned
  host ABI;
- handle-based TCP listen, connect, accept, byte/text reads and writes, timeouts, and explicit
  listener/connection close operations;
- owned and borrowed local remote actors, FIFO worker dispatch, specialized generic remote
  methods, ownership-correct scalar or aggregate messages/results, futures, and blocking `await`;
- `print` and `println` over scalar and descriptor-backed values; and
- printing scalar or aggregate results from `main`, followed by ownership-correct release of a
  managed aggregate result.

Only functions statically reachable from `main` are compiled. An unused function may therefore use
the complete VM language without preventing native compilation.

All declared runtime-backed categories have target-specific physical layouts, including bytes,
buffers, generic lists, places, callable handles, erased boxes, remote values, and futures. Core
list, string, byte, and byte-buffer algorithms using the supported primitives compile as ordinary
Foster functions. Platform-dependent filesystem, path, clock, entropy, TCP, actor-worker, and
future primitives lower through the stable native host ABI while their higher-level behavior
remains Foster code. Native remote objects are currently in-process actors backed by one operating
system thread each. Calls are FIFO; borrowed actor state and borrowed managed message arguments
force dispatch to complete before the caller resumes. `await` blocks its current thread and
consumes its future exactly once. Turning suspension points into resumable state machines remains
unsupported; the portable VM remains the complete execution path for that scheduling model.

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

`native::prepare` produces an immutable `NativeProgram`: reachability, specialization, layout
calculation, native SSA lowering, and verification happen once. Its `emit_ir`, `compile_object`,
and `build_executable` methods reuse those exact functions and layouts, including across optimization
modes. The convenience functions with the same names prepare a fresh program for a single request.
Each prepared function retains its specialized logical signature and parameter ownership modes,
compact verified logical alternatives by source storage home, and a per-value memory-management
classification. This preserves distinctions such as String versus Symbol after both become pointers.
ABI-only temporaries have no source logical identity; storage-home alternatives are not a
path-sensitive type assertion for each SSA value. Mutable parameter homes and storage types are
also calculated during preparation rather than reconstructed during emission.

The backend separates program preparation (`native/program.rs`), object assembly
(`native/emission.rs`), allocation and retain/release/destructor policy (`native/ownership.rs`),
and runtime-shim generation and linking (`native/runtime.rs`). Ownership-bearing SSA instructions
remain explicit; the management classification describes representation policy, not ownership of
every SSA alias. Legacy runtime pointers are explicitly classified as unmanaged by the aggregate
retain/release protocol. Helper definitions are emitted in stable key order, so hash-map iteration
does not change repeated object emission.

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

Function signatures preserve named generics for bytecode verification. Direct and method calls
substitute their concrete arguments before checking operands and results; remote calls infer
substitutions from verified receiver and argument types. Native remote-call selection retains
those logical types instead of reconstructing them from scalar/pointer representations, where
String and Symbol share an ABI.

`tests/backend_parity.rs` runs shared value, failure, ownership, and remote-call cases through the
VM and native executable with optimization both enabled and disabled. CI runs this suite in debug
and release Rust profiles on Windows and Linux. The broader Foster library
suite still runs on the VM; checking that a module declares tests is not native execution coverage.

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
or destructor until instantiated. Descriptor version 2 includes common-header offsets,
kind-specific offsets, record/variant/field names, scalar semantic tags, pointee identities,
mutability, capture ownership, and destruction metadata. Record, variant, buffer, generic
formatting, and structural-contract dispatch address these symbols directly. Lowering initializes
the common header, emits typed field/tag loads and stores, and follows the descriptor-derived drop
plan. Copy-on-write is explicit in shared IR: unique record and buffer storage is reused, while
shared storage detaches before mutation. Native buffers reuse capacity and grow geometrically with
checked size arithmetic. Projected mutable fields use typed borrowed addresses; detaching their
contents updates the owning field rather than treating its address as an object.

The object exports a C-ABI `foster_native_entry` symbol and, when needed, a result-release thunk. A
generated, temporary Rust entry shim collects Unicode command arguments, supplies legacy
argument/String views and typed platform imports, calls that symbol, supplies raw zeroed
allocation/deallocation, formats its result, and supplies the platform startup, operating-system,
actor-worker, and future services to the system linker. Filesystem paths resolve relative to the
captured startup directory; TCP listeners and streams live behind typed integer handles owned by
the shim. Remote method callbacks enter specialized generated Cranelift thunks directly; the shim
only schedules their messages and completion values. Object semantics—layout, field access,
reference counts, copy-on-write, callable and erased-value ownership, and recursive destruction—
are generated Cranelift code. Structural equality uses a shared runtime helper that reads the same
descriptors and recursively compares initialized values, including IEEE floating-point equality.
The authoritative intrinsic registry also declares each builtin's native policy:
Foster replacement, inline scalar/representation primitive, typed runtime import, or unavailable.
Native member helper selection for the legacy process/String ABI is registry-owned rather than an
ad hoc backend switch.
Temporary object and shim files are removed after linking; the resulting executable does not
contain or invoke the Foster VM.

The platform boundary is a stable, explicitly versioned C ABI. Imported symbols use the
`foster_rt_v1_*` namespace, so an incompatible runtime fails at link time. Checked integer
arithmetic, invalid shifts and conversions, division errors, and bounds failures call that ABI and
produce friendly diagnostics rather than machine traps.

`native/abi.rs` is the authoritative registry of runtime wire signatures and ownership contracts.
Runtime imports are checked against it and Cranelift signatures are built from its wire types.
Every linked Rust shim also contains compile-time function-pointer checks for all registered
exports, catching drift on either side of the ABI. Payloads governed by callbacks and legacy text
allocations are identified separately rather than claimed to have unconditional managed ownership.
These checks establish signature compatibility; they do not by themselves prove runtime ownership
behavior or eliminate the correctness gaps below.

## Known runtime correctness gaps

Legacy runtime-created String values do not yet participate in aggregate retain/release, so
string-heavy native programs can retain allocations until process exit. Native failures inside
remote methods also remain process-fatal rather than being stored on the returned future; even an
unawaited failing remote call can terminate the process. These are unresolved ownership and
failure-propagation ABI issues, not guarantees established by the current parity suite.
