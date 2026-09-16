# Foster compiled bytecode format

Format version 29; encoding and decoding use `foster::vm::{encode_program, decode_program}`.

The Foster bytecode format (`.fbc`) is a deterministic, portable representation of the register
VM `Program` produced after shared-SSA sealing, de-SSA lowering, optimization, drop insertion, and
verification. It contains everything needed to verify and execute a compiled codebase. It does not
preserve typed HIR, shared SSA, documentation, source, or diagnostics.

Type metadata uses the backend-neutral `foster::codegen::types::ExecutableType` and
`Specialization` definitions. Their binary tags and validation belong to the VM format;
the Rust module location does not affect the encoding.

## Conventions

- Integers are unsigned little-endian unless stated otherwise. Fixed widths are `u8`/`u16`/`u32`/`u64`.
- Signed integers use their two's-complement bits; floats use exact IEEE-754 binary64 bits in `u64`.
- A bool is one byte (`0` or `1`).
- A string is `u32 byte_length` plus UTF-8 bytes. A vector is `u32 item_count` plus its items.
- Arena IDs are raw `u32` indexes. Registers are `u16`.
- An optional ID is tag `0` or tag `1` followed by the ID. Enum tags and opcodes are `u8`.
- Map entries are vectors. Duplicate keys are invalid. Arena maps sort by raw ID; composite maps
  sort by their tuple keys.

The reference decoder limits vectors to 16,777,216 items and strings to 64 MiB, rejects unknown
tags, truncation and trailing data, and invokes the VM verifier before returning a program.

## File layout

| Field | Encoding | Meaning |
| --- | --- | --- |
| magic | 8 bytes | ASCII `FOSTERBC` |
| version | `u16` | `27` |
| flags | `u16` | `0`; reserved |
| constants | `vector<Constant>` | global constant pool |
| functions | `vector<(FunctionId, Function)>` | sorted by ID |
| drops inserted | `bool` | whether automatic register cleanup has already been inserted |
| main | optional `FunctionId` | entry point |
| main arguments | `bool` | whether `main` receives `std.process.Arguments` |
| string record | optional `RecordId` | String wrapper |
| symbol record | optional `RecordId` | Symbol wrapper |
| list record | optional `RecordId` | canonical core List identity |
| bytes record | optional `RecordId` | canonical core Bytes identity |
| byte buffer record | optional `RecordId` | canonical core ByteBuffer identity |
| remote result | optional `VariantTypeId` | nominal `core.result.Result` for remote outcomes |
| remote error | optional `VariantTypeId` | nominal `core.remote_error.RemoteError` for remote failures |
| records | `vector<(RecordId, string, vector<string> parameters, vector<(string, ExecutableType)>)>` | runtime name, generic parameters, and typed indexed field layout |
| dispatch | `vector<(NominalTypeId, u32 slot, FunctionId)>` | record and enum dispatch |
| enum cases | `vector<(VariantId, VariantTypeId, string, vector<string> parameters, string, vector<ExecutableType>)>` | parent enum, generic parameters, case label, and declared payload layout |
| symbolic modules | `string` | compact UTF-8 JSON descriptor table, version 2 |

The symbolic module table groups package-qualified type/function identities, semantic descriptors,
implementation bindings, and required imports. See [symbolic modules](symbolic-modules.md). Its JSON
schema is defined by `symbols::Table`; unknown fields and unsupported descriptor versions are
rejected. The writer orders modules, definitions, imports, and nominal bindings by symbolic identity.
The existing 64 MiB string bound and the JSON decoder's nesting bound apply to this section.

Core wrapper IDs identify standard-library records independently of user type names. Version 25
adds List, Bytes, and ByteBuffer identities; older bytecode must be rebuilt from source.

Remote outcome IDs identify the actual Result and RemoteError enums, independently of similarly
named user types. The verifier checks their cases, generic arity, and payload types before
accepting remote calls or awaits.

Dispatch slots are program-local `u32` identifiers assigned to the contract signatures selected by
type checking. The type checker also resolves every concrete record and enum implementation for
each used slot. Runtime lookup is therefore a direct `(concrete type, slot)` table access and does
not repeat signature matching. A `NominalTypeId` is tag `0` followed by a `RecordId`, or tag `1`
followed by a `VariantTypeId`.

