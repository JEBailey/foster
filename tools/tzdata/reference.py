"""Regenerate conformance cases using Python's independent TZif reader.

Run generate.py first. The exact same vendored IANA files are used; the host's
installed time-zone database is never consulted.
"""
import argparse
import datetime as dt
from zoneinfo import ZoneInfo
from generate import ROOT, VERSION

DATES = [(1900, 1, 1), (1970, 1, 1), (2026, 1, 15), (2026, 7, 15), (2050, 1, 15), (2050, 7, 15), (2400, 1, 15), (2400, 7, 15)]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    directory = ROOT / "target/tzdata-generator/zones"
    records = []
    for path in sorted((p for p in directory.rglob("*") if p.is_file()), key=lambda p: p.relative_to(directory).as_posix()):
        name = path.relative_to(directory).as_posix()
        with path.open("rb") as stream:
            zone = ZoneInfo.from_file(stream, key=name)
        offsets = [int(dt.datetime(*date, tzinfo=dt.timezone.utc).astimezone(zone).utcoffset().total_seconds()) for date in DATES]
        records.append(f"{name:<40}" + "".join(f"{offset:+07d}" for offset in offsets))
    path = ROOT / "tests/fixtures/tzdata-offsets.txt"
    assert records, "run generate.py before generating reference cases"
    output = "\n".join(records) + "\n"
    if args.check:
        assert path.read_text(encoding="ascii") == output, "reference offsets are stale"
    else:
        path.write_text(output, encoding="ascii", newline="\n")
    print(VERSION, len(records), "zones; UTC seconds:", [int(dt.datetime(*d, tzinfo=dt.timezone.utc).timestamp()) for d in DATES])
    for name, date in [
        ("America/New_York", (2024, 3, 10, 2, 30)),
        ("America/New_York", (2024, 11, 3, 1, 30)),
        ("Australia/Lord_Howe", (2024, 10, 6, 2, 15)),
        ("Australia/Lord_Howe", (2024, 4, 7, 1, 45)),
        ("Pacific/Apia", (2011, 12, 30, 12, 0)),
        ("Asia/Kathmandu", (1986, 1, 1, 0, 5)),
        ("Europe/Dublin", (2024, 10, 27, 1, 30)),
        ("America/New_York", (2500, 3, 14, 2, 30)),
    ]:
        with (directory / name).open("rb") as stream:
            zone = ZoneInfo.from_file(stream)
        wall = dt.datetime(*date)
        values = [int(wall.replace(tzinfo=zone, fold=i).timestamp()) for i in (0, 1)]
        print(name, date, "wall", int(wall.replace(tzinfo=dt.timezone.utc).timestamp()), "candidates", values)


if __name__ == "__main__":
    main()
