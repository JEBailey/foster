# Failure-state caching measurements (2026-10-16)

These investigations led to the scalar/failure-cache changes in commit `83689b8`.
Reports retain their original local artifact names; build products and full logs
remain local under `target/`. Raw timing samples are saved here for comparison.

| Question | Report | Samples |
| --- | --- | --- |
| Can a frame reuse its execution-state slot? | [Frame-local cache](frame-report.txt) | [Lookup](frame-micro-results.json), [Doom](frame-doom-results.json) |
| Should single-check functions avoid setup? | [Selective analysis](selective-report.txt) | [Doom](selective-doom-results.json), [classification](selective-classification.json) |
| Does actor lookup still regress? | [Actor follow-up](actor-report.txt) | [Coroutine lookup](actor-micro-results.json) |

Repeated cached actor reads measured about 1.86 ns/check versus 8.57 ns for the
original lookup. Current direct lookup measured 8.55 ns; acquiring a handle for
only one read measured 9.38 ns. The compiler avoids that setup when it can prove
at most one check executes. A potentially repeated path can still execute just
once in practice. These are lookup measurements, not actor message throughput.

Selective versus unconditional frame caching was roughly neutral in Doom: a
0.48% lower median of layout medians, with individual layouts varying in both
directions. The earlier frame-cache change and selective follow-up have different
baselines. Read each report's timing boundaries and limitations before comparing.
