# C header importer

`cbind` reads a C header and produces typed Foster functions using the Windows x64
C bridge. Header mapping and manifest generation live in `src/bindings.fos`.
`src/bridge.fos` owns reviewed-manifest validation, schema IDs, struct handling,
and generation of both C adapters and Foster wrappers. `foster bridge` embeds
and runs that Foster source, so the command works without a tools checkout.
Its Rust host only decodes JSON, runs the generator, writes artifacts, and invokes
Clang. JSON objects are passed as a schema-neutral tree with hex-encoded UTF-8
keys and values; contract rules are checked in Foster. Duplicate JSON keys are
rejected during decoding rather than silently overwriting ownership declarations.
The host compiles the embedded generator to bytecode once per process and runs it
in the Foster VM. Compilation and generation add build-time overhead; application
calls use the compiled C bridge directly.
Clang handles preprocessing and C parsing. `cbind.ps1` runs Clang, passes its
declarations to Foster, and invokes `foster bridge`. The launcher still uses
PowerShell; `std.process.spawn` now provides the subprocess support needed for a
Foster launcher.

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

- `-AdditionalHeaders`: explicitly include more headers in declaration selection,
  for example a small ownership adapter alongside the original library header.
- `-Sources` and `-Libraries`: C sources or linkable library files to link.
- `-IncludeDirectories`: additional header search directories.
- `-Functions`: exact C function names to import; missing names are errors.
- `-CStringParameters`: explicit borrowed-string contracts such as
  `DrawText:0` (zero-based argument index). Each must identify a
  selected `const char *` parameter. This promises UTF-8 with no pointer retention;
  the importer never assumes that contract merely from the C spelling.
- `-SkipUnsupported`: explicitly allow a partial import, retaining diagnostics.
- `-ManifestOnly`: write the manifest without compiling a DLL.
- `-Foster` and `-Clang`: executable paths; default to commands on PATH.

Only external function declarations belonging to the explicitly requested headers are selected.
Included headers contribute typedef definitions, not additional functions. Macros
and conditional declarations use Clang's preprocessing results. Duplicate function
declarations produce one binding. Typedef chains resolve to supported scalar types:
`_Bool`, signed/unsigned char, short, int, long long, float, double, and void results.
Complete structs containing supported scalars or other supported structs are
imported automatically, including typedef aliases, anonymous typedef structs,
and packed structs. Both arguments and results are supported. A C `Color`
becomes a public `CColor` Foster record with a `copy()` method and public fields.
Ordinary field names are preserved. Foster keywords, `self`, `copy`, and names
starting with `c_` gain a `c_` prefix to avoid name collisions.

The C compiler handles native layout, padding, and calling conventions. Generated
bridges copy each field through checked scalar slots, never reinterpret Foster
memory as a C struct. C compile-time checks verify each declared field type.
Records are limited to 32 nesting levels and 8191 scalar slots.

`long` and plain `char` are intentionally rejected: their C types do not match the
bridge's exact fixed-width function-pointer signatures even when widths agree.

Unannotated pointers, arrays, unions, bitfields, callbacks, variadics,
static/inline functions, old-style prototypes, and unknown types are reported.
By default any unsupported declaration stops the build after writing the report
and draft manifest. Header types cannot establish retention, ownership, destructor,
or error-handling contracts. To add resources or buffers, copy the generated
manifest to a maintained file and supply reviewed operations/resources following
[C integration](../../docs/c-integration.md), then run `foster bridge` on that file.
The importer does not guess these contracts or modify an existing Foster module.

See the [raylib example](../../examples/raylib/README.md) for a working graphics
UI built from the real raylib header, including returned `Color`/`Vector2` values
and by-value `Rectangle` parameters. Resource IDs stored in integer fields still
need library-specific ownership rules; a structurally copyable record does not
prove that duplicating the underlying native resource is valid.

Validation:

```powershell
./target/debug/foster.exe check tools/cbind
./target/debug/foster.exe test tools/cbind
./target/debug/foster.exe fmt tools/cbind --check
cargo test --test c_header_tool
```
