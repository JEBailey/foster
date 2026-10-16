# Executable representation and contracts

`foster-bytecode` owns instructions, executable metadata, serialized formats,
intrinsic and native ABI contracts, shared SSA/flow analysis, linking, and
validation. It has no production compiler dependency. Both compiler lowering and
decoded executables use its contracts.

From the repository root:

```text
cargo test -p foster-bytecode --lib
cargo test --test backend_parity
```

Changes here can affect both backends and require broad CI coverage. See
[workspace architecture](../documentation/workspace.md), [VM](../documentation/vm.md),
and the [native backend](../documentation/native.md).
