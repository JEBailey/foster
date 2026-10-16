# C bridge correctness

These targets import headers, generate bindings, compile C bridges, exercise
callbacks and borrowed buffers, and check pointer ownership contracts. Foreign
sources and manifests are shared under `../fixtures/c_bridge/`.

```text
python tools/integration_tests.py --suite cbridge --jobs 2
cargo test --test c_integration
```

Run from the repository root. See [C integration](../../documentation/c-integration.md)
for platform requirements and [test registration](../README.md) for new targets.
