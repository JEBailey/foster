# Unicode data

Foster pins classification and default casing to Unicode **17.0.0**. Both the VM
and native backend execute `library/core/unicode.fos` using the generated Foster
tables in `library/core/unicode/data.fos`. No Unicode intrinsics or Rust algorithms
are required. The offline generator is Foster source in `src/` and imports
`std.crypto.sha256` from the standard library. No Python or external hashing
command is needed.

The files in `17.0.0/` are unmodified downloads from
[the official Unicode Character Database](https://www.unicode.org/Public/17.0.0/ucd/).
They and the derived tables are covered by [Unicode License V3](LICENSE.txt).
Generated table comments record SHA-256 hashes of the inputs.

From the repository root, build and run the generator with Foster. Native execution
is recommended for processing and hashing the full Unicode database:

```
foster build tools/unicode --native -o target/unicode-generator.exe
./target/unicode-generator.exe
./target/unicode-generator.exe --check
```

The native build uses Foster's usual native toolchain. To use the VM instead,
run `foster run tools/unicode` or `foster run tools/unicode -- --check`; it is slower
for this workload. `--root PATH` selects a different repository directory, so the
executable can be invoked from any working directory. No network access is used.

`--check` compares both generated files without writing them and fails if either
is stale. Generation validates the input records and computes both outputs before
writing. Tests cover record parsing, collection updates, range encoding, mapping
ordering and padding. SHA-256 known-answer tests live with the standard-library module:

```
foster test tools/unicode
foster test tools/unicode --no-optimize
foster test library/std/crypto/sha256.fos
```

The SHA-256 functions and constants follow
[RFC 6234, section 5](https://www.rfc-editor.org/rfc/rfc6234#section-5).

When upgrading, vendor the new release, update the generator's version and paths,
review new conditional SpecialCasing rules, regenerate, and run Unicode library and
backend parity tests. Locale-specific mappings are excluded. The default
Final_Sigma rule is implemented in Foster with Case_Ignorable context.
Case folding uses the C and F mappings, without normalization.

Tables contain fixed-width ASCII hexadecimal records. Category/property records
contain inclusive start, end, and value fields (6+6+2 characters). Simple mappings
contain source and target (6+6). Full mappings contain source, target count, and
three padded target slots (6+1+18). Foster reads these fields directly from the
literal's UTF-8 bytes and binary-searches the sorted records without constructing
large lists or decoding the entire table on every lookup.
