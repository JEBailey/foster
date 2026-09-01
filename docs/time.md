# Time in Foster

Foster's time library separates exact time, civil time, offsets, and time-zone rules. The separation
prevents code from silently treating a calendar value as an exact timestamp or using an adjustable
wall clock to measure elapsed work.

The library is split into four modules:

| Module | Main types | Purpose |
| --- | --- | --- |
| `std.time` | `Instant`, `Duration`, `Interval`, `Clock<T>` | Exact points, exact elapsed lengths, and clocks |
| `std.time.civil` | `Date`, `TimeOfDay`, `DateTime`, `Span` | Calendar and clock values without a time zone |
| `std.time.zone` | `Offset`, `OffsetDateTime`, `TimeZone`, `ZonedDateTime` | Mapping between civil values and exact instants |
| `std.time.format` | `FormatError` and parse/format functions | ISO-8601 and RFC-3339-style portable text |

Most programs that use the complete time model start with these imports:

```foster
import core.result
import std.time
import std.time.civil
import std.time.zone
import std.time.format
```

Imports expose public declarations directly. They also bind the final module component, so
`time::now()` and `format::parse_instant(source)` can be used when qualification makes an operation
clearer.

## Choosing a type

Choose a type according to what the value means, not how it will initially be displayed:

| Question | Type |
| --- | --- |
| When did an event occur on the global time line? | `Instant` |
| How much exact time elapsed? | `Duration` |
| What calendar date or local clock time was entered? | `Date`, `TimeOfDay`, or `DateTime` |
| How much calendar time should be added? | `Span` or its alias `Period` |
| What local value is paired with a fixed numeric offset? | `OffsetDateTime` |
| What local value is interpreted using named time-zone rules? | `ZonedDateTime` |
| What was read from an elapsed-time clock? | `MonotonicInstant` |

An `Instant` does not contain year, month, day, clock, offset, or zone fields. A `DateTime` does not
identify an exact instant until an offset or time zone is supplied. Keeping those facts in the types
prevents implicit and environment-dependent conversions.

## Exact durations

`Duration` is a signed, exact amount represented as whole seconds plus a fractional nanosecond
component. It has no month or day component because those units do not have a fixed elapsed length.

```foster
let seconds = Duration.from_seconds(30)
let milliseconds = Duration.from_milliseconds(1250)
let nanoseconds = Duration.from_nanoseconds(42)

let combined = seconds.add(milliseconds)
assert(combined.total_milliseconds() == 31250)
assert(nanoseconds.total_nanoseconds() == 42)
```

Use `subtract`, `negated`, `zero?`, `compare`, and `equal?` for duration arithmetic and comparison.
`Duration.from_parts` validates that its fractional component is between zero and 999,999,999:

```foster
branch Duration.from_parts(12, 500000000) {
    Result.Ok(value) -> assert(value.total_milliseconds() == 12500)
    Result.Error(error) -> assert(false, error.message)
}
```

Negative values use a canonical floor-based representation. For example, negative 1.5 seconds is
stored as minus 2 whole seconds plus 500,000,000 nanoseconds:

```foster
let value = Duration.from_milliseconds(-1500)

assert(value.seconds() == -2)
assert(value.nanosecond() == 500000000)
assert(value.total_milliseconds() == -1500)
```

`total_nanoseconds` can overflow `Int` for sufficiently large durations. `total_milliseconds`
truncates toward negative infinity.

## Instants and exact arithmetic

`Instant` is a precise point on the Unix time line. Unix epoch second zero is
`1970-01-01T00:00:00Z`.

```foster
let epoch = Instant.epoch()
let later = Instant.from_epoch_milliseconds(1750)
let elapsed = epoch.until(later)

assert(epoch.epoch_seconds() == 0)
assert(later.epoch_milliseconds() == 1750)
assert(elapsed.total_milliseconds() == 1750)
```

Time values are immutable. Adding or subtracting returns a new instant:

```foster
let captured = time::now()
let exactly_one_hour_later = captured.add(Duration.from_seconds(60 * 60))
let original_again = exactly_one_hour_later.subtract(Duration.from_seconds(60 * 60))

assert(original_again.equal?(captured))
```

