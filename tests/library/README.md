# Library and package correctness

These targets run the embedded library, portable Foster suites, algorithms,
public API contracts, JSON, and optional-package consumers. API-local tests stay
beside their `.fos` implementation under `library/`.

```text
python tools/integration_tests.py --suite library --jobs 2
cargo test --test tzdata
```

Run from the repository root. See [test registration](../README.md) and the
[library guide](../../library/README.md).
