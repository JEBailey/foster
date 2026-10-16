# Optional Foster packages

Packages here are independently imported by consumers and are not automatically
embedded in the core library. Each package owns its `foster.toml`, source, tests,
and usage documentation.

| Package | Purpose | Integration target |
| --- | --- | --- |
| [tzdata](tzdata/README.md) | Optional IANA timezone database | `cargo test --test tzdata` |

Use the package README for generation and artifact requirements. Add consumer
integration tests under `tests/library/` when adding another package; register
their Cargo targets as described in the [test guide](../tests/README.md).