The three highest slots are reserved: `0xffffffff` invokes Copy, `0xfffffffe` queries Copy,
and `0xfffffffd` identifies Drop. The query has no implementation entry. Copy/Drop entries
must have one borrowed receiver, no captures, and respectively the receiver type or unit result.
Only ownership cleanup may invoke Drop. Version 23 also records whether automatic drops were
inserted, and distinguishes moving a register from taking the pointee of a generated reference.

A function is `string name`, `bool intrinsic_stub`, `u16 parameter_count`,
`vector<ExecutableType> parameter_types`, `vector<ParameterMode> parameter_modes`,
`vector<bool> mutable_parameters`, `bool returns_reference`, `u16 capture_count`,
`vector<ExecutableType> capture_types`, `ExecutableType result_type`, `u16 register_count`,
`vector<Instruction>`, then `vector<Span>`. A span is `u32 start, u32 end` in source byte offsets.
Instruction and span counts must match. An intrinsic stub describes a source-level intrinsic whose
executable calls have already lowered to `Builtin`; it is retained for identity but is not an
executable call target.

## Tagged values

Constant tags: `0 empty tuple (())`, `1 Bool(bool)`, `2 Integer(u64 bits)`, `3 Float(u64 bits)`, `4
String(string)`, `5 CodePoint(u32 scalar)`, `6 Symbol(string)`.

Pattern tags: `0 Spanned(Pattern, Span)`, `1 Wildcard`, `2 Binding(LocalId)`, `3 Bool(bool)`, `4
Integer(u64 bits)`, `5 Float(u64 bits)`, `6 String(string)`, `7 CodePoint(string)`, `8 Symbol(string)`,
`9 Variant(VariantId, vector<Pattern>)`, and
`10 IsType(ExecutableType target, ExecutableType source, vector<ExecutableType> conforming, optional<LocalId> binding)`.
Source typing preserves semantic distinctions such as String versus Symbol during
native specialization. Conformance witnesses use the structural type checker;
linking refreshes library witnesses to include client-defined types.

Capture modes: `0 Copy`, `1 Move`, `2 Ref`. Parameter modes: `0 Borrow`, `1 Consume`. Unary
operators: `0 Negate`, `1 Not`, `2 BitNot`. Binary tags in order are Add, Subtract, Multiply,
Divide, BitAnd, BitOr, BitXor, ShiftLeft, ShiftRight, Equal, NotEqual, Less, LessEqual, Greater,
GreaterEqual. Builtin tags use the explicit stable values in the intrinsic registry, from `Print = 0` through
`TcpWaitAccept = 64`. Readiness tags are `TcpWaitReadable = 62`, `TcpWaitWritable = 63`,
and `TcpWaitAccept = 64`. Other host-service tags include `IoReadRange = 56`, `IoAppendBytes = 57`,
`IoFileLength = 58`, `TimeWallNow = 59`, `TimeMonotonicNow = 60`, and `RandomBytes = 61`.

Aggregate metadata retains declared record-field and enum-payload types, nominal parameter names,
and concrete construction arguments. Statically resolved calls and closure construction retain
deterministic generic substitutions, so code and environment layout share one specialization
identity. `MakeList` retains its concrete element type, and reference construction retains its
pointee type. These preserve empty generic-list and projected-reference layouts across the shared
SSA, bytecode, and native boundaries.

Verification type tags are `0 Unknown`, `1 Unit`, `2 Bool`, `3 Integer`, `4 Float`, `5 CodePoint`,
`6 Byte`, `7 Bytes`, `8 ByteBuffer`, `9 List(ExecutableType)`,
`10 Reference(ExecutableType)`, `11 Remote(ExecutableType)`, `12 Future(ExecutableType)`,
`13 Function(vector<ExecutableType>, vector<ParameterMode>, ExecutableType)`,
`14 Record(RecordId, vector<ExecutableType> arguments)`,
`15 Variant(VariantTypeId, vector<ExecutableType> arguments)`,
`16 Alternatives(vector<ExecutableType>)` (formerly named `Union`), and `17 Generic(string identity)`.
Alternative members are sorted, unique, flattened, contain at least two types, and exclude `Unknown`
(which would absorb the alternatives). The tag and encoding are unchanged. Native `Intersection`
and `AliasArguments` metadata have no bytecode tags and cannot be serialized.
Readers reject verification types nested more than 64
levels deep. Generic identities are retained so target-specific layout selection can materialize a
concrete nominal layout for each reachable native specialization.

