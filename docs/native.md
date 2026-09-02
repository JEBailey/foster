# Native compilation

Status: initial host-native AOT backend implemented with Cranelift.

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
- assertions, guarded returns, `loop`, guarded `break`/`continue`, jumps, conditional control
  flow, and recursion; and
- printing a result from `main` whose type is not `()`, matching `foster run` for these primitive
  values.

Only functions statically reachable from `main` are compiled. An unused function may therefore use
the complete VM language without preventing native compilation.

General lists, String concatenation and library algorithms, symbols, user records, enums,
references, closures, dynamic calls, intrinsics, pattern matching, remote objects, futures, and
host I/O do not yet have a native runtime representation. If one is reachable, compilation stops
before object emission and reports the unsupported type or instruction. The diagnostic recommends
ordinary `foster build`, which emits portable `.fbc` for the complete language.

## Architecture

The frontend, type/effect/ownership checks, ownership MIR, and structured register lowering are
shared with the VM. Native compilation intentionally consumes non-optimized register bytecode so
each register retains a stable static type. A separate lowering builds the shared code-generation
IR: typed basic blocks in SSA form where instructions define immutable values and jumps carry only
the values live at the target block. Foster moves become SSA aliases instead of machine
instructions. The IR has an independent verifier for definitions, dominance, types, call
signatures, block arguments, and terminators. Cranelift consumes this IR directly, so ordinary
values no longer round-trip through a VM-style register stack array before machine optimization.

The portable, versioned bytecode remains the VM's execution and distribution format. The native
IR is a shared internal backend boundary rather than a replacement for bytecode, leaving room for
the VM and other native code generators without exposing Cranelift types to the frontend. Its
Foster scalar types map to Cranelift types only in the Cranelift emitter. All reachable functions
are declared before any is defined, allowing direct recursion and mutual recursion.

The object exports a C-ABI `foster_native_entry` symbol. A generated, temporary Rust entry shim
collects Unicode command arguments, supplies the supported String/List runtime operations, calls
that symbol, formats its result, and supplies the platform startup pieces to the system linker.
Temporary object and shim files are removed after linking; the resulting executable does not
contain or invoke the Foster VM.

Checked integer addition, subtraction, multiplication, invalid shifts, and integer division errors
currently become native machine traps. A future native runtime ABI should turn those traps into the
same friendly runtime diagnostics produced by the VM.
