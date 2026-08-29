# Foster register VM

Status: typed-HIR lowering, optimizing register IR, verifier, and reference execution core
implemented. A strict subset of this register IR also feeds the Cranelift AOT backend.

Foster uses a custom register VM as its executable semantic reference. The pipeline is:

```text
source -> AST -> resolved HIR -> type/effect/loan/ownership checks
       -> ownership MIR validation -> structured register bytecode
       -> optional optimizer -> liveness-driven drops -> verifier -> machine
```

Ownership-MIR and bytecode lowering consume the authoritative semantic branch/loop CFG in
`src/control_flow.rs`.
Conditional arm tests are evaluated in sequence, matched arms complete the branch, and `continue`
is exclusively a loop transfer. Branch result decisions use the same reachability-aware arm-flow
summary.

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
their enum and case names through program metadata rather than allocating those strings per
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
method calls, assertions, guarded returns, `try` Result propagation, loops, guarded
`break`/`continue`, conditional and
subject-based pattern branches, jumps, lists and indexing, records and field mutation, enum-case
construction, atomic pattern bindings, closure
environments, dynamic and specialized non-escaping closure calls, partial application, projected
references, structural mutation, remote objects, futures, await, and returns.
Lowering rejects unsupported HIR explicitly; it never interprets an unsupported node as a fallback.
`try` evaluates its operand into one register, tests the `Result.Ok` payload, and lowers the
`Result.Error` edge to the existing return machinery, so it requires no dedicated bytecode
instruction or format change.

Function and concrete-method overloads are selected during type checking and lower to their exact
function IDs. A structural-contract call lowers to a program-local dispatch slot. Type checking
builds one complete `(nominal type, slot) -> function` table for records and enums, so the VM performs one direct lookup
and never repeats overload or generic-signature matching. Lowering consumes the resolved call kind
directly; it does not search for methods or reclassify contract members from their receiver types.

Closure frames use a fixed `[captures][parameters][locals/temporaries]` register layout. Copy and
move captures contain values; reference captures contain weak `PlaceHandle`s that are materialized
as forwarding slots in the called frame. Reference wrappers are flattened when captured so an
escaping closure points directly to the original place. Named recursion resolves directly to a
function ID. Calls execute on an explicit VM frame vector rather than recursively invoking Rust.

## VM design principles

- keep lowering, IR, verification, and execution in separate modules;
- keep the VM as the complete semantic reference while native coverage grows;
- use stable semantic IDs for direct calls instead of resolving names at runtime;
- retain a readable structured instruction representation until semantics stabilize;
- keep instruction and source-span tables aligned;
- verify register bounds, constant references, and function references;
- add compiler passes only through an explicit pipeline once there is a demonstrated need.

Foster resolves type, ownership, group, effect, namespace, and callable questions before bytecode
lowering. Runtime checks therefore cover genuinely dynamic conditions rather than repeating static
analysis.

## Implemented evolution

1. Explicit basic blocks, conditional branches, and an iterative call-frame stack.
2. Lists, records, enums, field/index places, and pattern decisions.
3. Closure environment layouts with explicit copy, move, and reference capture instructions.
4. Move operations and liveness-driven deterministic register destruction points.
5. Remote construction, remote calls, futures, and suspension.
6. The legacy AST execution path was removed after the complete example and conformance suite ran
   on the VM.
7. A portable `.fbc` serialization with canonical map ordering and defensive decoding.

## Related native backend

The initial [native backend](native.md) finds functions reachable from `main`, validates its
supported primitive subset, and lowers unoptimized structured bytecode to Cranelift machine code.
Cranelift performs machine-level optimization independently. Keeping this route downstream of the
same checked compiler IR lets native execution reuse the language's type, effect, and ownership
decisions without weakening the VM's role as the complete reference implementation.
