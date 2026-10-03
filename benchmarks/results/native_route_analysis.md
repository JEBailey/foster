# Why native compilation from bytecode is faster in some cases

These measurements describe the removed bytecode-to-native experiment. The analysis tool
now emits the optimized default route; saved objects and raw results retain the original comparison.

## Finding

The benefits come from a cleaner control-flow graph and less storage bookkeeping before native
lowering. They are not an inherent advantage of a binary bytecode representation. Both routes
use the same shared semantic optimizer, Cranelift speed optimization, native runtime, and linker.
The bytecode route adds backend cleanup and reconstructs a graph with fewer temporary bindings.

The strongest directly observed contributors are:

1. Fewer basic blocks, hence fewer cancellation/scheduling/failure polls.
2. Fewer copied and carried scalar temporaries, hence fewer stack slots and stores.

No production compiler or runtime source was changed for this analysis. Six workload pairs were
examined using optimized native preparation, disassembled, and compared with diagnostic copies
of the original benchmark executables. Every normal and diagnostic run returned its expected
checksum. The original executables remain intact.

## Pipeline difference

Default native compilation optimizes shared SSA and then lowers it to native representations.
The experimental route optimizes that same shared SSA, lowers it to VM registers, runs VM backend
cleanup, serializes/reloads the result, and reconstructs SSA before native lowering. It does not
run shared optimization again.

The additional active passes in [VM backend finishing](../../compiler/src/vm/optimizer/mod.rs)
include jump-chain redirection, unreachable instruction removal, copy propagation, dead-write
elimination, non-escaping closure specialization, register compaction, and constant-pool
deduplication. The closure pass is not implicated in the six closure-free cases examined here.
Legacy VM inlining algorithms are test-only; extra recursive inlining is not the explanation.

The shared [graph simplifier](../../compiler/src/codegen/optimizer/graph.rs) performs scalar
propagation, constant-branch pruning, and reachability pruning, but does not perform equivalent
general jump-only block threading. Shared CSE already reduces the repeated comparisons in
`scalar_cse` to one comparison in both routes. Its speedup is therefore not evidence that the
bytecode route uniquely performs common-expression elimination.

## Fewer block polls

[Native legalization](../../compiler/src/native/legalize/function.rs) inserts a cancellation poll
at every shared basic block. This is an observable runtime call, so Cranelift cannot simply
discard it when an otherwise empty block is bypassed. The
[runtime helper](../../runtime/src/host.rs) checks cooperative scheduling, cancellation, and
pending execution failure. It also creates failure branches in generated code.

Removing redundant blocks **before** inserting those calls removes repeated runtime work.

| Native function | Static block/poll sites, default → bytecode | Stack frame, default → bytecode |
| --- | ---: | ---: |
| Branching counter `main` | 22 → 17 | 272 → 112 bytes |
| Enum dispatch `main` | 24 → 20 | 320 → 240 bytes |
| Enum dispatch `inspect` | 9 → 7 | 144 → 112 bytes |
| Fibonacci | 7 → 5 | 144 → 144 bytes |
| Repeated-expression `main` | 12 → 10 | 224 → 96 bytes |
| Arithmetic `main` | 6 → 6 | 128 → 96 bytes |
| Growing-list `main` | 6 → 6 | 208 → 192 bytes |

These are static sites, not dynamic execution counts. Fibonacci gives a particularly clear
dynamic example: a successful base-case call visits four polled blocks in the default graph and
three in the bytecode graph; a recursive case visits five versus four. Both still perform the
same recursive calls and arithmetic. Thus the bytecode route avoids one poll on **every**
Fibonacci invocation. At input 35, that is 29,860,703 fewer polls.

## Fewer temporary storage homes

[Native preparation](../../compiler/src/native/program.rs) collects a storage home for every
hinted native value. [Machine lowering](../../compiler/src/native/lowering.rs) allocates stack
slots for those homes and stores incoming block parameters and instruction results into them.
The current scheme backs many ordinary scalar values with memory, even when they never need
an address. Consequently, extra logical temporaries can survive as stack traffic in machine code.

VM copy propagation and register compaction reduce those identities. Reconstruction also uses
per-instruction binding types for liveness: it avoids carrying a scalar's previous binding as if
it might be a reference. Ordinary sealing uses a more conservative set of potential reference
homes, including addition destinations. These are multiple differences; this analysis does not
assign an exact percentage to each individual pass.

