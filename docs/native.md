# Native compilation

The host-native AOT backend uses Cranelift; scalar, aggregate, concrete and erased
callable, list, string/bytes, erased-value, local-reference, closed-world structural-contract,
local remote-actor, and stackful virtual-thread execution is supported. Filesystem, path, environment,
clock, entropy, and TCP host services are executable. Actors use the same May coroutine scheduler
as the VM.

`foster build --native` compiles the functions reachable from `main` into a native object and links
that object into a standalone executable with the installed Rust toolchain. `main` may take no
parameters or one `std.process.Arguments` value:

```powershell
foster build benchmarks/fibonacci.fos --native -o fibonacci.exe
./fibonacci.exe
```

Without `-o`, a source file produces a sibling executable with the source extension removed. A
directory package produces `main` (or `main.exe` on Windows) inside that directory. `--optimize`
is the default; `--no-optimize` disables shared SSA, Cranelift, and linker optimization.

The shared Rust runtime is compiled once per runtime source, Rust toolchain/host, and optimization
mode, then reused across native builds and compiler processes. Each executable still compiles its
own Foster object and a small startup shim containing its string constants and entry signature.
The runtime is linked statically, so the resulting executable does not need the cache to run.

The first runtime build uses Cargo with an embedded manifest and lockfile to build May and its
dependencies. These must be available in the Cargo cache or downloadable. May and its dependencies
are linked statically into the executable. Subsequent builds reuse the cached runtime and link
artifacts; deployed executables need neither Cargo nor the cache.

Stable runtime code lives in the ordinary Rust `runtime` workspace crate. The compiler embeds
those source files for distribution and appends ABI checks when building the cached archive.
Both backends share the `host` crate for host contexts, path resolution, clocks, and TCP services;
the compiler does not link the program-facing native ABI runtime. Workspace Cargo checks,
Clippy, and runtime unit tests cover the extracted source directly.


By default, runtime archives live in `%LOCALAPPDATA%/foster/native-runtime` on Windows and
`$XDG_CACHE_HOME/foster/native-runtime` (or `$HOME/.cache/foster/native-runtime`) elsewhere.
Set `FOSTER_NATIVE_CACHE_DIR` to select another directory, for example a CI cache or
`target/native-runtime-cache`. Removing that directory forces a rebuild on the next native build.
Concurrent builds share a lock for each runtime version; failed builds are retried rather than
published as usable cache entries. Allocation-instrumented runtime tests build their own runtime
and do not use this production cache.

Use `--emit native-ir` to print the deterministic, verified code-generation IR without linking:

```powershell
foster build benchmarks/fibonacci.fos --native --emit native-ir
```

## Shared executable type conversion

`codegen::types::{ExecutableType, Specialization}` defines the logical schemas shared by
specialization, SSA metadata, layout selection, and bytecode verification. Semantic `types::Type`
describes source requirements; `ExecutableType` retains executable shape and ownership modes;
legalized SSA types and physical layouts select representation, sizes, alignment, and offsets.
The shared definitions and substitution helpers have no VM dependency. Layout construction
consumes the neutral `ProgramMetadata` shared by both backends.

`Unknown` means erased or unknown shape: it is the verifier's top type and selects an opaque
native representation, without proving source conformance. `Generic` names can remain during
intermediate specialization but must be resolved or explicitly handled for materialized layouts.
Specializations contain sorted, unique names; their values can still contain generic leaves.
Nominal IDs belong to the current compilation or relocated program rather than a global namespace.

`src/codegen/type_conversion.rs` owns the recursive conversion from semantic types to
`ExecutableType`, which both bytecode verification and native layout selection consume.
The `Bytecode` and `Native` policies explicitly preserve these backend differences:

| Decision | Bytecode | Native |
| --- | --- | --- |
| Intersections | Erase to `Unknown` | Retain unordered requirements in `Intersection` |
| Alias argument metadata | Erase to `Unknown` | Retain alias identity and positional arguments in `AliasArguments` |
| Record erasure | Erase fieldless records without copy/deinit capabilities | Erase nested structural dispatch contracts; preserve root nominal identity |
| Empty remote receiver | Preserve nominal identity for dispatch | Apply ordinary nested conversion |
| Nesting at depth 64 | Return `Unknown` | Report the native specialization nesting error |
| Builtin list arguments | Use the first argument, or `Unknown` if absent | Recognize the builtin only with exactly one argument |

