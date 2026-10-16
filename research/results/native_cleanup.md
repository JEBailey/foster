# Default native cleanup comparison

31 checked workloads on AMD Ryzen 5 4500 6-Core Processor. The smaller workloads cover 14 families. Their family-balanced geometric mean is 1.437× faster (30.4% less runtime); native build time changes by -0.9%. Formatter and parser results appear separately in the table.

Default optimized native route in both compilers. Identical source hashes and expected results from the larger comparison. Three alternating build samples and five alternating runtime samples after warmup. Includes process startup and cleanup. First nine rows were measured again after unrelated background builds ended; their original samples are retained.

Both routes use the default optimized native compiler. The new route keeps source type information and does not reconstruct types from VM bytecode. Each run checks its result against the independently calculated expected value; source hashes are identical. Bytecode artifacts also match between the two frozen compilers for scalar loops, record mutation, and list growth in both optimization modes.

Compiler hashes:

- Before: `9d2e55a63ad48e34c44d667b2a40b0877c46f6130f3217f7dae86525acc75d81`
- After: `dc9503d9ae2e99c4dfece7d2479844ea473f748fa01f466f5da5a57be80210fd`

| Workload | Before (ms) | After (ms) | Runtime reduction | Build change |
| --- | ---: | ---: | ---: | ---: |
| scalar_1x | 555.5 | 358.8 | 35.4% | 5.0% |
| scalar_4x | 2205.8 | 1499.3 | 32.0% | 5.6% |
| record_fresh_1x | 412.0 | 291.1 | 29.4% | 3.4% |
| record_fresh_4x | 1542.7 | 1134.1 | 26.5% | -5.4% |
| record_reuse_1x | 237.3 | 156.6 | 34.0% | -1.6% |
| record_reuse_4x | 920.2 | 600.9 | 34.7% | -2.5% |
| list_fresh_1x | 125.9 | 106.2 | 15.7% | -5.3% |
| list_fresh_4x | 487.1 | 398.6 | 18.2% | 3.7% |
| list_push_1x | 65.1 | 50.7 | 22.1% | -0.6% |
| list_push_4x | 232.4 | 154.6 | 33.5% | -5.8% |
| field_read_1x | 234.7 | 151.9 | 35.3% | 4.1% |
| field_read_4x | 902.8 | 562.7 | 37.7% | -1.8% |
| method_read_1x | 329.3 | 249.1 | 24.4% | -0.9% |
| method_read_4x | 1293.6 | 987.2 | 23.7% | -1.9% |
| list_iterator_1x | 290.4 | 226.9 | 21.9% | 14.4% |
| list_iterator_4x | 979.7 | 791.8 | 19.2% | 1.7% |
| list_snapshot_1x | 276.9 | 235.4 | 15.0% | -2.8% |
| list_snapshot_4x | 1088.9 | 897.2 | 17.6% | 10.1% |
| branch_counter_1x | 851.5 | 436.2 | 48.8% | -19.3% |
| branch_counter_4x | 3508.1 | 1793.7 | 48.9% | 0.4% |
| enum_dispatch_1x | 810.9 | 645.6 | 20.4% | 2.3% |
| enum_dispatch_4x | 2851.1 | 2236.2 | 21.6% | -0.4% |
| float_loop_1x | 250.5 | 176.7 | 29.4% | -1.7% |
| float_loop_4x | 1013.4 | 657.5 | 35.1% | 2.8% |
| scalar_cse_2000000 | 575.1 | 363.9 | 36.7% | -9.1% |
| scalar_cse_8000000 | 2396.8 | 1441.9 | 39.8% | 2.0% |
| fibonacci_28 | 197.3 | 131.5 | 33.4% | -2.5% |
| fibonacci_32 | 1239.4 | 734.7 | 40.7% | -2.9% |
| fibonacci_35 | 5232.7 | 3089.1 | 41.0% | -17.5% |
| formatter_1000_functions | 1539.6 | 815.6 | 47.0% | -6.8% |
| taker_10000_integers | 892.0 | 650.3 | 27.1% | 0.3% |

Generated code for the default route:

| Workload / function | Polls before → after | Block parameters before → after | Stack reservation before → after (bytes) |
| --- | ---: | ---: | ---: |
| branch_counter_4x / main | 22 → 10 | 172 → 26 | 272 → 64 |
| enum_dispatch_4x / inspect | 9 → 7 | 23 → 9 | 144 → 64 |
| enum_dispatch_4x / main | 24 → 17 | 159 → 50 | 320 → 80 |
| fibonacci_32 / fibonacci | 7 → 3 | 19 → 3 | 144 → 64 |
| fibonacci_32 / main | 1 → 1 | 0 → 0 | 64 → 48 |
| scalar_cse_8000000 / main | 12 → 7 | 48 → 12 | 224 → 64 |
| scalar_4x / main | 6 → 4 | 21 → 5 | 128 → 64 |
| list_push_4x / main | 6 → 4 | 16 → 5 | 208 → 112 |

Poll counts are static sites, not dynamic execution counts. Stack reservations include backend spill and call storage. IR home annotations are retained as identities and do not count allocated stack slots.

Native cleanup removes non-addressable scalar copies and unused scalar block parameters, threads poll-only jump blocks, and allocates stack homes only for address-taken storage. Managed ownership operations, exposed storage, checked arithmetic, poll-only cycles, and failure cleanup remain intact. Unoptimized native lowering is unchanged.

Validation: 180 compiler/runtime tests passed (one existing ignored test), plus all 24 native integration tests. Coverage includes cancellation while records remain live, failure reclamation, reference captures, address-taken pattern bindings, copy-on-write mutation, collections, contract dispatch, remote workers, and host services. A native regression check confirms VM encoding is unchanged and scalar workloads allocate no addressable stack homes.

These are local paired measurements with process startup and cleanup included. They establish improvements for the measured workloads, not a guarantee for every Foster program. Original early samples are retained alongside the repeated measurements.
