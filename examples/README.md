# Foster examples

`live_inventory_pipeline.foster` is the flagship end-to-end example. It models
a concurrent inventory audit with owned remote actors, futures, borrowed remote
arguments, a persistent read-only loan, atomic owner mutation, records, and
ownership effects. It uses normal Foster source with inferred effects; compiler
documentation, core interfaces, and focused tests show explicit bracketed contracts.

Run it with:

```console
cargo run --bin foster -- run examples/live_inventory_pipeline.foster
```

The program deterministically returns `1242`:

- `1000` for one healthy audit
- `200` for two alerting audits
- `30` weighted shortage points
- `12` items observed through the live remote inventory view

The smaller programs under `pima/` focus on individual language features.

`type_composition.foster` demonstrates declaration-side composition, intersection parameters, and
static duck typing without a wrapper or runtime conversion:

```console
cargo run --bin foster -- run examples/type_composition.foster
```