`Alternatives` describes control-flow possibilities and uses any-member matching for expected
types. Native `Intersection` and `AliasArguments` retain metadata behind opaque representations
and never use that matching rule. Intersections are canonicalized as sets; alias arguments retain
their order and duplicates. Both native-only forms are rejected by bytecode validation and encoding.
Alternative and intersection payloads are distinct private containers with read-only member access.
Canonical constructors create them; a checked decoder constructor rejects malformed alternatives.
Native record classification
preserves private storage and distinguishes concrete implementations from inherited defaults.
Both policies erase sequence views and modules, and share scalar, container, callable, nominal,
and generic conversion. Native specialization supplies executable generic substitutions;
bytecode conversion retains generic names. Already-converted substitutions are copied as leaves.

These policies document existing behavior rather than requiring identical runtime layouts.
The shared converter's tests cover agreement, intentional differences, and the depth boundary.

## Supported subset

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
  higher-order Foster functions accept and return independently shaped closure environments;
  forwarded callables, nested factories, and runtime-selected closure results use this same ABI;
- owned erased boxes for dynamic contract ABI boundaries, with scalar-or-pointer
  payloads and type-specific release thunks;
- receiver-dependent pop results for `Queue`, `Deque`, and `Stack`, including owned payloads
  and concrete remainders viewed through the corresponding structural contract. Native dispatch
  rebuilds matching record/enum containers when their nested contract representation changes;
  this conversion is bounded and excludes containers with custom destructors or borrowed fields;
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
  methods, ownership-correct scalar or aggregate messages/results, futures, and suspending `await`;
- `print` and `println` over scalar and descriptor-backed values; and
- printing scalar or aggregate results from `main`, followed by ownership-correct release of a
  managed aggregate result.

Generic `Sequence<T>` values retain erased storage across function calls and stored fields. Native
contract dispatch selects `empty?`, `length`, `head`, and `rest` from the concrete descriptor,
including built-in lists, strings and bytes and user-defined implementations. Contract result
types survive specialization, so iterators with different element types can coexist in one
program. A custom `rest` implementation may return another conforming representation.
Collection `.iterator()` calls use list/byte indices and a UTF-8 byte offset for strings in both
backends. Each cursor retains one stable snapshot; advancing does not construct suffix collections.
`Iterator.from_sequence` and `.iterator()` on an erased `Sequence<T>` retain the generic head/rest
adapter, whose cost depends on the source's `rest` implementation. Lazy pipelines use either kind
of cursor without changing their API. This does not introduce borrowed views or consuming iteration.

Only functions statically reachable from `main` are compiled. An unused function may therefore use
the complete VM language without preventing native compilation.

All declared runtime-backed categories have target-specific physical layouts, including bytes,
buffers, generic lists, places, callable handles, erased boxes, remote values, and futures. Core
list, string, byte, and byte-buffer algorithms using the supported primitives compile as ordinary
Foster functions. Platform-dependent filesystem, path, clock, entropy, TCP, actor-worker, and
future primitives lower through the stable native host ABI while their higher-level behavior
remains Foster code. Native remote objects are in-process actors running on May stackful virtual
threads, as in the VM. Many actors share scheduler OS threads; FIFO mailbox receives, borrowed dispatch waits,
and `await` suspend the current coroutine while preserving its native call stack. Futures are
still consumed exactly once. Generated basic-block cancellation checks yield periodically so
CPU-bound actors share workers. Failure, cancellation, and cleanup state is coroutine-local.
Generated poll branches reuse the cancellation helper's returned failure status, avoiding
a second failure query while preserving the same live-value cleanup path.
Post-call failure checks only observe failures already raised by the call. Cancellation
is discovered at basic-block polls and while waiting for futures, where live values
have cleanup coverage; a successful owned return cannot be discarded by a racing
post-call cancellation check.
The process entry function runs on the main OS thread and can await actors there.

Filesystem and TCP operations use the shared blocking host API. Calls from actors temporarily
run on a host OS thread while the actor suspends; this avoids pinning a scheduler worker, but
large numbers of concurrent blocking host operations can still create many OS threads. Host
work owns copied inputs and completes before its actor resumes, including during cancellation.
Native actor scheduling does not require compiler-generated resumable state machines.

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

SSA instructions carry their source spans, and captures carry their value/type pairs. Temporary
construction buffers enter through length-checked adapters. `SharedProgram` exposes immutable
access to its functions, metadata, and derived signatures; sealing validates both the construction
program and SSA. Backend transforms consume this boundary and validate their resulting IR.
Retaining shared SSA and lowering it to VM bytecode use one sealing routine, including construction
validation and whole-graph signature checks. VM lowering commits new bodies only after every
function succeeds; errors preserve the original bodies and constant pool.
SSA value allocation uses `ir::ValueBuilder`, which assigns stable IDs and stores each type with
its optional construction storage home. Finished functions expose an immutable `ValueTable`;
native specialization resumes a builder to retype existing IDs and allocate temporaries.
Definition uniqueness, parameter/edge compatibility, and CFG relationships remain verifier-checked.

