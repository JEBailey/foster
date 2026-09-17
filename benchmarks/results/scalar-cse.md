# Shared scalar CSE measurements

Measured 2026-09-16 on Windows AMD64 (AMD Family 23 Model 96), Rust 1.98.1.
Baseline: `35a9293`, with the measurement harness added but without shared CSE.
After: the same checkout with the shared CSE pass. Both builds used release Rust
compilation and enabled all normal Foster optimizations, including Cranelift `speed`.

## Repeated comparisons

`benchmarks/scalar_cse.fos` repeats four identical integer comparisons per iteration,
checks their agreement, and returns 300000 after 200000 iterations. Both backends
returned 300000 in every recorded harness run.

| Metric | Before | After | Change |
| --- | ---: | ---: | ---: |
| VM instructions | 68 | 60 | -11.8% |
| VM execution | 188.106 ms | 140.394 ms | -25.4% |
| VM compilation | 2.108 ms | 2.072 ms | -1.7% |
| Native object compilation | 6.488 ms | 6.396 ms | -1.4% |
| Native executable process | 102.325 ms | 101.753 ms | -0.6% |

Numbers are medians of five independent harness-run medians per compiler version.
Each harness run discards one warmup and measures nine samples. Run order alternated
between versions. No compiler builds or test suites ran concurrently with these
measurements. Earlier measurements taken alongside builds were discarded.

The native and compilation differences are too small to establish a meaningful
improvement. VM execution shows a repeatable benefit on this deliberately favorable
workload: four after-run medians were 139.851–140.516 ms, with one 190.448 ms outlier;
before-run medians were 187.293–191.501 ms. This is not a general 25% language speedup.

## Control

The existing `benchmarks/fibonacci.fos` returned 6765 on both backends and retained
54 VM instructions. Two harness runs per version produced these ranges:

| Metric | Before | After |
| --- | ---: | ---: |
| VM execution | 13.662–14.391 ms | 14.881–15.699 ms |
| VM compilation | 1.152–1.157 ms | 1.189–1.208 ms |
| Native object compilation | 5.455–5.641 ms | 5.383–5.904 ms |
| Native executable process | 14.081–14.359 ms | 14.229–15.115 ms |

There is no demonstrated control-workload speedup. The observed VM compile increase
is about 0.03–0.06 ms; its execution samples were also slower. These small runs do not
establish whether that runtime difference is systematic. Larger application and
library-heavy compilation workloads remain outside this measurement's scope.

## Reproduction and scope

Run the same `examples/measure_backends.rs` harness on each compiler revision:

```powershell
$env:FOSTER_NATIVE_CACHE_DIR = Join-Path (Get-Location) 'target/optimizer-test-cache'
cargo run --release --example measure_backends -- benchmarks/scalar_cse.fos target/cse-native.exe
cargo run --release --example measure_backends -- benchmarks/fibonacci.fos target/fib-native.exe
```

Build both harness executables before collecting timings and retain them under separate
names so repeated runs do not rebuild the compiler. Compilation timings start from an
already checked frontend result, include shared construction and optimization, and end
at VM bytecode or native object emission. They exclude frontend checking, executable
linking, and runtime-cache construction. VM runtime runs in-process with a new machine;
native runtime includes process startup, output capture, and shutdown. These absolute
runtime numbers must not be treated as a direct VM-versus-native comparison.

The CSE phase is separately instrumented as `shared.cse` in compiler-profile builds.
The recorded per-run medians are in [scalar-cse.json](scalar-cse.json).