`start.until(end)` returns a signed duration. Reversing the operands reverses the sign.

## Wall time and elapsed time

Foster exposes two host clocks because reading the current civil time and measuring elapsed work
have different correctness requirements.

### `SystemClock`: when did it happen?

`SystemClock.now()` returns a serializable `Instant` from the host wall clock. The `time::now()`
convenience function performs the same operation.

```foster
let clock = SystemClock.new()
let recorded_at = clock.now()
let also_recorded_at = time::now()
```

Use wall-clock instants for persisted event timestamps, logs, protocol timestamps, and comparison
with other Unix timestamps.

Two wall-clock readings can be subtracted:

```foster
let started_at = SystemClock.new().now()
// Work occurs here.
let finished_at = SystemClock.new().now()

let timestamp_difference = started_at.until(finished_at)
```

That result is the difference between the recorded timestamps. It is not guaranteed to be the
physical elapsed duration. A wall clock can move because of clock synchronization, an administrator
change, a virtual-machine restore, or an operating-system correction. The result can therefore be
shorter, longer, or even negative. Comparisons between different machines also include their clock
synchronization error.

### `ContinuousClock`: how long did it take?

`ContinuousClock.now()` returns a `MonotonicInstant`. Readings progress within the host's monotonic
clock domain and are suitable for measuring work and implementing deadlines.

```foster
let measurement_start = ContinuousClock.new().now()
// Work occurs here.
let measurement_end = ContinuousClock.new().now()

let elapsed = measurement_start.until(measurement_end)
assert(elapsed.total_nanoseconds() >= 0)
```

A `MonotonicInstant` has no Unix or calendar meaning. Do not persist it, format it as a timestamp,
send it to another machine, or compare it with a reading from another clock domain. Its `ticks`
value is useful only relative to another reading in the same domain.

Capture both clocks when an operation needs a timestamp and a reliable duration:

```foster
let started_at = SystemClock.new().now()
let measurement_start = ContinuousClock.new().now()

// Work occurs here.

let finished_at = SystemClock.new().now()
let measurement_end = ContinuousClock.new().now()

let recorded_difference = started_at.until(finished_at)
let elapsed = measurement_start.until(measurement_end)
```

The generic structural contract `Clock<T>` allows application and test clocks to supply their own
`now()` implementation. Code can depend on `Clock<Instant>` or `Clock<MonotonicInstant>` instead of
constructing a host clock internally when deterministic time is required.

## Civil dates and times

Civil values describe calendar and clock fields without claiming that they identify a unique point
on the global time line.

Constructors validate their fields and return `Result` where invalid input is possible:

```foster
branch Date.from(2024, 2, 29) {
    Result.Ok(value) -> {
        assert(value.day_of_year() == 60)
        assert(value.month() == 2)
        ()
    }
    Result.Error(error) -> assert(false, error.message)
}

assert(Date.from(2023, 2, 29).error?())
assert(TimeOfDay.from(24, 0, 0, 0).error?())
```

`Date.at(time)` and `DateTime.from(date, time)` combine a date and a clock value:

```foster
let date = Date.from_epoch_day(0)
let time_of_day = TimeOfDay.midnight()
let local = date.at(time_of_day)

assert(local.date().year() == 1970)
assert(local.time().hour() == 0)
```

`Date.from_epoch_day(0)` is `1970-01-01`, but it is still only a civil date. Similarly,
`DateTime.from_epoch_parts` interprets the supplied Unix fields as UTC to construct civil fields; it
returns a `DateTime`, not an `Instant`.

`IsoCalendar` provides proleptic ISO-8601 Gregorian calendar facts:

```foster
let calendar = IsoCalendar.new()

assert(calendar.id() == "iso8601")
assert(calendar.leap_year?(2000))
assert(not calendar.leap_year?(1900))
assert(calendar.days_in_month(2024, 2) == 29)
```

## Calendar spans and overflow

`Span` represents calendar-aware components from years through nanoseconds. `Period` is an alias for
the same type.

```foster
let one_day = Span.from_days(1)
let one_month = Span.from_months(1)
let composite = Span.from(1, 2, 0, 3, 4, 5, 6, 7)

assert(composite.years() == 1)
assert(composite.months() == 2)
assert(composite.days() == 3)
```

