# Compiler and optimizer measurements

These recorded local measurements were moved from the benchmarking guide.
Use the [current harness instructions](../../documentation/benchmarking.md) for
new measurements; the figures below describe their original runs.

## Instrumented allocation sample (2026-09-15)

A local Windows debug sample on 2026-09-15 compared the instrumented pipeline before and
after shared-state reuse and conditional rechecking/cleanup. Allocation requests averaged over
the final four Fibonacci iterations were:

| Operation | Before | After |
| --- | ---: | ---: |
| Frontend checking | 5,124 | 3,814 |
| Native preparation, optimization disabled | 5,164 | 4,599 |
| Native preparation, optimization enabled | 6,087 | 5,942 |
| VM compilation, optimization disabled | 5,482 | 5,493 |
| VM compilation, optimization enabled | 7,526 | 6,961 |

On that sample, optimized VM CFG cleanup calls fell from four to two and register allocation
calls from two to one. These are workload-specific allocation and scheduling observations;
the small instrumented timing sample does not establish a general compiler speedup.

## Criterion optimizer comparison (2026-09-12)

A local Windows release run on 2026-09-12 measured these central timing estimates:

| Workload | Before | After |
| --- | ---: | ---: |
| Many constants | 13.74 ms | 2.52 ms |
| Loop liveness | 54.31 ms | 32.37 ms |

Both stress workloads improved significantly in Criterion's comparison. The small Fibonacci
optimized and unoptimized lowering benchmarks showed no statistically significant change.
These are local measurements, not timing thresholds enforced by tests.