`codegen::metadata::ProgramMetadata` owns constants, symbolic identities, nominal record/variant
schemas, dispatch targets, core-type identities, and entry-point conventions. Logical record field
lookup also lives there; it has no VM values or physical offsets. `vm::Program` composes this data
with bytecode functions and the drop-insertion flag. The binary field order and format are unchanged.
`SharedProgram::metadata()` and `NativeProgram::metadata()` expose only the neutral data.
Nominal layout construction and native representation/ownership helpers accept neutral metadata.
Shared sealing discards construction bodies. Neutral declarations retain callable metadata, and
SSA bodies are authoritative for literals, layout discovery, reachability, and both backends.
Native logical flow and physical representation inference both consume the sealed SSA graph.
Record schemas store paired `RecordField` entries. Construction derives their immutable lookup
layout and rejects duplicate names; linking remaps field types without changing storage order.


`src/codegen/construction.rs` lowers HIR into the shared slot IR in `codegen::storage`.
Shared lifetime analysis and validation live under `src/codegen/storage/`; `codegen::sealing`
builds the authoritative SSA graph. These production stages do not depend on the VM backend.
`src/codegen/vm/emission.rs` assigns VM registers and emits edge copies;
`src/codegen/vm/instructions.rs` maps SSA operations to slot instructions for serialization
and execution. The VM reuses the shared slot model and verifier. Native lowering consumes SSA
directly. Differential evidence checks compare slot-flow and SSA-flow facts at sealing.

`codegen::flow` owns logical flow analysis. A sealed program retains paired logical function
schemas, immutable per-function facts, and the exact SSA graphs those facts describe. A query at
a block/instruction site distinguishes unreachable code, an unavailable definition, and a known
logical type (which may itself be `Unknown`). Representation types never reconstruct this evidence.

The flow engine entry module owns the worklist. Its `state` module owns availability and state
joins; `types` owns logical type rules; `transfer` and `calls` own operation and call transfer;
`operations` defines the existing logical operation vocabulary. Together they track pattern-binding
availability and excluded variant facts. The VM adapter analyzes consuming slots and still validates serialized bytecode.
The SSA adapter analyzes immutable identities, transfers block arguments in parallel, and remaps
pattern evidence on edges. Sealing preserves prior SSA bindings for writes that can assign through
a reference, including bindings that ordinary value liveness would discard. Consuming an owner does not erase the type of its immutable SSA aliases;
ownership permission remains separately checked. Sealing tests compare reachable use-site evidence
against the VM slot adapter; the source correspondence exists only in test builds.

Native contract arguments and remote receivers/arguments query these SSA sites. Remote call targets
are keyed by SSA result identity, and logical summaries no longer combine unrelated values that
reuse a storage home. Native physical representation inference propagates layouts from SSA
definitions through block arguments to a fixed point, including loop backedges. It does not read
construction registers or storage hints. Logical flow proves source-language facts; native inference
selects physical layouts and callable ABIs. They remain distinct analyses over the same graph.
Different closure environments join at a uniform callable ABI, and erased/concrete values join
at an erased ABI. Native edge legalization inserts conversions before parallel argument assignment
and releases the old owners on the affected edge. Storage hints remain ownership locations during
legalization, rather than identities for type inference.
`src/native/inference.rs` owns the fixed-point solver; its `instructions` and `representations`
modules contain result-layout rules and layout/ABI helpers. Empty storage tokens receive layouts
from their receiving blocks or callee ABIs, without constraining live producers backwards.

The portable, versioned bytecode remains the VM's execution and distribution format. The native
IR is a shared internal backend boundary rather than a replacement for bytecode, leaving room for
the VM and other native code generators without exposing Cranelift types to the frontend. Its
Foster scalar types map to Cranelift types only in the Cranelift emitter. All reachable functions
are declared before any is defined, allowing direct recursion and mutual recursion.

