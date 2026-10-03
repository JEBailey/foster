# Reproducible tzdata generation

The optional [`tzdata` package](../../packages/tzdata/README.md) uses IANA **2026d**.
The unmodified source archives in `vendor/` come from:

- https://data.iana.org/time-zones/releases/tzdata2026d.tar.gz
- https://data.iana.org/time-zones/releases/tzcode2026d.tar.gz

The Foster program in `src/` verifies both pinned SHA-256 hashes, validates flat archive
filenames, and extracts the archives with `tar`. It compiles the matching IANA `zic`,
produces ordinary POSIX-time TZif files, validates their structure and POSIX footers,
and emits deterministic ASCII tables into `packages/tzdata/src/data.fos`.
Generation uses no network or installed system time-zone database.

Requirements: Foster, `tar`, and a C compiler. On Windows use Clang with the Visual
Studio C runtime; `windows_getopt.c` supplies command-line parsing for `zic`.
On Unix the default compiler is `cc`. Override it with `--cc PATH`.
The generation tools do not ship in the `.flib` or application executable.

From the repository root (add `.exe` to the generator name on Windows):

```sh
foster build tools/tzdata --native -o target/tzdata-generator
target/tzdata-generator
target/tzdata-generator --check
target/tzdata-generator --reference
target/tzdata-generator --reference --check
foster test tools/tzdata
foster build packages/tzdata --library -o target/tzdata.flib
cargo test --test tzdata
```

`--root PATH` selects the repository root when running elsewhere. `--archives PATH`
and `--work PATH` override the archive and scratch directories. Each run creates a
fresh numbered scratch directory, preventing stale zones without deleting caller
files. Generation publishes only after successful validation; `--check` compares
complete output with the checked-in file. Identifiers are ordered explicitly
by ASCII spelling, independently of host filesystem ordering.

`--reference` refreshes `tests/fixtures/tzdata-offsets.txt`. Foster builds and calls
pinned IANA `localtime.c` through the small `reference.c` offset oracle, which
reads the generated TZif files independently of Foster's parser and tzdata library.
The fixture covers all identifiers at eight instants spanning 1900 through 2400.
Integration tests also check exact transition boundaries and local-time gaps/overlaps.

For an update, vendor both official release archives, change `VERSION`, `DATA_HASH`,
and `CODE_HASH` in `src/main.fos`, regenerate both outputs, review changed rules and
identifiers, update pinned release assertions and counts, then rebuild and test.
Unsupported future footer syntax fails generation rather than discarding recurring rules.

The data and IANA code are public domain, with the archive exceptions described in
[LICENSE](LICENSE). The Foster tools and C shims use Foster's project license.
