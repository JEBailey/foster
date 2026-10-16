# Runtime and backend correctness

These targets exercise VM/native parity, actor lifetimes, host adapters, buffers,
collection storage, cancellation, and generated native programs. Assertions check
behavior and resource lifetimes rather than elapsed time.

```text
python tools/integration_tests.py --suite runtime --jobs 2
cargo test --release --test backend_parity
```

Run from the repository root. Performance workloads belong in
[benchmarks](../../benchmarks/README.md); see [test registration](../README.md).
