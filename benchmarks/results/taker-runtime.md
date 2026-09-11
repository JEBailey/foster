# Java Taker versus Foster Taker runtime

This is the baseline before the native callable fixes. See the [follow-up native comparison](taker-runtime-native.md) for measurements after those fixes.

Measured 2026-09-10T23:43:59.560Z on AMD Ryzen 5 4500 6-Core Processor (win32). Foster compiler commit cc0d7256711c632bdb45c9ddb3fd58b8e75d0fb0, release executable SHA-256 b02cdb712512b0f5cb3cd97bb8abf5ce66d9b479da9fb08bb80dfb8beb805b87.

Java Taker was substantially faster than the current Foster VM on these four fixed-input parser microbenchmarks. These results include the Java JIT and Foster VM execution models and must not be generalized to native Foster performance or full application throughput.

| Operation | Java ns/parse | Foster VM ns/parse | VM / Java |
| --- | ---: | ---: | ---: |
| Match 16-character literal | 23.1 | 324633.6 | 14072× |
| Parse 9-digit signed long | 51.7 | 521042.2 | 10071× |
| Scan 256 ASCII letters | 579.4 | 5563496.9 | 9601× |
| Parse integer with whitespace | 112.1 | 798185.2 | 7119× |

Values are the median of three process medians; each process contributes five measured batches. Ranges below are the minimum and maximum process medians, not confidence intervals.

| Operation | Java range (ns/parse) | Foster VM range (ns/parse) |
| --- | ---: | ---: |
| Match 16-character literal | 22.9–23.6 | 322945.3–335757.4 |
| Parse 9-digit signed long | 48.6–52.4 | 517445.7–528285.9 |
| Scan 256 ASCII letters | 577.0–586.0 | 5359065.6–5951525.0 |
| Parse integer with whitespace | 106.7–112.5 | 770449.6–908953.1 |

## Method and scope

- Compiled current Java library sources with `javac --release 21`. Java ran with `-Xms256m -Xmx256m`, two seconds of JIT warmup per workload, then batch calibration and five timed batches. No Maven dependency downloads were needed.
- Built Foster once into optimized `.fbc` bytecode and ran it with the release VM. Parser construction happens before calibration and measurement. Two untimed batches follow calibration.
- Both implementations calibrate batch size to at least 100 ms. Timing uses each runtime’s monotonic clock, excluding startup, compiler work, output, and checksum validation.
- Every parse uses `parseAll` / `parse_all`, requires full input consumption, and contributes its returned value or string length to a checked checksum. Java also stores the checksum in a volatile sink. Parser instances and input text are reused across iterations.
- Three fresh processes per backend; backend order alternates across repetitions. Java and Foster never run measurements simultaneously.
- Inputs are identical ASCII strings. The scan predicate is the same explicit `a`–`z` comparison, avoiding Unicode category/version differences. These microbenchmarks cover successful fixed inputs, not backtracking failures, memoization, large grammars, or changing production input distributions. JVM inlining and allocation elimination remain enabled.
- This is a custom cross-language harness, not JMH. The very small Java timings are workload-specific; the broad gap does not imply one universal speed ratio for the libraries.

## Native Foster

The same workload was also submitted to native compilation. It failed before producing an executable:

```text
error: native function `take_while` returns an erased callable value
```

There is no native Foster runtime measurement in this report. The VM results should not be presented as native performance. Resolving the erased-callable compilation limitation is necessary before comparing the corresponding native workload.

## Reproduction

Run `node benchmarks/taker_runtime.cjs` on Windows with Java 21+, a release Foster compiler, and the sibling `taker/taker` and `taker_foster` repositories.

- [Runner](../taker_runtime.cjs)
- [Foster workload](../taker/main.fos)
- [Java workload](../taker/TakerSpeed.java)
- [Raw samples](taker-runtime.json)

Java runtime:

```text
java version "21" 2023-09-19 LTS
Java(TM) SE Runtime Environment (build 21+35-LTS-2513)
Java HotSpot(TM) 64-Bit Server VM (build 21+35-LTS-2513, mixed mode, sharing)
```