Adding months or years can produce a day that is not present in the destination month. The caller
must choose an `Overflow` policy:

```foster
branch Date.from(2024, 1, 31) {
    Result.Ok(january_31) -> {
        branch january_31.add(Span.from_months(1), Overflow.Constrain) {
            Result.Ok(value) -> assert(value.day() == 29)
            Result.Error(error) -> assert(false, error.message)
        }

        assert(january_31.add(Span.from_months(1), Overflow.Reject).error?())
        ()
    }
    Result.Error(error) -> assert(false, error.message)
}
```

`Constrain` selects the final valid day of the destination month. `Reject` returns `CivilError`.
Clock components added to `DateTime` carry into the adjacent civil date:

```foster
let local = DateTime.from(
    Date.from_epoch_day(0),
    TimeOfDay.from_nanosecond_of_day(23 * 60 * 60 * 1000000000)
)

let two_hours = Span.from(0, 0, 0, 0, 2, 0, 0, 0)

branch local.add(two_hours, Overflow.Reject) {
    Result.Ok(value) -> {
        assert(value.date().epoch_day() == 1)
        assert(value.time().hour() == 1)
        ()
    }
    Result.Error(error) -> assert(false, error.message)
}
```

`Date.add` accepts only the date-bearing span components: years, months, weeks, and days. It rejects
a span containing nonzero clock components because a date alone has no clock on which to apply them.

## Exactly 24 hours versus tomorrow

These operations express different intentions:

```foster
let current = time::now()
let exactly_24_hours_later = current.add(Duration.from_seconds(24 * 60 * 60))
```

The result is exactly 86,400 seconds later on the time line.

To request the same local clock time on the next calendar day, first interpret the instant in a time
zone and then add a calendar span:

```foster
let current = ZonedDateTime.from_instant(
    time::now(),
    FixedOffsetZone.utc()
)

let tomorrow = current.add_span(
    Span.from_days(1),
    Overflow.Reject,
    Disambiguation.Compatible
)
```

In a regional zone, a calendar day can correspond to 23, 24, or 25 elapsed hours around offset
transitions. A fixed-offset zone has no transitions, so the two operations happen to have the same
elapsed length there.

## Fixed offsets

`Offset` is a validated displacement east of UTC in seconds. Its magnitude must be less than 24
hours.

```foster
func minus_five_hours() -> Result<Offset, ZoneError> {
    Offset.from_seconds(-5 * 60 * 60)
}
```

`OffsetDateTime` pairs an exact instant with a numeric offset but does not carry regional transition
rules. Converting from an instant is always unambiguous:

```foster
branch Offset.from_seconds(-5 * 60 * 60) {
    Result.Ok(offset_value) -> {
        let value = OffsetDateTime.from_instant(Instant.epoch(), offset_value)

        assert(value.instant().equal?(Instant.epoch()))
        assert(value.local().epoch_seconds() == -5 * 60 * 60)
        ()
    }
    Result.Error(error) -> assert(false, error.message)
}
```

Converting a local value under a validated fixed offset also produces exactly one instant:

```foster
let local = DateTime.from(Date.from_epoch_day(0), TimeOfDay.midnight())

branch Offset.from_seconds(2 * 60 * 60) {
    Result.Ok(offset_value) -> {
        let value = OffsetDateTime.from_local(local, offset_value)
        assert(value.instant().epoch_seconds() == -2 * 60 * 60)
        ()
    }
    Result.Error(error) -> assert(false, error.message)
}
```

`OffsetDateTime.add(duration)` advances the exact instant and preserves the numeric offset.
Offset-date-time equality and ordering compare the underlying instant, not the displayed local
fields.

## Zoned date-times

`TimeZone` is a structural rule-set contract with three operations:

- `id()` returns a stable identifier.
- `offset_at(instant)` returns the offset active at an exact instant.
- `resolve(local)` maps a civil date-time to its possible exact instant or instants.

`FixedOffsetZone` is the built-in implementation. UTC is the simplest example:

```foster
let value = ZonedDateTime.from_instant(
    Instant.epoch(),
    FixedOffsetZone.utc()
)

assert(value.zone_id() == "UTC")
assert(value.offset().seconds() == 0)
assert(value.local().epoch_seconds() == 0)
```

