# Foster compiled bytecode format

Status: version 17, implemented by `foster::vm::{encode_program, decode_program}`.

The Foster bytecode format (`.fbc`) is a deterministic, portable representation of the register
VM `Program` produced after lowering and optimization. It contains everything needed to verify and
execute a compiled codebase. It does not preserve typed HIR, documentation, source, or diagnostics.

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
| version | `u16` | `17` |
| flags | `u16` | `0`; reserved |
| constants | `vector<Constant>` | global constant pool |
| functions | `vector<(FunctionId, Function)>` | sorted by ID |
| main | optional `FunctionId` | entry point |
| main arguments | `bool` | whether `main` receives `std.process.Arguments` |
| string record | optional `RecordId` | String wrapper |
| symbol record | optional `RecordId` | Symbol wrapper |
| records | `vector<(RecordId, string, vector<string>)>` | runtime name and indexed field layout |
| dispatch | `vector<(NominalTypeId, u32 slot, FunctionId)>` | record and enum dispatch |
| enum cases | `vector<(VariantId, VariantTypeId, string, string)>` | parent enum and case label |

Dispatch slots are program-local `u32` identifiers assigned to the contract signatures selected by
type checking. The type checker also resolves every concrete record and enum implementation for
each used slot. Runtime lookup is therefore a direct `(concrete type, slot)` table access and does
not repeat signature matching. A `NominalTypeId` is tag `0` followed by a `RecordId`, or tag `1`
followed by a `VariantTypeId`.

A function is `string name`, `bool intrinsic_stub`, `u16 parameter_count`,
`vector<VerificationType> parameter_types`, `vector<ParameterMode> parameter_modes`,
`vector<bool> mutable_parameters`, `bool returns_reference`, `u16 capture_count`,
`vector<VerificationType> capture_types`, `VerificationType result_type`, `u16 register_count`,
`vector<Instruction>`, then `vector<Span>`. A span is `u32 start, u32 end` in source byte offsets.
Instruction and span counts must match. An intrinsic stub describes a source-level intrinsic whose
executable calls have already lowered to `Builtin`; it is retained for identity but is not an
executable call target.

## Tagged values

Constant tags: `0 empty tuple (())`, `1 Bool(bool)`, `2 Integer(u64 bits)`, `3 Float(u64 bits)`, `4
String(string)`, `5 CodePoint(u32 scalar)`, `6 Symbol(string)`.

Pattern tags: `0 Spanned(Pattern, Span)`, `1 Wildcard`, `2 Binding(LocalId)`, `3 Bool(bool)`, `4
Integer(u64 bits)`, `5 Float(u64 bits)`, `6 String(string)`, `7 CodePoint(string)`, `8 Symbol(string)`,
`9 Variant(VariantId, vector<Pattern>)`.

Capture modes: `0 Copy`, `1 Move`, `2 Ref`. Parameter modes: `0 Borrow`, `1 Consume`. Unary
operators: `0 Negate`, `1 Not`, `2 BitNot`. Binary tags in order are Add, Subtract, Multiply,
Divide, BitAnd, BitOr, BitXor, ShiftLeft, ShiftRight, Equal, NotEqual, Less, LessEqual, Greater,
GreaterEqual. Builtin tags use the explicit stable values in the intrinsic registry, from `Print = 0` through
`RandomBytes = 61`. Version 14 appended `IoReadRange = 56`, `IoAppendBytes = 57`, and
`IoFileLength = 58`. Version 15 appended `TimeWallNow = 59` and `TimeMonotonicNow = 60`.
Version 16 appends `RandomBytes = 61`; all earlier tags retain their previous values.

Verification type tags are `0 Unknown`, `1 Unit`, `2 Bool`, `3 Integer`, `4 Float`, `5 CodePoint`,
`6 Byte`, `7 Bytes`, `8 ByteBuffer`, `9 List(VerificationType)`,
`10 Reference(VerificationType)`, `11 Remote(VerificationType)`, `12 Future(VerificationType)`,
`13 Function(vector<VerificationType>, vector<ParameterMode>, VerificationType)`,
`14 Record(RecordId)`, `15 Variant(VariantTypeId)`, and
`16 Union(vector<VerificationType>)`. Union members are sorted, unique, and contain at least two
types. Readers reject verification types nested more than 64 levels deep.

## Instructions

Each starts with its opcode. `R` is a register, `F` a function ID, and `regs` a register vector.

| Op | Instruction | Operands in encoded order |
| --: | --- | --- |
| 0 | Drop | `R` |
| 1 | LoadConstant | `R destination, u16 constant` |
| 2 | Move | `R destination, R source` |
| 3 | Unary | `R destination, UnaryOp, R operand` |
| 4 | Binary | `R destination, BinaryOp, R left, R right` |
| 5 | MakeList | `R destination, regs` |
| 6 | Index | `R destination, R object, R index` |
| 7 | MakeRecord | `R destination, RecordId, vector<(string, R)>` |
| 8 | MakeVariant | `R destination, VariantId, regs` |
| 9 | LoadField | `R destination, R object, string, bool by_reference` |
| 10 | StoreField | `R object, string, R source` |
| 11 | StoreIndex | `R object, R index, R source` |
| 12 | MakeReference | `R destination, R object, R index` |
| 13 | MoveOut | `R destination, R source` |
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
| 25 | Call | `R destination, F, regs` |
| 26 | CallMethod | `R destination, R receiver, F, regs` |
| 27 | CallContractMethod | `R destination, R receiver, u32 slot, string name, regs` |
| 28 | MakeClosure | `R destination, F, vector<(CaptureMode, R)>` |
| 29 | CallValue | `R destination, R callee, regs` |
| 30 | CallClosure | `R destination, F, vector<(CaptureMode, R)>, regs` |
| 31 | Return | `R source` |
| 32 | MakeFieldReference | `R destination, R object, string field` |
| 33 | Assert | `R condition, optional<R> message` |
| 34 | MakeWholeReference | `R destination, R object` |

## Compatibility and canonical form

Version 17 readers accept only version 17 with zero flags. Development bytecode from another version
must be rebuilt. Changing any existing tag, opcode, field, or meaning requires a new version. A
canonical encoder emits sorted maps, exact lengths, no
duplicates, and no trailing data. Thus identical programs produce identical bytes independent of
`HashMap` iteration order.
