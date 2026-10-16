# Language correctness

These Rust targets check accepted/rejected source, diagnostics, types, effects,
ownership, control flow, and semantic-specification witnesses. Shared inputs live
under `../fixtures/`; portable Foster tests live under `../foster/`.

```text
python tools/integration_tests.py --suite language --jobs 2
cargo test --test ownership_soundness
```

Run from the repository root. Add targets through the [test registration guide](../README.md).
