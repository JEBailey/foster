# Foster virtual machine

`foster-vm` executes verified bytecode and owns interpreter state, runtime values,
builtin handlers, and VM remote execution. It depends on
[bytecode contracts](../bytecode/README.md) and [host services](../host/README.md).
The driver combines it with compilation and project discovery.

From the repository root:

```text
cargo test -p foster-vm --lib
python tools/integration_tests.py --suite runtime library --jobs 2
cargo test --test backend_parity
```

See the [VM design](../documentation/vm.md). Timing workloads live in
[benchmarks](../benchmarks/README.md); correctness tests do not assert timing thresholds.
