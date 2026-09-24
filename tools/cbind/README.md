# C header importer

`cbind` reads a C header and produces typed Foster functions using the Windows x64
C bridge. The mapping and manifest generator is written in Foster in `src/`.
Clang handles preprocessing and C parsing. `cbind.ps1` runs Clang, passes its
declarations to Foster, and invokes `foster bridge`; Foster's process library
currently exposes arguments but cannot launch subprocesses.

From the repository root, with Clang installed:

```powershell
cargo build --bin foster
./tools/cbind/cbind.ps1 -Header ./tests/fixtures/c_bridge/fixture.h `
  -Sources ./tests/fixtures/c_bridge/fixture.c -Functions fixture_add,fixture_float `
  -Output ./target/imported/fixture.dll -Foster ./target/debug/foster.exe
```

This writes `fixture.fos`, `fixture.dll`, `fixture.schema`, and `fixture.bridge.c`.
The generated public functions in this example are `c_fixture_add` and
`c_fixture_float`; they return `Result<..., CError>`. Import the generated `.fos`
module in your project. Bindings contain the absolute DLL path, as other bridge
bindings do; regenerate them when relocating the DLL.

The intermediate `.bindings.json`, `.import.h`, `.declarations.tsv`, `.ast.json`,
and `.unsupported.txt` files make the conversion inspectable. These are generated
files and are replaced on each run, so use a dedicated output basename. Keep the
import shim alongside the manifest for subsequent bridge builds.

Options:

- `-Sources` and `-Libraries`: C sources or linkable library files to link.
- `-IncludeDirectories`: additional header search directories.
- `-Functions`: exact C function names to import; missing names are errors.
- `-SkipUnsupported`: explicitly allow a partial import, retaining diagnostics.
- `-ManifestOnly`: write the manifest without compiling a DLL.
- `-Foster` and `-Clang`: executable paths; default to commands on PATH.

Only external function declarations belonging to the requested header are selected.
Included headers contribute typedef definitions, not additional functions. Macros
and conditional declarations use Clang's preprocessing results. Duplicate function
declarations produce one binding. Typedef chains resolve to supported scalar types:
`_Bool`, signed/unsigned char, short, int, long long, float, double, and void results.
`long` and plain `char` are intentionally rejected: their C types do not match the
bridge's exact fixed-width function-pointer signatures even when widths agree.

Pointers (including `const char *`), arrays, aggregates, callbacks, variadics,
static/inline functions, old-style prototypes, and unknown types are reported.
By default any unsupported declaration stops the build after writing the report
and draft manifest. Header types cannot establish retention, ownership, destructor,
or error-handling contracts. To add resources or buffers, copy the generated
manifest to a maintained file and supply reviewed operations/resources following
[C integration](../../docs/c-integration.md), then run `foster bridge` on that file.
The importer does not guess these contracts or modify an existing Foster module.

Validation:

```powershell
./target/debug/foster.exe check tools/cbind
./target/debug/foster.exe test tools/cbind
./target/debug/foster.exe fmt tools/cbind --check
cargo test --test c_header_tool
```
