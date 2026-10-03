# Formatter measurements

Measured on Windows on 2026-10-02 using release builds. The before executable
was saved immediately before the formatter changes, after the compiled-library
work. [Raw results](formatter.json) record both executable hashes. These are
individual measurements, not confidence intervals.

| Input | Before | After |
| --- | ---: | ---: |
| 100 inline declarations, CLI | 717 ms | 100 ms |
| 500 inline declarations, CLI | 14,848 ms | 468 ms |
| 1,000 inline declarations, CLI | timed out at 15 s | 944 ms |
| Raylib `src/lib.fos`, 352,113 bytes | CLI timed out at 30 s | LSP response in 10.06 s |

The Raylib comparison uses different entry points and establishes completion,
not an exact speedup. Its final LSP cancellation response took 0.90 ms after
the cancellation notification. Opening and initial diagnostics are excluded
from the formatting interval. The file was never changed on disk. Generated
CLI inputs use `func itemN() -> Int { N }`, one declaration per line; an exit
status indicating required formatting is expected.

The VM previously copied a parent value for every mutation of a projected
field, repeatedly copying growing byte builders. Projected growth now mutates
unique storage directly while retaining snapshot isolation and invalidating
only references into the modified collection. The Foster formatter scans
UTF-8 bytes for ASCII syntax and avoids unnecessary line-ending normalization.
Host builds link the same Foster policy as native code; cross builds retain
the bytecode path. Both execution paths honor LSP cancellation.

Validation passed: 16 Rust formatter tests, six Foster formatter tests, 101 LSP
tests, 22 VM tests, four native-runtime tests, and the agent documentation test.
Formatter tests compare native and bytecode output, including Unicode,
interpolation, comments, CRLF, repeatability, and cancellation followed by a
successful formatting request.

To repeat the LSP measurement:

```text
node benchmarks/formatter.cjs target/release/foster.exe path/to/lib.fos target/formatter-results.json
```

The benchmark starts its own server, waits for diagnostics, requests formatting,
then cancels a second request after 50 ms. Use a sufficiently large input so the
second request remains active when cancellation is sent.
