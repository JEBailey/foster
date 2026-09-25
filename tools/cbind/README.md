# Standalone C header importer

`cbind` is a Foster program. It invokes Clang, parses Clang's JSON AST with
`std.json`, maps C declarations, validates reviewed contracts, and generates and
builds C adapters and Foster bindings. Its native executable needs Clang and a
Windows x64 C linking toolchain at runtime; it needs neither PowerShell nor an
installed Foster compiler or Rust toolchain.

Build once from the repository root:

```powershell
cargo run --bin foster -- build tools/cbind --native --output target/cbind.exe
```

Then run the executable from any directory:

```powershell
./target/cbind.exe --header ./tests/fixtures/c_bridge/fixture.h `
  --source ./tests/fixtures/c_bridge/fixture.c `
  --function fixture_add --function fixture_float `
  --output ./target/imported/fixture.dll
```

For development, the same program runs with `foster run tools/cbind -- ...`.
`cbind --help` prints the command syntax. Options taking lists are repeated,
so paths containing spaces stay single arguments; comma-separated lists are not
interpreted.

The output includes `.fos`, `.dll`, `.schema`, and `.bridge.c` files. The
`.bindings.json`, `.import.h`, `.declarations.tsv`, `.ast.json`, and
`.unsupported.txt` intermediates make conversion inspectable. Use a dedicated
output basename: these generated files are replaced. The generated module
embeds a *module path* naming the bridge for the runtime: by default the
`--output` value exactly as given, normalized to forward slashes, so relative
outputs stay portable. Relative names resolve at run time against
`FOSTER_BRIDGE_DIR`, the current directory, and the executable directory,
trying the full name before the bare file name; absolute names load as given.
Pass `--module-path NAME` to control the embedded name explicitly (for
example, a path relative to the application root), and regenerate when the
bridge moves to a location the resolver cannot reach.
Use a distinct bridge basename such as `raylib_bridge.dll`; dependent DLLs must
retain their original names beside the bridge.

Without `--function`, the tool attempts the entire selected header: functions,
value records, enum values, and active object-like macro constants. The report
also accounts for unsupported type declarations, global variables, and macros.
`--function` requests a focused function import and omits constant discovery.
It remains an error to silently deliver an incomplete library: use
`--skip-unsupported` explicitly while working through the report.

Enum members, including aliases and flags, become integer constants such as
`C_KEY_SPACE`. Primitive macro values become `pub const` declarations, for example
`C_RAYLIB_VERSION` and `C_PI`. Value-record macros become factories such as
`C_RED() -> CColor`, since Foster constant initializers do not support records.
The factories construct Foster values and do not call the DLL. Unsigned 64-bit
values use Foster's signed Int bit representation, as do bridge scalar slots.
The minimum Int value also uses a factory: Foster cannot spell its magnitude
as an integer literal in a constant initializer.

Clang evaluates C expressions in their actual target types; the tool does not
translate C expression syntax into Foster. It discovers active macros through
preprocessing, probes static initializers, and builds and runs a small native
constant exporter. This also happens with `--manifest-only`, which skips the
library build, not constant extraction. The `.preprocessed.h`, `.constants.c`,
and `.constants.exe` files expose this stage (the last successful batch when
unsupported macros require splitting). Function-like macros, nonconstant
expressions, non-finite numbers, and unsupported constant types are reported.
Empty preprocessing markers, including header guards, do not declare values.

Parameter names and Clang-associated documentation comments are retained in the
manifest and generated Foster source. Names that collide with Foster keywords,
imported modules, or generated locals are disambiguated. Record field comments
appear in the record documentation, because Foster does not accept field doc
comments. These comments are descriptive; ownership contracts remain explicit.

Options:

- `--header FILE`: a header to import; repeat to include ownership adapters.
- `--source FILE`, `--library FILE`, `--include DIRECTORY`: sources, linkable
  libraries, and header search paths. Repeat for multiple values.
- `--function NAME`: select an exact C function; missing names are errors.
- `--c-string NAME:INDEX`: explicitly promise a selected `const char *` parameter
  is borrowed UTF-8 without retention. Indexes are zero-based.
- `--contracts FILE.json`: merge reviewed `operations`, `resources`, and `exclude`
  lists into header discovery. Referenced functions must exist in the selection.
- `--skip-unsupported`: allow a partial import and retain its diagnostics.
- `--manifest-only`: write the draft manifest without compiling a DLL.
- `--clang EXE`: compiler executable; defaults to `clang` on PATH.
- `--output FILE.dll`: required output basename.
- `--module-path NAME`: name embedded in the generated module; defaults to
  the `--output` value normalized to forward slashes.

Build a reviewed manifest without rediscovering headers:

```powershell
./target/cbind.exe --manifest ./tests/fixtures/c_bridge/bindings.json `
  --output ./target/imported/reviewed.dll
```

