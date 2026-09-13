"""Build the optional Foster tzdata library's tables from pinned IANA sources.

Python 3.12+ and a C compiler are build-time tools only. No host time-zone
database, Python package, or network connection is used during generation.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess
import tarfile

ROOT = Path(__file__).resolve().parents[2]
VERSION = "2026d"
HASHES = {
    "tzdata": "0cb2aa8e333c3dc049badc42a0c61f21987b8cd44e107fa900bad764aacc7767",
    "tzcode": "2f5c9f7fe29e6b8cb863583667884b8ce17b0a485355a054b591c6bdfcd81791",
}
SOURCES = "africa antarctica asia australasia europe northamerica southamerica etcetera backward".split()


def tzif(blob: bytes):
    def block(position, width):
        assert blob[position:position + 4] == b"TZif"
        gmt, std, leaps, count, types, chars = struct.unpack_from(">6I", blob, position + 20)
        position += 44
        times = struct.unpack_from(f">{count}{'q' if width == 8 else 'i'}", blob, position)
        position += count * width
        indices = blob[position:position + count]
        position += count
        offsets = [struct.unpack_from(">iBB", blob, position + 6 * n)[0] for n in range(types)]
        position += 6 * types + chars + leaps * (width + 4) + std + gmt
        assert leaps == 0, "Foster uses POSIX seconds, not leap-second time"
        return position, offsets[0], list(zip(times, (offsets[i] for i in indices)))
    position, _, _ = block(0, 4)
    assert blob[4:5] in (b"2", b"3", b"4")
    position, initial, transitions = block(position, 8)
    assert blob[position:position + 1] == b"\n" and blob[-1:] == b"\n"
    return initial, transitions, blob[position + 1:-1].decode("ascii")


def seconds(text):
    sign = -1 if text.startswith("-") else 1
    parts = [int(p) for p in text.lstrip("+-").split(":")]
    assert len(parts) <= 3 and all(0 <= p < 60 for p in parts[1:])
    return sign * sum(p * scale for p, scale in zip(parts, (3600, 60, 1)))


def rule(text):
    date, _, clock = text.partition("/")
    at = seconds(clock) if clock else 7200
    if date.startswith("M"):
        month, week, day = map(int, date[1:].split("."))
        assert 1 <= month <= 12 and 1 <= week <= 5 and 0 <= day <= 6
        return [1, month, week, day, at]
    if date.startswith("J"):
        day = int(date[1:])
        assert 1 <= day <= 365
        return [2, day, 0, 0, at]
    day = int(date)
    assert 0 <= day <= 365
    return [3, day, 0, 0, at]


def footer(text, final):
    if not text:
        return [final, final] + [0] * 10
    name = r"(?:[A-Za-z]{3,}|<[^>]+>)"
    offset = r"[+-]?\d+(?::\d+(?::\d+)?)?"
    match = re.fullmatch(rf"{name}({offset})(?:{name}({offset})?,([^,]+),([^,]+))?", text)
    if not match:
        raise ValueError(f"unsupported zic footer: {text!r}")
    standard = -seconds(match[1])
    if match[3] is None:
        return [standard, standard] + [0] * 10
    daylight = -seconds(match[2]) if match[2] else standard + 3600
    return [standard, daylight] + rule(match[3]) + rule(match[4])


def number(value, width):
    result = f"{value:+0{width}d}"
    assert len(result) == width
    return result


def tables(directory):
    blobs = {}
    names = []
    for path in sorted((p for p in directory.rglob("*") if p.is_file()), key=lambda p: p.relative_to(directory).as_posix()):
        name = path.relative_to(directory).as_posix()
        initial, transitions, tail = tzif(path.read_bytes())
        # zic may emit a sentinel at INT64_MIN; it denotes the initial state.
        if transitions and transitions[0][0] == -(1 << 63):
            _, initial = transitions.pop(0)
        future = footer(tail, transitions[-1][1] if transitions else initial)
        assert all(-86400 < offset < 86400 for offset in [initial, future[0], future[1]] + [o for _, o in transitions])
        payload = "".join(number(v, 7) for v in [initial] + future)
        assert len(payload) == 91
        payload += "".join(number(t, 20) + number(o, 7) for t, o in transitions)
        index = blobs.setdefault(payload, len(blobs))
        assert len(name) <= 40 and index < 10000
        names.append(f"{name:<40}{index:04d}")
    header = f"//! Generated from IANA {VERSION}; do not edit. See tools/tzdata/generate.py.\n"
    output = header + "\n/// Returns the pinned IANA release.\npub func version() -> String { " + json.dumps(VERSION) + " }\n"
    output += "\n/// Returns sorted, fixed-width identifier/index records.\npub func names() -> String { " + json.dumps("".join(names)) + " }\n"
    position = 0
    positions = []
    for payload in blobs:
        assert position < 10_000_000 and len(payload) < 10_000_000
        positions.append(f"{position:07d}{len(payload):07d}")
        position += len(payload)
    output += "\n/// Returns fixed-width offset/length pairs for the rule sets.\npub func positions() -> String { " + json.dumps("".join(positions)) + " }\n"
    output += "\n/// Returns the contiguous signed decimal headers and transition records.\npub func rules() -> String { " + json.dumps("".join(blobs)) + " }\n"
    return output, len(names), len(blobs)


def generate(archive_dir, work, compiler):
    work.mkdir(parents=True, exist_ok=True)
    for kind, digest in HASHES.items():
        archive = archive_dir / f"{kind}{VERSION}.tar.gz"
        assert hashlib.sha256(archive.read_bytes()).hexdigest() == digest, f"hash mismatch: {archive}"
        with tarfile.open(archive) as source:
            source.extractall(work, filter="data")
    assert (work / "version").read_text().strip() == VERSION
    (work / "version.h").write_text(f'#define PKGVERSION ""\n#define TZVERSION "{VERSION}"\n#define REPORT_BUGS_TO "tz@iana.org"\n')
    (work / "tzdir.h").write_text('#define TZDIR "zoneinfo"\n#define TZDEFAULT "localtime"\n')
    executable = work / ("zic.exe" if os.name == "nt" else "zic")
    command = [compiler, "-O2", "-std=gnu17", "-o", str(executable), str(work / "zic.c")]
    if os.name == "nt":
        command += ["-w", "-DHAVE_DIRECT_H=1", "-DHAVE_FCHMOD=0", "-DHAVE_LINK=0", "-DHAVE_SYMLINK=0", "-DHAVE_GETRANDOM=0", "-DHAVE_UNISTD_H=0", "-Dssize_t=intptr_t", str(ROOT / "tools/tzdata/windows_getopt.c")]
    subprocess.run(command, check=True)
    zones = work / "zones"
    # Never reuse files removed or renamed in a later source release.
    if zones.exists():
        assert zones.resolve().parent == work.resolve()
        shutil.rmtree(zones)
    subprocess.run([str(executable), "-b", "fat", "-d", str(zones)] + [str(work / source) for source in SOURCES], check=True)
    return tables(zones)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archives", type=Path, default=ROOT / "tools/tzdata/vendor")
    parser.add_argument("--work", type=Path, default=ROOT / "target/tzdata-generator")
    parser.add_argument("--cc", default="clang" if os.name == "nt" else "cc")
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    output, names, rules = generate(args.archives.resolve(), args.work.resolve(), args.cc)
    path = ROOT / "packages/tzdata/src/data.fos"
    if args.check:
        assert path.read_text(encoding="utf-8") == output, "generated tables are stale"
    else:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(output, encoding="utf-8", newline="\n")
    print(f"IANA {VERSION}: {names} identifiers, {rules} distinct rule sets, {len(output)} source bytes")


if __name__ == "__main__":
    main()
