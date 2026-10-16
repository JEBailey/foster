#!/usr/bin/env python3
"""Build integration targets once, then run them with bounded concurrency."""
import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
import json
import os
from pathlib import Path
import subprocess
import sys
import time

from test_suites import SUITES, load_suites, select_targets


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--jobs", type=int, default=2,
                        help="concurrent test binaries (default: 2)")
    parser.add_argument("--threads", type=int,
                        help="threads per binary (default: CPU count / jobs)")
    parser.add_argument("--test", action="append", help="integration target; repeat to select several")
    parser.add_argument("--suite", nargs="+", choices=SUITES,
                        help="component suites; combined with any --test targets")
    parser.add_argument("--list", action="store_true", help="list selected targets without building")
    parser.add_argument("--target-dir", default="target")
    args = parser.parse_args()
    if args.jobs < 1 or (args.threads is not None and args.threads < 1):
        parser.error("jobs and threads must be positive")
    threads = args.threads or max(1, (os.cpu_count() or 1) // args.jobs)
    root = Path(__file__).resolve().parent.parent
    try:
        targets = select_targets(load_suites(root), args.suite, args.test)
    except ValueError as error:
        parser.error(str(error))
    if args.list:
        print("\n".join(targets))
        return 0
    target = (root / args.target_dir).resolve()
    logs = target / "integration-logs" / f"{time.strftime('%Y%m%d-%H%M%S')}-{os.getpid()}"
    logs.mkdir(parents=True, exist_ok=True)
    command = ["cargo", "test", "--no-run",
               "--message-format=json-render-diagnostics", "--target-dir", str(target)]
    for name in targets:
        command.extend(["--test", name])
    executables = {}
    with subprocess.Popen(command, cwd=root, stdout=subprocess.PIPE, text=True) as build:
        for line in build.stdout:
            try:
                message = json.loads(line)
            except json.JSONDecodeError:
                print(line, end="")
                continue
            if message.get("reason") == "compiler-message":
                print(message["message"].get("rendered", ""), end="", file=sys.stderr)
            if (message.get("reason") == "compiler-artifact"
                    and message.get("executable")
                    and "test" in message["target"]["kind"]
                    and message["target"]["name"] in targets
                    and Path(message["manifest_path"]).parent.resolve() == root):
                executables[message["executable"]] = (
                    message["target"]["name"], Path(message["manifest_path"]).parent
                )
        if build.wait():
            return build.returncode
    found = {name for name, _ in executables.values()}
    if found != set(targets):
        print(f"Integration target discovery mismatch: expected {targets}, found {sorted(found)}",
              file=sys.stderr)
        return 1

    def run(index, executable, name, directory):
        log = logs / f"{index:03}-{name}.log"
        start = time.monotonic()
        with log.open("w", encoding="utf-8") as output:
            try:
                code = subprocess.run(
                    [executable, f"--test-threads={threads}"], cwd=directory,
                    stdout=output, stderr=subprocess.STDOUT,
                ).returncode
            except OSError as error:
                print(error, file=output)
                code = 1
        return name, code, time.monotonic() - start, log

    failed = 0
    started = time.monotonic()
    print(f"Running {len(executables)} targets: {args.jobs} binaries, {threads} threads each", flush=True)
    with ThreadPoolExecutor(max_workers=args.jobs) as pool:
        futures = [
            pool.submit(run, index, executable, name, directory)
            for index, (executable, (name, directory))
            in enumerate(sorted(executables.items()))
        ]
        for future in as_completed(futures):
            name, code, elapsed, log = future.result()
            print(f"{'PASS' if code == 0 else 'FAIL'} {name} ({elapsed:.1f}s): {log}", flush=True)
            if code:
                failed += 1
                print(log.read_text(encoding="utf-8", errors="replace"))
    print(f"Completed {len(executables)} targets in {time.monotonic() - started:.1f}s: "
          f"{len(executables) - failed} passed, {failed} failed", flush=True)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
