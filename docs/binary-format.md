# Foster compiled bytecode format

Status: version 4, implemented by `foster::vm::{encode_program, decode_program}`.

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
  sort lexicographically by `(raw ID, UTF-8 name)`.

The reference decoder limits vectors to 16,777,216 items and strings to 64 MiB, rejects unknown
tags, truncation and trailing data, and invokes the VM verifier before returning a program.

## File layout

| Field | Encoding | Meaning |
| --- | --- | --- |
| magic | 8 bytes | ASCII `FOSTERBC` |
| version | `u16` | `4` |
| flags | `u16` | `0`; reserved |
| constants | `vector<Constant>` | global constant pool |
| functions | `vector<(FunctionId, Function)>` | sorted by ID |
| main | optional `FunctionId` | entry point |
| string record | optional `RecordId` | String wrapper |
| symbol record | optional `RecordId` | Symbol wrapper |
| records | `vector<(RecordId, string, vector<string>)>` | runtime name and indexed field layout |
| methods | `vector<(RecordId, string, FunctionId)>` | record dispatch |
| variant methods | `vector<(VariantTypeId, string, FunctionId)>` | variant dispatch |
| variants | `vector<(VariantId, VariantTypeId, string, string)>` | parent, type, alternative |

A function is `string name`, `u16 parameter_count`, `vector<ParameterMode> parameter_modes`,
`vector<bool> mutable_parameters`, `u16 capture_count`, `u16 register_count`,
`vector<Instruction>`, then `vector<Span>`. A span is `u32
start, u32 end` in source byte offsets. Instruction and span counts must match.

## Tagged values

Constant tags: `0 Unit`, `1 Bool(bool)`, `2 Integer(u64 bits)`, `3 Float(u64 bits)`, `4
String(string)`, `5 CodePoint(u32 scalar)`, `6 Symbol(string)`.

Pattern tags: `0 Spanned(Pattern, Span)`, `1 Wildcard`, `2 Binding(LocalId)`, `3 Bool(bool)`, `4
Integer(u64 bits)`, `5 Float(u64 bits)`, `6 String(string)`, `7 CodePoint(string)`, `8 Symbol(string)`,
`9 Variant(VariantId, vector<Pattern>)`.

Capture modes: `0 Copy`, `1 Move`, `2 Ref`. Parameter modes: `0 Borrow`, `1 Consume`. Unary
operators: `0 Negate`, `1 Not`, `2 BitNot`. Binary tags in order are Add, Subtract, Multiply,
Divide, BitAnd, BitOr, BitXor, ShiftLeft, ShiftRight, Equal, NotEqual, Less, LessEqual, Greater,
GreaterEqual. Builtin tags use `hir::Builtin` declaration order, `Print = 0` through
`TcpCloseConnection = 50`.

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
| 27 | CallContractMethod | `R destination, R receiver, string, regs` |
| 28 | MakeClosure | `R destination, F, vector<(CaptureMode, R)>` |
| 29 | CallValue | `R destination, R callee, regs` |
| 30 | CallClosure | `R destination, F, vector<(CaptureMode, R)>, regs` |
| 31 | Return | `R source` |
| 32 | MakeFieldReference | `R destination, R object, string field` |

## Compatibility and canonical form

Version 4 readers accept only version 4 with zero flags. Changing any existing tag, opcode, field,
or meaning requires a new version. A canonical encoder emits sorted maps, exact lengths, no
duplicates, and no trailing data. Thus identical programs produce identical bytes independent of
`HashMap` iteration order.