Converting an instant into a zone is always unambiguous. Converting a local date-time into a
regional zone may be unique, ambiguous, or skipped:

| `LocalResolution` case | Meaning |
| --- | --- |
| `Unique(instant)` | Exactly one instant has the local fields |
| `Ambiguous(options)` | An overlap maps the local fields to an earlier and a later instant |
| `Skipped(options)` | A gap contains no instant; neighboring instants are exposed |

`ZonedDateTime.from_local` requires a `Disambiguation` policy:

| Policy | Overlap | Gap |
| --- | --- | --- |
| `Compatible` | Earlier instant | First instant after the gap |
| `Earlier` | Earlier instant | Last instant before the gap |
| `Later` | Later instant | First instant after the gap |
| `Reject` | `ZoneError` | `ZoneError` |

Fixed-offset zones always resolve valid civil values uniquely, but using an explicit policy keeps
calling code compatible with regional zones:

```foster
let local = DateTime.from(Date.from_epoch_day(0), TimeOfDay.midnight())

branch ZonedDateTime.from_local(
    local,
    FixedOffsetZone.utc(),
    Disambiguation.Reject
) {
    Result.Ok(value) -> assert(value.instant().equal?(Instant.epoch()))
    Result.Error(error) -> assert(false, error.message)
}
```

`with_zone(zone)` preserves the instant and changes its local interpretation. `as_offset()` captures
the offset active at that instant and discards the zone's transition rules.

Zoned arithmetic makes the exact/calendar choice visible:

- `add_duration(duration)` advances by an exact elapsed duration and retains the zone.
- `add_span(span, overflow, disambiguation)` performs arithmetic in local civil time and resolves
  the result using the supplied policies.

Equality and ordering of zoned values compare their underlying instants. Two values in different
zones can therefore be equal when they identify the same point on the time line.

## Parsing and formatting

The format module uses portable, locale-independent text. Parsing validates both syntax and field
ranges and returns `Result<value, FormatError>`.

| Function | Accepted input |
| --- | --- |
| `format::parse_date` | `YYYY-MM-DD` |
| `format::parse_time` | `HH:MM:SS` with up to nine fractional digits |
| `format::parse_date_time` | Date and time separated by uppercase `T` |
| `format::parse_offset` | `Z`, signed `HH:MM`, or signed `HH:MM:SS` |
| `format::parse_instant` | Date-time ending in `Z` or a numeric offset |

Formatting functions are `format::date`, `format::time`, `format::date_time`, `format::offset`,
`format::instant`, `format::offset_date_time`, and `format::zoned_date_time`.

Parse and normalize an exact timestamp:

```foster
func normalize_timestamp(source: String) -> Result<String, FormatError> {
    let value = try format::parse_instant(move source)
    Result.Ok(format::instant(move value))
}
```

For example, `1970-01-01T02:00:00+02:00` normalizes to
`1970-01-01T00:00:00Z`. Instant formatting always uses UTC.

Civil parsing does not introduce an offset or zone:

```foster
branch format::parse_date_time("2024-02-29T23:59:58.123400") {
    Result.Ok(value) -> {
        assert(format::date_time(move value) == "2024-02-29T23:59:58.1234")
        ()
    }
    Result.Error(error) -> assert(false, error.message)
}
```

Formatters remove insignificant trailing fractional zeroes. Zero offset is rendered as `Z`, and a
nonzero offset includes seconds only when necessary. Zoned formatting appends the zone identifier:

```foster
let value = ZonedDateTime.from_instant(Instant.epoch(), FixedOffsetZone.utc())
assert(format::zoned_date_time(move value) == "1970-01-01T00:00:00Z[UTC]")
```

The current format module does not provide locale-sensitive presentation. It also does not parse
the bracketed output of `zoned_date_time` or provide a separate `OffsetDateTime` parser; parse an
instant or its civil and offset components and construct the desired typed value explicitly.

## Intervals

`Interval` represents a half-open exact range `[start, end)`. The start is included and the end is
excluded. An end before the start is rejected, while an empty interval with equal boundaries is
valid.