`SharedProgram::optimized()` is the common semantic pipeline: bounded scalar leaf inlining,
constant propagation, branch pruning, unreachable-block removal, and dead scalar elimination.
Shared scalar CSE then reuses dominating expressions with identical SSA operands. Its initial
whitelist covers scalar constants, integer/byte/code-point comparisons, Boolean equality and
negation, and byte bitwise operations. Checked arithmetic, shifts, floating point, memory reads,
calls, and ownership-sensitive values are excluded. Reused results lose unobservable storage
hints so later writes cannot overwrite them during VM lowering. Cleanup runs again only after
CSE changes a graph; profiling records the pass as `shared.cse`.
Transforms invalidate graph-specific evidence, verify the resulting SSA, and rebuild logical flow
for affected functions. Native specialization, representation inference, and cleanup planning run
only after this boundary; no semantic transform runs over already planned cleanup sites.

`native::prepare` produces an immutable baseline `NativeProgram`. Rendering remains stable.
Requesting another optimization mode prepares and caches a separate variant from the retained
canonical SSA, including new specialization and cleanup evidence. Repeated object emission reuses
that variant. `native::prepare_with_options` and the convenience compilation/build functions
prepare the requested mode directly.

Baseline preparation shares the canonical SSA graph instead of cloning the complete shared
program. Optimized preparation copies the graph on mutation and shares immutable flow facts for
unchanged functions; changed functions receive fresh analysis evidence. Native preparation copies
the layout registry because specialization adds layouts. The retained canonical graph still allows
the other optimization mode to be prepared without reconstructing bytecode or repeating the frontend.
Cranelift performs machine-specific optimization after native representation and ABI lowering.
Each prepared function retains its specialized logical signature and parameter ownership modes,
compact verified logical alternatives by original SSA value, and a per-value memory-management
classification. This preserves distinctions such as String versus Symbol after both become pointers.
ABI-only temporaries have no source logical identity. Value summaries aggregate that identity
across paths; specialization queries the separate use-site facts. Mutable parameter homes and storage types are
also calculated during preparation rather than reconstructed during emission.

The backend separates program preparation (`native/program.rs`), object assembly
(`native/emission.rs`), allocation and retain/release/destructor policy (`native/ownership.rs`),
runtime-shim generation and linking (`native/runtime.rs`), and shared runtime caching
(`native/runtime_cache.rs`). Ownership-bearing SSA instructions
remain explicit; the management classification describes representation policy, not ownership of
every SSA alias. Raw host pointers are explicitly classified as unmanaged by the aggregate
retain/release protocol. Helper definitions are emitted in stable key order, so hash-map iteration
does not change repeated object emission.

The native entry module contains the public API and shared compilation contexts. Preparation
uses `specialization.rs` for reachability and shared flow-fact queries, `representation.rs` and
`inference.rs` for type conversion, `validation.rs` for native restrictions, and `legalize.rs`
for converting shared IR to native IR. Legalization is split into `legalize/function.rs` for
function preparation and cleanup, `legalize/instructions.rs` for instruction lowering, and
`legalize/conversions.rs` for return, argument, capture, callable, and erased-value conversions.
Machine emission uses `lowering.rs` for blocks and
patterns, `portable.rs` for portable instructions, `operations.rs` for arithmetic and control
flow, and `machine.rs` for signatures and scalar storage. Host calls, remote calls, buffers,
record fields, runtime handles, and callable thunks each have a separate module. These modules
share the immutable preparation/backend contexts; helper visibility stays within the backend.

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
runtime-backed structural values. Portable bytecode retains generic identities, nominal
parameters and arguments, and sorted substitutions at statically resolved calls and closure
construction. Native
reachability is keyed by function plus substitutions. Logical SSA facts are calculated once per
verified graph revision and reused by all specializations and layout/lowering passes. Built-in representation selection uses canonical record IDs.
Native preparation materializes concrete signatures and
record/enum/closure and runtime-backed generic layouts before target-specific physical layout
calculation. Generic lists, callable signatures, remote/future handles, and places are cached by
their concrete verifier type. Explicit opaque slots remain only for values whose representation is
genuinely dynamic.

