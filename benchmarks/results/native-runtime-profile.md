# Native runtime profile

Measured 2026-09-17 on Windows AMD64 (AMD Family 23 Model 96), Rust 1.98.1,
with the working tree's shared scalar CSE enabled. Release Rust compilation and
normal Foster native optimizations were enabled. This is a microbenchmark baseline,
not a before/after production optimization result.

Follow-up: [cancellation-status reuse](native-poll-status-reuse.md) implements the
first recommendation and records paired execution measurements and validation.

## Results

Each fixture runs 100,000 iterations. Times are medians of seven measured fresh
processes after one discarded warmup. The timer surrounds native entry execution,
including frame cleanup; compilation, process startup, runtime initialization, and
output are excluded. All executables were built before timing. Counters were
collected in a separate invocation, without affecting ordinary timing samples.

| Workload | Native ms | Allocations | Requested bytes | Cancellation polls | Diagnostic bypass ms |
| --- | ---: | ---: | ---: | ---: | ---: |
| Scalar loop | 23.0844 | 0 | 0 | 300,004 | 11.0878 |
| Fresh record | 39.3516 | 100,000 | 4,000,000 | 300,004 | 24.1162 |
| Reused record | 23.2303 | 1 | 40 | 300,004 | 11.3166 |
| Direct field reads | 23.4165 | 1 | 40 | 300,004 | 11.7477 |
| Method reads | 38.7403 | 1 | 40 | 400,004 | 22.1260 |
| Fresh four-element list | 41.5336 | 200,000 | 8,000,000 | 300,004 | 28.1869 |
| Growing list | 23.8859 | 19 | 2,097,192 | 300,004 | 12.0046 |
| Iterator without list mutation | 182.2461 | 700,000 | 28,800,000 | 1,200,004 | 132.8201 |
| Iterator with list mutation | 190.5824 | 900,000 | 36,800,000 | 1,200,004 | 141.4142 |

The diagnostic bypass replaces the cancellation helper body with a zero return;
generated caller failure checks remain. It removes scheduling/cancellation behavior
and is **not a valid application runtime**. It measures a potential source of cost,
not a safe or guaranteed optimization. Timing modes ran sequentially rather than
in randomized order, so differences also include possible machine drift.

Raw samples, checksums, and counters are in [the JSON results](native-runtime-profile.json).

## Priorities supported by this profile

1. **Remove redundant failure querying at cancellation polls.** The scalar loop
   performs 300,004 polls without heap allocations. Bypassing the helper body reduces
   its median by 52.0%. Source inspection finds that
   `runtime/src/host.rs::foster_rt_v4_cancellation_point` already returns
   `foster_rt_v4_failure_pending()`, but `src/native/operations.rs::runtime_call`
   invokes `propagate_native_failure`, which queries failure again. Reusing the
   returned status is a concrete optimization candidate. Preserve scheduling,
   cancellation, and frame cleanup, then measure the actual gain; the full bypass
   percentage must not be attributed to this duplication alone.
2. **Investigate small method and iterator call overhead.** Method reads take
   38.74 ms versus 23.42 ms for direct reads, with one allocation in either case.
   Additional polling explains part of the difference; the remaining difference
   needs attribution before choosing inlining or ownership changes. Iterator
   fixtures have much higher allocation and poll counts even without list mutation.
3. **Target escaping snapshots and temporary allocation.** Mutating a list while
   its iterator retains a snapshot adds 200,000 allocation requests and 8 MB of
   requested storage over the otherwise identical iterator fixture. The ordinary
   median increases by 8.34 ms (4.6%). Fresh records allocate once per iteration,
   whereas the reused record allocates only once overall. List growth already uses
   amortized capacity: only 19 requests for 100,001 elements. These results favor
   focused temporary/snapshot analysis over a blanket allocator rewrite.

## Scope and validation

The ignored profiling test verifies reduced inputs independently on the VM and
checks the expected native result in every timing and census invocation. All nine
fixtures passed across all three modes. Allocation and deallocation counts match
for every fixture's measured entry scope; this is not a global leak proof.

The census counts Foster allocation hooks, not arbitrary host Rust allocations.
Requested bytes are cumulative, not peak live memory. Explicit buffer-copy helper
counts are zero throughout, but inline copies and inline retain/release atomics are
not counted. Consequently this report does not quantify all copying or reference
count traffic. This is controlled timing and instrumentation, not a CPU sampling
profile or a flamegraph of a representative application.

Instrumentation and the diagnostic executable exist only in the opt-in test path.
No production runtime behavior or ABI was changed. See
[reproduction instructions](../../docs/benchmarking.md#native-runtime-profiling).