```foster
branch Interval.from(
    Instant.from_epoch_seconds(10),
    Instant.from_epoch_seconds(20)
) {
    Result.Ok(window) -> {
        assert(window.contains?(Instant.from_epoch_seconds(10)))
        assert(window.contains?(Instant.from_epoch_seconds(19)))
        assert(not window.contains?(Instant.from_epoch_seconds(20)))
        assert(window.duration().equal?(Duration.from_seconds(10)))
        ()
    }
    Result.Error(error) -> assert(false, error.message)
}
```

`DateInterval` provides the same half-open model for civil dates. Its `days()` operation counts civil
days rather than elapsed seconds.

## Partial calendar values

`YearMonth` represents a year and month without choosing a day. It can report the number of days in
the month and validate a later day selection:

```foster
branch YearMonth.from(2024, 2) {
    Result.Ok(february) -> {
        assert(february.days() == 29)
        assert(february.on_day(29).success?())
        ()
    }
    Result.Error(error) -> assert(false, error.message)
}
```

`MonthDay` represents a recurring month and day without choosing a year. February 29 is
representable, but placing it in a non-leap year returns an error:

```foster
branch MonthDay.from(2, 29) {
    Result.Ok(leap_day) -> assert(leap_day.in_year(2023).error?())
    Result.Error(error) -> assert(false, error.message)
}
```

## Errors and validation

The module family uses ordinary Foster `Result` values rather than exceptions:

| Error | Produced by |
| --- | --- |
| `TimeError` | Invalid exact parts or interval order |
| `CivilError` | Invalid civil fields or rejected calendar arithmetic |
| `ZoneError` | Invalid offsets, zone lookup, or rejected local resolution |
| `FormatError` | Invalid portable text |

Use `try` when a function returns the same error type:

```foster
func beginning_of_leap_day() -> Result<DateTime, CivilError> {
    let date = try Date.from(2024, 2, 29)
    Result.Ok(date.at(TimeOfDay.midnight()))
}
```

Use `branch` when recovering, translating the error, or handling it locally. Error records expose a
human-readable `message`; civil errors additionally identify the invalid `field` and `value`, format
errors retain the input `value`, and zone errors include the `zone` identifier.

## Current scope and limitations

The current implementation deliberately establishes the type and contract taxonomy before adding
provider data and advanced operations:

- `FixedOffsetZone`, including UTC, is the built-in zone implementation.
- `TimeZoneDatabase` is a structural provider contract, but a versioned IANA database and regional
  identifiers such as `America/New_York` are not supplied yet.
- ISO-8601 Gregorian civil values are implemented; additional calendar providers are not supplied.
- Formatting is portable and locale-independent; reusable format patterns and locale providers are
  roadmap work.
- Calendar-aware `until`/`since`, rounding, balancing, and transition introspection are roadmap work.
- Civil clock values reject leap seconds; seconds range from 0 through 59.
- The date parser currently accepts the exact four-digit `YYYY-MM-DD` shape. Extended years emitted
  by the date formatter are not yet accepted by `parse_date`.

Until regional zone data is available, the API can express and test gaps and overlaps through the
`TimeZone` contract, but applications cannot look up real-world regional transition histories from
the standard library.

## Common recipes

| Goal | Operation |
| --- | --- |
| Current timestamp | `time::now()` |
| Timestamp a fixed elapsed time later | `instant.add(duration)` |
| Difference between recorded timestamps | `start.until(end)` on `Instant` |
| Reliably measure elapsed work | Two `ContinuousClock` readings and `start.until(end)` |
| Construct user-entered local fields | `Date`, `TimeOfDay`, and `DateTime` |
| Move to the next calendar day | `zoned.add_span(Span.from_days(1), ...)` |
| Move exactly 24 hours | `instant.add(Duration.from_seconds(86400))` |
| Attach a numeric offset | `OffsetDateTime.from_local(local, offset)` |
| Interpret an instant in a zone | `ZonedDateTime.from_instant(instant, zone)` |
| Resolve local fields in a zone | `ZonedDateTime.from_local(local, zone, policy)` |
| Parse a portable timestamp | `format::parse_instant(source)` |
| Format an instant in UTC | `format::instant(value)` |
