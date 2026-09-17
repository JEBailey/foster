# Native cancellation status reuse

Measured 2026-09-17 on Windows AMD64, Rust 1.98.1, with normal Foster optimizations
and release Rust compilation enabled.

## Change

Native cancellation polls now branch on the failure status returned by
`foster_rt_v4_cancellation_point`, instead of querying `foster_rt_v4_failure_pending`
again. The measured change left the runtime helper and cooperative scheduling unchanged.
Both paths use the same generated live-value cleanup and placeholder-return code.
Other fallible runtime calls retain their separate failure queries.

## Measured execution improvement

| Workload | Before ms | After ms | Time reduction |
| --- | ---: | ---: | ---: |
| Scalar loop | 22.5914 | 12.4507 | 44.9% |
| Fresh record | 38.7204 | 25.6242 | 33.8% |
| Reused record | 22.8538 | 12.6381 | 44.7% |
| Direct field reads | 23.2114 | 12.4782 | 46.2% |
| Method reads | 37.2563 | 23.5777 | 36.7% |
| Fresh list | 40.8886 | 29.9924 | 26.6% |
| Growing list | 23.6065 | 13.0829 | 44.6% |
| Iterator without mutation | 181.5981 | 137.1175 | 24.5% |
| Iterator with mutation | 189.3161 | 146.9930 | 22.4% |

These are normal runtime executables, not the diagnostic cancellation bypass.
The original baseline executables were preserved before rebuilding. Old and new
executables were then run in alternating order, reversing order on successive
pairs, with one warmup per variant and seven measured samples per variant. Every
run checked its result and exit status. All builds and test processes finished
before measurement. Each fixture runs 100,000 iterations; timing surrounds native
entry execution and cleanup, excluding process startup, initialization, and output.

[Paired samples and executable hashes](native-poll-status-reuse.json) record the
comparison. [The new profiling run](native-poll-status-reuse-profile.json) includes
the separate census. Allocation requests, requested bytes, frees, explicit copy
helper counts, and cancellation-poll counts match the original baseline for all
nine workloads. Polls were not removed or made less frequent.

The observed 22–46% reduction applies to these microbenchmarks on this machine;
it is not a whole-application performance guarantee. The profiler's diagnostic
bypass now also bypasses the only poll failure query, so its timings should not
be compared directly with the earlier bypass variant. No compilation-cost claim
is made by this execution benchmark.

## Validation

- All 326 compiler unit tests passed; the profiling test is separately opt-in.
- The new regression passes with native optimization enabled and disabled. It
  counts failure queries across 300,004 loop polls (allowing two separate record
  lifetime queries), injects failure while a record is live, and checks complete
  allocation reclamation. Duplicating each poll query fails this regression.
- Existing remote-owner cancellation, single-worker scheduling, frame cleanup,
  and transferred-argument reclamation tests passed.
- All 89 backend parity cases passed in both debug and release, counting the
  targeted reruns after correcting a stale frontend diagnostic expectation.
- All 23 native integration tests passed; all 16 ownership-soundness tests passed
  in debug and release after correcting an outdated dump-version assertion.
- The workspace test sweep covered every integration target, host/runtime tests,
  documentation examples, and doctests. Stale CLI dump-version and documented
  function-count assertions were also corrected and passed targeted reruns.
- All nine profiling workloads passed in all three profiling modes. Formatting,
  whitespace checks, and all-target compilation passed.

### Initial repository-wide failures and follow-up fixes

The initial full suite was **not entirely green**. Six tests failed on paths that
do not reach the changed native code generator:

| Test | Observed failure |
| --- | --- |
| `foster::public_library_modules_declare_tests_or_have_external_coverage` | `core/copy.fos` lacks declared module-test coverage. |
| `language::assertions_stop_the_current_invocation_with_an_optional_message` | Expects older type-mismatch wording; receives a structural-adaptation diagnostic. |
| `language::checks_explicit_import_core_library_usage` | Expects the `core` module to be implicit. |
| `language::structural_adaptation_reports_missing_and_incompatible_fields` | Expects older incompatible-field diagnostic wording. |
| `language_ownership::checks_qualified_call_arguments` | Expects the phrase `type mismatch` in the diagnostic. |
| `remote_lifetime::consumed_owner_parameters_cannot_return_pending_futures` | Frontend accepts an escape that the test expects to reject; this needs a separate ownership investigation. |

Strict `cargo clippy --all-targets -- -D warnings` also initially reported 52 diagnostics in
unchanged code, including clone-on-Copy, needless borrows, and test-module layout.

The follow-up cleanup removes the Clippy diagnostics without suppressing lints.
Structural-conversion errors now include expected/found types as well as the
private-field reason. The core namespace test recognizes the source-backed,
documented `core.fos` module. `core.copy` now has scalar and independently owned string
tests instead of a coverage exemption.

The ownership failure was a compiler bug: initializing a remote parameter's
borrow provenance overwrote its seeded owner identity. The remote analysis now
maps that parameter-contents origin back to the parameter owner, preserving
pending request obligations. Regression tests cover implicit and explicit returns,
local moves, and stored futures; awaiting before destruction remains accepted.

The new library tests also exposed an inconsistent constant-deduplication fixture:
it replaced the constant pool while retaining library functions that referenced
the old pool. That fixture now retains only the function whose constants it tests.
All six original failures pass targeted debug reruns, the expanded remote suite
passes all ten tests, and strict workspace Clippy passes. Follow-up logs are under
`target/fixes-*.log`. The initial remote-lifetime failure log includes a large
compilation dump, so read `target/poll-*.log` with bounded output.

`copy` is now an ordinary identifier, recognized as a capture mode only in capture
clauses. Direct generic `copy(...)` calls, identifier/list uses, and mixed capture
clauses have regression coverage; editor keyword handling follows the lexer.
The complete library test suite passes with and without optimization.

The release sweep also reproduced a native cancellation race: a post-call failure
query could discover cancellation after a successful owned return, discarding the
result before cleanup registration. Failure queries now only read the execution
flag; cancellation is discovered at generated polls and future waits. A runtime
regression checks that separation, and the allocation-census failure suite passes
in both optimization modes. The final release unit rerun passes 328 compiler,
3 host, and 7 runtime tests, with one existing ignored compiler test. Final-source
debug reruns also pass 127 language, 121 ownership, 10 remote-lifetime, 4 Foster
coverage/fixture, and 1 documentation-example tests. The 89-case backend-parity
release sweep passes. The timings
above predate this follow-up correctness change and have not been remeasured.

The full release workspace sweep completed. Its original unit failures are covered
by the clean final unit rerun above. Three integration targets had been built before
the contextual-`copy` change and rejected updated library files read from disk;
rebuilding and rerunning `foster`, `library_contracts`, and `required_type_contracts`
passes all 4, 5, and 6 tests respectively (`target/fixes-final-library-contracts.log`).
Every observed failure has a passing final rerun; the other integration targets and
doctests passed in the workspace sweep.

Reproduce profiling with the [native profiling command](../../docs/benchmarking.md#native-runtime-profiling).
For paired comparisons, preserve the old `*-mode0.exe` files before rebuilding,
then alternate old/new runs and use their `PROFILE.elapsed_ns` entry timings.