Paths inside reviewed manifests resolve relative to the manifest's directory.
`foster bridge` remains a compiler command that dispatches to the same Foster
implementation; its Rust adapter only embeds and invokes the Foster program.
No C binding policy, JSON decoder, artifact writing, or Clang orchestration is
implemented in that adapter.

The tool captures up to 64 MiB per subprocess output stream and accepts JSON
nesting up to 256 containers. Exceeding either limit is an error. It launches
executables directly with separate arguments, without a command shell.

Only external function declarations belonging to the explicitly requested headers are selected.
Included headers contribute typedef definitions, not additional functions. Macros
and conditional declarations use Clang's preprocessing results. Duplicate function
declarations produce one binding. Typedef chains resolve to supported scalar types:
`_Bool`, plain char, signed/unsigned char, short, int, long, long long, float,
double, and void results (including unsigned integer variants).
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

Fixed-size array fields become `List<T>`, including nested arrays and arrays of
value records. Each dimension must have exactly its declared length when passed
to C; mismatches return `CError` before invoking the C function. Returned arrays
are owned copies, and record `copy()` methods copy every element. Plain `char`
array elements use byte values 0–255, preserving embedded NULs without assuming
text encoding. C compile-time checks verify element types and every dimension.

Named enums and enum typedefs generate transparent aliases such as
`pub type CKeyboardKey = Int`. Enum values remain `C_KEY_*` constants. Enum
parameters and fields preserve the actual C enum type in the bridge and reject
values outside its integer representation. Unnamed numeric values and combined
flags are allowed; the binding does not invent a closed Foster enum.

Plain `char`, `long`, and `unsigned long` retain their exact C types in bridge
signatures and fields, rather than substituting same-width integer types. All
three use Foster `Int`: char preserves byte values 0–255, and long inputs are
checked against the C target's limits (32-bit long on Windows x64). The same
rules apply to constants, array fields, reviewed output/buffer parameters, and
callbacks. No additional Foster primitive types are needed. Extended types such
as `long double`, complex numbers, and 128-bit integers remain unsupported.

Unannotated pointers, flexible or zero-length arrays, unions, bitfields, unreviewed callbacks,
variadics, static/inline functions, old-style prototypes, and unknown types are reported.
By default any unsupported declaration stops the build after writing the report
and draft manifest. Header types cannot establish retention, ownership, destructor,
or error-handling contracts. Add reviewed operations/resources/callbacks with `--contracts`
following [C integration](../../docs/c-integration.md), or build a maintained
manifest directly. Resource contracts remove their copyable value records,
dependent records, and unreviewed operations using those records. Destructors and
validity functions are handled by the owner rather than exposed as raw operations.
The [foster-raylib contracts](../../../foster-raylib/contracts.json) (sibling repository) cover file strings,
binary data, and owned Images, including pointer-based image mutation.
The importer does not guess these contracts or modify an existing Foster module.

Callback/context pairs can be reviewed with `callbacks` definitions and callback
operation parameters. Add `c_type` for named callback typedefs so their report
entries are resolved after C type validation. For a complete executable fixture:

```powershell
./target/cbind.exe --header ./tests/fixtures/c_bridge/callbacks.h `
  --source ./tests/fixtures/c_bridge/callbacks.c `
  --contracts ./tests/fixtures/c_bridge/callbacks.contracts.json `
  --output ./target/imported/callbacks.dll
```

The generated methods support borrowed synchronous handlers, owned registrations,
and explicitly polled background notifications; see
[callback contracts](../../docs/c-integration.md#callbacks) for lifetime and signature limits.

See the [foster-raylib repository](../../../foster-raylib/README.md) (sibling of this one) for a working graphics
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
