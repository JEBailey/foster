# Optional IANA time-zone database

`tzdata` is a separate Foster package implementing `std.time.zone.TimeZoneDatabase`.
It pins **IANA 2026d**, with **597 identifiers and aliases** and **343 distinct rule sets**.
It is not imported or embedded by the standard library. UTC and fixed-offset applications
do not need this dependency.

Build the distributable library from the repository root:

```sh
foster build packages/tzdata --library -o target/tzdata.flib
```

Copy that artifact into an application's `vendor` directory and declare:

```toml
[dependencies]
tzdata = { path = "vendor/tzdata.flib" }
```

```foster
import core.result
import std.time
import std.time.zone
import tzdata

func main() -> Int {
    let database = IanaDatabase.new()
    branch database.find("America/New_York") {
        Result.Ok(rules) -> rules.offset_at(Instant.from_epoch_seconds(1784073600)).seconds()
        Result.Error(_) -> 0
    }
}
```

The example returns `-14400`: four hours west of UTC at that instant. `find` is case-sensitive;
unknown names return `ZoneError`. Aliases such as `US/Eastern` share the same rules and retain
their requested spelling in `id()`. `version()` reports the dataset release and `identifiers()`
lists all supported names.

The `.flib` contains compiled Foster code and the complete dataset as constants. Consumers need
neither its source nor external data files. VM bytecode and native executables embed the linked
data. The runtime lookup accepts arbitrary identifiers, so a linked database includes the whole
dataset; this implementation does not prune it to the names appearing in source code. Merely
using `std.time` or `FixedOffsetZone` does not pull it in.

## Time semantics

Historical transitions use binary search and second-precision offsets, including local mean time.
Beyond the explicit transition table, the provider evaluates the release's recurring POSIX rules
using the Gregorian 400-year cycle. This includes southern-hemisphere seasons, negative daylight
saving, and non-hour changes. It does not freeze DST at the final stored transition or invent
future government decisions.

`resolve` implements the existing `LocalResolution` contract:

- A unique time produces one instant and preserves its nanoseconds.
- An overlap produces the earlier and later instants, preserving nanoseconds in both.
- A skipped time produces the final nanosecond before the transition and the first instant after it.
  These are the gap edges specified by Foster's current contract, not a shift of the supplied local
  clock time by the gap duration.

The data follows IANA's default main geographic files plus `etcetera` and `backward`. Optional
`backzone` history is excluded, as are leap-second-adjusted `right/` zones. Times use POSIX seconds
like `std.time.Instant`. Pre-1970 history therefore has IANA's main-dataset coverage and limitations.
Zone abbreviations, transition enumeration, and automatic operating-system zone discovery are not
part of this provider's API.

## Updating and testing

The [offline generator](../../tools/tzdata/README.md) records the source release and SHA-256 hashes.
Updates require an intentional regeneration and `.flib` rebuild; neither execution nor ordinary
application builds fetch data. Keep the artifact version fixed for reproducible results and update
it deliberately when governments change their rules. `.flib` compiler-version compatibility rules
still apply; the IANA release number is separate from the Foster artifact format version.

```sh
cargo test --test tzdata
```

The integration test builds the library and consumes only its artifact. It covers VM/native
execution with optimization on/off, nanoseconds, alias and error behavior, historical offsets,
future recurrence, overlaps, half-hour changes, and skipped days. An independent Python `zoneinfo`
reader generates reference offsets from the same pinned TZif files: all identifiers are checked
at eight historical/future instants in native mode, with a representative subset in the VM.