| Function | Storage homes, default → bytecode | Block parameters, default → bytecode |
| --- | ---: | ---: |
| Branching counter `main` | 20 → 5 | 172 → 70 |
| Repeated-expression `main` | 18 → 4 | 48 → 22 |
| Arithmetic `main` | 8 → 3 | 21 → 11 |
| Fibonacci | 9 → 9 | 19 → 10 |

The branching counter's emitted function contains 726 versus 449 machine instructions and
424 versus 180 stack-memory references. Those static counts include cold failure paths; they
are not dynamic instruction counts. They nevertheless confirm a substantial difference in
generated code, not just a timing fluctuation. Growing lists show little such benefit: 431 versus
429 machine instructions, and stack-memory references actually rise from 124 to 147.

## Controlled poll bypass

For a diagnostic experiment, only cancellation-call instructions in copies of the benchmark
executables were replaced by an instruction returning a false failure status. Each native
function body was matched exactly against its compiler-generated object, masking linker
relocations; ambiguous matches abort the experiment. This bypass disables cancellation and
failure polling and is **not a valid production optimization**.

Five measured samples followed one warmup, alternating/reversing the four combinations of
route and normal/bypassed polling. Process startup is included. All results were checked.

| Workload | Normal default / bytecode runtime | With poll calls bypassed |
| --- | ---: | ---: |
| Branching counter, 8 million | 1.23x | 1.98x |
| Enum dispatch, 4 million | 1.17x | 1.00x |
| Fibonacci, input 32 | 1.22x | 1.11x |
| Repeated expressions, 8 million | 1.17x | 1.27x |
| Arithmetic, 20 million | 1.01x | 1.69x |
| List growth, 2 million | 1.02x | 0.96x |

Higher ratios favor the bytecode route. Enum dispatch's advantage largely disappears when
poll calls are bypassed; Fibonacci's advantage shrinks. That directly supports the poll-overhead
explanation. Branching and repeated expressions retain a substantial advantage, consistent
with their much smaller temporary state and stack traffic.

Arithmetic's storage improvement is mostly hidden by identical polling work during normal
execution. Growing lists remain roughly tied normally and do not gain in the diagnostic case.
The route is not uniformly better at generating machine code.

The bypass leaves pointer loads, failure tests, stack operations, and CFG layout intact; it does
not reoptimize the patched machine code. Therefore the remaining gaps cannot be attributed
exclusively to stack traffic. Normal ratios also vary between sample batches; retain the original
[larger comparison](native_from_bytecode_large.md) as the broader performance measurement.

## What this suggests changing

The evidence favors improving default native SSA rather than relying on a lossy bytecode
round-trip to obtain these benefits:

1. Thread/merge redundant jump blocks, composing their block arguments, before native polls
   are inserted. Preserve instructions with ownership or other effects.
2. Propagate scalar copies and eliminate dead block parameters and unnecessary entry seeds.
   Use type-aware binding liveness without dropping reference-origin protections.
3. Restrict memory backing to values that actually require addressable storage or ownership
   cleanup, retaining correct alias mutation and failure cleanup.
4. Measure cancellation and scheduling behavior as well as runtime after changing poll
   placement. Removing all polls would invalidate the language/runtime contract.

These are recommendations, not implemented changes. The larger comparison's reconstruction
failures remain: type information lost through register lowering still prevents the bytecode
route from compiling several library-backed programs.

## Evidence and reproduction

[Code-generation metrics](native_route_codegen.json) include object/IR hashes.
[Paired normal and diagnostic samples](native_route_poll_ablation.json) include exact matched
body sizes, patched call counts, checksums, and executable paths. The compiler matches the
larger suite's release compiler hash. IR and assembly are retained under
`target/native-route-analysis`.

The standalone [Rust analysis tool](../native_route_analysis.rs) uses
`prepare_with_options(..., CompileOptions::default())` for the default route. This matters:
the current CLI's ordinary `--emit native-ir` path prepares unoptimized IR, which would be
an unfair comparison against optimized bytecode. The tool avoids that mismatch.

Compile that tool against the matching release `foster_compiler` library, then run it with
`target/native-route-analysis` followed by the six generated source paths recorded in the
large-suite JSON (`branch_counter_4x`, `enum_dispatch_4x`, `fibonacci_32`,
`scalar_cse_8000000`, `scalar_4x`, and `list_push_4x`). Then run:

```powershell
node benchmarks/native_route_codegen.cjs
node benchmarks/native_route_poll_ablation.cjs
```

The disassembly tool currently uses the installed Windows LLVM path. The bypass experiment
supports this measured Windows x64 machine-code format and asserts instruction encodings.
