# Component CI selection

`plan.py` selects checks from changed paths. The [suite registry](../test_suites.py) reads the root Cargo
manifest and rejects missing, duplicate, or unregistered integration targets.
Python 3.11+ is required. All selection values come from fixed component names;
changed filenames are never inserted into workflow shell commands.

Preview decisions from the repository root:

```text
python tools/ci/plan.py --changed-file src/lsp/hover.rs
python tools/ci/plan.py --changed-file runtime/src/host.rs
python tools/ci/plan.py --base origin/main
python -m unittest discover -s tools/ci/tests -v
```

| Change | Coverage |
| --- | --- |
| Compiler, executable/ABI contracts, VM, host, runtime, library, root build inputs | All component integration suites, default workspace unit/doc tests, ownership gates, release backend parity |
| Language/ownership contract documentation or shared fixtures | Broad correctness gates |
| Driver/LSP/debugger/documentation adapters | Driver unit/doc tests and tooling integration suite |
| C bridge adapters/generator | Tooling/C bridge integration and release parity |
| A component integration target | That suite, plus its dedicated ownership/parity gate where applicable |
| Optional package | Library integration suite |
| Editor | npm type/grammar and run/debug tests |
| Benchmark/research harness | Syntax checks; Rust benchmark/example compilation when relevant |
| Unknown path, unavailable base, initial push, or manual run | Broad checks |

Independent changes combine their selections. Shared source changes still run the
Linux and Windows correctness matrix. Benchmark fixture changes also run broad
checks because compiler/parity tests consume them. Report-only and README-only
changes avoid rebuilding the language implementation; repository navigation and
test registration are checked on every run.

Both sides of a rename/deletion are considered. CI uses the PR base or push's
previous commit; a missing base falls back to the broad plan. A manual workflow
run also selects all gates. Library documentation generation has its own selection
for compiler/library inputs and documentation adapters.

Timing measurements are deliberately separate from CI pass/fail checks. Use the
[benchmark guide](../../benchmarks/README.md) on a quiet machine for those.
