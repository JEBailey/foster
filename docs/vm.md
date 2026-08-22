# Foster register VM

Status: typed-HIR lowering, optimizing register IR, verifier, and execution core implemented.

Foster uses a custom register VM as its executable semantic reference. The pipeline is:

```text
source -> AST -> resolved HIR -> type/effect/loan/ownership checks
       -> ownership MIR validation -> structured register bytecode
       -> optional optimizer -> liveness-driven drops -> verifier -> machine
```

The structured instruction enum is both the optimizer-facing IR and executable form while the
language evolves. The explicit optimizer pipeline performs typed constant and branch folding,
control-flow cleanup, CFG-aware copy propagation, liveness-based dead-write elimination and
register reuse, and constant-pool deduplication. Rewrites preserve the parallel instruction
source-span table. Capture/parameter frame prefixes and reference origins are pinned where identity
is observable. The structured program has a deterministic, versioned
[compiled bytecode format](binary-format.md) for caching and distribution. Bytecode is verified
before execution and again after deserialization.

Frames store ordinary register values inline. A register is promoted to a stable reference-counted
slot only when it is borrowed, captured by reference, shared remotely, or used as an observable
method receiver. Consuming call parameters transfer values out of caller registers; read-only
borrowed parameters observe a promoted caller slot but detach if the parameter local is reassigned.

Record instances use dense indexed value arrays. Field names and their index table live once in a
shared record layout, including fields contributed by composed contracts. Variants similarly share
their type and alternative names through program metadata rather than allocating those strings per
value. Wire conversion restores names only at the remote serialization boundary.

After all optional representational rewrites, the compiler inserts explicit `Drop` instructions
at register last-use points. A drop clears an inline value or detaches a promoted register from its
slot, allowing ordinary acyclic values to be reclaimed immediately. It does not write through an
observable slot: reference captures,
projected-reference parameters, and method receivers can share slot identity and are protected for
as long as that identity remains observable. Conditional branches receive cleanup on both outgoing
edges when their condition dies at the branch. Frame teardown remains the final cleanup boundary
for protected slots and returned values.

The interprocedural tier inlines small straight-line leaf functions. Inlined parameters receive
fresh virtual registers before copy propagation, so assigning to a parameter cannot mutate the
caller's argument slot. Escape analysis recognizes single-use closure values when delaying capture
is safe and replaces the allocation plus dynamic dispatch pair with `CallClosure`. Reference
captures preserve slot identity; move captures are specialized only for immediate invocation.
Projected references contain one weak root plus a field/index path. Extending a projection through
an intermediate reference wrapper flattens onto that root, so nested method receivers do not need a
strong keep-alive edge or form an `Rc` cycle.

The current instruction set covers constants, moves, typed unary and binary operations, direct and
method calls, guarded returns, conditional and subject-based pattern branches, jumps, lists and
indexing, records and field mutation, variant construction, atomic pattern bindings, closure
environments, dynamic and specialized non-escaping closure calls, partial application, projected
references, structural mutation, remote objects, futures, await, and returns.
Lowering rejects unsupported HIR explicitly; it never interprets an unsupported node as a fallback.

Closure frames use a fixed `[captures][parameters][locals/temporaries]` register layout. Copy and
move captures contain values; reference captures contain weak `PlaceHandle`s that are materialized
as forwarding slots in the called frame. Reference wrappers are flattened when captured so an
escaping closure points directly to the original place. Named recursion resolves directly to a
function ID. Calls execute on an explicit VM frame vector rather than recursively invoking Rust.

## Lessons adopted from Pima

- keep lowering, IR, verification, and execution in separate modules;
- keep the VM as the sole execution engine;
- use stable semantic IDs for direct calls instead of resolving names at runtime;
- retain a readable structured instruction representation until semantics stabilize;
- keep instruction and source-span tables aligned;
- verify register bounds, constant references, and function references;
- add compiler passes only through an explicit pipeline once there is a demonstrated need.

Foster does not adopt Pima's dynamic binding cells, runtime type constraints, namespace dispatch,
or dynamic callable machinery. Foster's type, ownership, group, and effect passes settle those
questions before bytecode lowering. Runtime checks should therefore cover genuinely dynamic
conditions, not repeat static analysis.

## Implemented evolution

1. Explicit basic blocks, conditional branches, and an iterative call-frame stack.
2. Lists, records, variants, field/index places, and pattern decisions.
3. Closure environment layouts with explicit copy, move, and reference capture instructions.
4. Move operations and liveness-driven deterministic register destruction points.
5. Remote construction, remote calls, futures, and suspension.
6. The legacy AST execution path was removed after the complete example and conformance suite ran
   on the VM.
7. A portable `.fbc` serialization with canonical map ordering and defensive decoding.

## Next backend work

Extend interprocedural optimization with multi-block and profile-guided inlining, scalar
replacement of aggregate closure environments, and specialization across module boundaries; then
add serialization and Cranelift after bytecode semantics stabilize.