`codegen::types::Specialization` enforces sorted, unique substitution names at construction.
Compiler entry lists are canonicalized, serialized lists are checked without repair, and renaming
restores order while rejecting collisions atomically. Executable callable types and native logical
signatures use `types::Parameter<ExecutableType>` so ownership stays attached during specialization.

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
generated, temporary Rust entry shim collects Unicode command arguments, imports them into managed
Foster `Arguments`, `List<String>`, and `String` storage, supplies typed platform imports,
calls that symbol, supplies raw zeroed
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
Strings use the ordinary String record descriptor and owned Bytes storage. Generated boundary
adapters allocate and access this storage; Rust host responses copy text into it before releasing
their temporary buffers. Static literals are borrowed UTF-8 source data, not Rust String objects.
Argument field access and indexing use ordinary record/list instructions. `.bytes` retains the
underlying immutable Bytes, and `String.from_utf8(move bytes)` validates and constructs a String
in Foster without a decoding intrinsic or native storage copy. Native text property helpers remain
registry-owned; Unicode classification and numeric text conversion still use runtime primitives.
Branch-edge cleanup releases owned values omitted from successor SSA arguments. Generated
retain/release operations use atomic reference counts, including text shared with remote workers.
Move-out and consumed-call homes carry empty values into later blocks, so cleanup cannot reclaim
their former owners twice. Writable move-out projections detach shared parents before selecting
child addresses; retaining a partially moved aggregate preserves its null slots.
Preparation also retains the live ownership set at each native instruction. Modeled failure paths
release that set before returning to the caller: transferred arguments belong to the callee, borrowed
addresses do not own their pointees, and ABI argument copies remain owned until transferred.
Cleanup uses the same generated release functions and recursive layout destructors as normal exits.
Unused managed ABI parameters are released at entry, even when SSA removes their storage homes.
Host response buffers and private collection copies created inside an instruction are released
before its frame cleanup.
Temporary object and shim files are removed after linking; the resulting executable does not
contain or invoke the Foster VM.

The platform boundary is a stable, explicitly versioned C ABI. Imported symbols use the
`foster_rt_v4_*` namespace, so an incompatible runtime fails at link time. Checked integer
arithmetic, invalid shifts and conversions, division errors, and bounds failures call that ABI and
produce friendly diagnostics rather than machine traps.

`native/abi.rs` is the authoritative registry of runtime wire signatures and ownership contracts.
Runtime imports are checked against it and Cranelift signatures are built from its wire types.
Every linked Rust shim also contains compile-time function-pointer checks for all registered
exports, catching drift on either side of the ABI. Text-producing helpers return managed owned
values; callback-governed payloads retain their explicit transfer contracts.
These checks establish signature compatibility; they do not by themselves prove runtime ownership
behavior; the cleanup guarantees and limits are described below.

## Runtime cleanup guarantees and limits

Filesystem, clocks, TCP, and timed socket readiness share the injectable host-provider boundary
with the VM. See [host providers](host-providers.md) for embedding setup, Foster provider contracts,
and the current limits of per-operation readiness offloading.

The [remote lifecycle contract](remote-semantics.md) implements scoped cancellation and conservative
request-lifetime checks. Native workers contain language execution failures
and deliver `Result<T, RemoteError>` through futures. Failure is terminal for the worker, including
when the failing future is discarded. Queued and later calls receive the original failure without
invoking their methods; rejected messages release transferred arguments.

Generated calls check a coroutine-local failure flag before using their results and release live
managed values as they return through generated frames. Main-thread failures follow the same
cleanup path before the entry shim reports a diagnostic and exits. This avoids unwinding through
Cranelift frames or treating placeholder return values as owned results. Allocation-census tests
exercise successful exits and failures in both optimization modes, checking for leaks and duplicate
deallocation, including opaque host response buffers.

`deinit(self) -> ()` runs once at ownership end before child values are
released. Compiler-owned Copy/Drop dispatch slots select concrete implementations, and generated
layout destructors invoke the callback while the receiver is intact. A header flag transfers the
cleanup obligation across internal copy-on-write updates and prevents recursive invocation.
Cleanup saves and restores an existing language failure while running subsequent callbacks.
Host wrappers close external resources automatically only when they implement `deinit`; scoped
remote cancellation follows owner lifetimes, while process shutdown ordering remains open. Host-fatal events
such as allocation failure and invalid compiler/runtime metadata are
outside modeled language failure cleanup.

Locals passed by reference retain addressable storage across control-flow edges. Native
instruction operands and branch arguments reload that storage after possible alias mutations,
including replacement of aggregate storage during copy-on-write. Nested indexed writes preserve
their projected destination and detach shared ancestors before taking child addresses.
Loaded reference parameters remain borrowed during cleanup. Failure cleanup reloads address-taken
owners after a failing call, releasing replacement storage rather than its pre-call pointer.

SSA sealing also preserves the drop planner's protection of weak-reference origins across joins,
while explicit ownership drops still clear their storage. Copying a reference into a value-typed
branch result loads the pointee before releasing its origin.

Native value-returning functions read indexed references before releasing local origins. For
example, `let selected = ref items[0].left[0]` followed by `selected` as an `Int` function's
result returns the selected integer directly. Both explicit and implicit returns support this
conversion. Ownership checking still rejects escaping borrows of local managed storage (`E0402`).