Callable parameter type and mode vectors must have equal lengths. Decoding checks this before
constructing the paired in-memory parameters, including for nested callables. Specialization names
must be strictly ascending and unique; decoding rejects invalid order rather than sorting it.
These construction checks do not change the wire format or version.

## Instructions

Each starts with its opcode. `R` is a register, `F` a function ID, and `regs` a register vector.

| Op | Instruction | Operands in encoded order |
| --: | --- | --- |
| 0 | Drop | `R` |
| 1 | LoadConstant | `R destination, u16 constant` |
| 2 | Move | `R destination, R source` |
| 3 | Unary | `R destination, UnaryOp, R operand` |
| 4 | Binary | `R destination, BinaryOp, R left, R right` |
| 5 | MakeList | `R destination, ExecutableType element, regs` |
| 6 | Index | `R destination, R object, R index` |
| 7 | MakeRecord | `R destination, RecordId, vector<ExecutableType> arguments, vector<(string, R)>` |
| 8 | MakeVariant | `R destination, VariantId, vector<ExecutableType> arguments, regs` |
| 9 | LoadField | `R destination, R object, string, bool by_reference` |
| 10 | StoreField | `R object, string, R source` |
| 11 | StoreIndex | `R object, R index, R source` |
| 12 | MakeReference | `R destination, ExecutableType pointee, R object, R index` |
| 13 | MoveOut | `bool by_reference, R destination, R source` |
| 14 | Push | `R destination, R object, R value` |
| 15 | Append | `R destination, R object, R value` |
| 16 | Contains | `R destination, R value, regs` |
| 17 | Builtin | `R destination, Builtin, regs` |
| 18 | SpawnRemote | `R destination, R value` |
| 19 | SpawnRemoteBorrow | `R destination, R source` |
| 20 | RemoteCall | `R destination, R remote, F, vector<(ParameterMode, R)>` |
| 21 | Await | `R destination, R future` |
| 22 | MatchPattern | `R destination, R subject, Pattern, regs bindings` |
| 23 | Jump | `u32 target` |
| 24 | JumpIfFalse | `R condition, u32 target` |
| 25 | Call | `R destination, F, vector<(string, ExecutableType)> substitutions, regs` |
| 26 | CallMethod | `R destination, R receiver, F, vector<(string, ExecutableType)> substitutions, regs` |
| 27 | CallContractMethod | `R destination, R receiver, u32 slot, string name, regs, ExecutableType result` |
| 28 | MakeClosure | `R destination, F, vector<(string, ExecutableType)> substitutions, vector<(CaptureMode, R)>` |
| 29 | CallValue | `R destination, R callee, regs` |
| 30 | CallClosure | `R destination, F, vector<(string, ExecutableType)> substitutions, vector<(CaptureMode, R)>, regs` |
| 31 | Return | `R source` |
| 32 | MakeFieldReference | `R destination, ExecutableType pointee, R object, string field` |
| 33 | Assert | `R condition, optional<R> message` |
| 34 | MakeWholeReference | `R destination, ExecutableType pointee, R object` |

## Compatibility and canonical form

Version 29 readers accept only version 29 with zero flags. String accessors use Foster
grapheme algorithms, and strings implement `Sequence<String>`; scalar-based artifacts
from earlier versions must be rebuilt. The format retains the symbolic module table.
Contract calls retain their checked
generic result type, including when the receiver's concrete representation is erased.
Development bytecode from another version
must be rebuilt. Changing any existing tag, opcode, field, or meaning requires a new version. A
canonical encoder emits sorted maps, exact lengths, no
duplicates, and no trailing data. Thus identical programs produce identical bytes independent of
`HashMap` iteration order.
