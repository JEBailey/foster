# Reproducible tzdata generation

The optional [`tzdata` package](../../packages/tzdata/README.md) uses IANA **2026d**.
The unmodified source archives in `vendor/` come from:

- https://data.iana.org/time-zones/releases/tzdata2026d.tar.gz
- https://data.iana.org/time-zones/releases/tzcode2026d.tar.gz

`generate.py` verifies their pinned SHA-256 hashes before extracting them with Python's safe data
filter. It compiles the matching IANA `zic`, produces ordinary POSIX-time TZif files, validates their
structure and POSIX footers, and emits deterministic ASCII tables into
`packages/tzdata/src/data.fos`. No network or installed system time-zone database is used.

Requirements: Python 3.12+ and a C compiler. On Windows use Clang with the Visual Studio C runtime;
the included `windows_getopt.c` supplies command-line parsing for `zic` only. On Unix the default
compiler is `cc`. Override it with `--cc PATH`. Neither Python nor `zic` ships in the `.flib` or
application executable.

```sh
python tools/tzdata/generate.py
python tools/tzdata/generate.py --check
python tools/tzdata/reference.py
python tools/tzdata/reference.py --check
foster build packages/tzdata --library -o target/tzdata.flib
cargo test --test tzdata
```

`--archives PATH` and `--work PATH` override the archive and scratch directories. Generation writes
only after successful validation; `--check` compares the generated table with the checked-in file.
The scratch zone directory is recreated to prevent stale zones surviving an update. The generator
orders identifiers by ASCII spelling explicitly, independently of host filesystem ordering.

`reference.py` uses Python's independent `zoneinfo` TZif reader with the generated files, never the
host database, to refresh `tests/fixtures/tzdata-offsets.txt`. It covers all identifiers at eight
instants spanning 1900 through 2400. Integration tests also check exact transition boundaries and
local-time gaps/overlaps separately.

For an update, vendor both official release archives, change `VERSION` and `HASHES` in `generate.py`,
regenerate the tables and references, review changed rules and identifiers, update pinned release
assertions and counts, then rebuild and test. An unsupported future footer syntax fails generation
rather than silently discarding recurring rules.

The data and `zic` are public domain, with the archive exceptions described in [LICENSE](LICENSE).
The Foster implementation, Python tools, and Windows shim use Foster's project license.
