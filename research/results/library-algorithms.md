# Collection algorithm measurements (2026-09-04)

A local Windows release run on 2026-09-04 measured 67.1 ms for the recursive-tail reference and
2.83 ms for the indexed fold with optimized bytecode (about 24x faster). This compares both
strategies in the same build, not two historical releases; timings are machine-dependent.

The harness compares matching 2,048-element list folds with identical input
construction. See the [current benchmark instructions](../../documentation/benchmarking.md).
