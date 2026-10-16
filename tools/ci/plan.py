"""Choose CI checks from changed paths; shared contracts and unknown paths run everything."""
import argparse
import json
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from test_suites import ROOT, SUITES, load_suites


def plan(paths, suites):
    selected, packages = set(), set()
    full = ownership = parity = editor = benchmarks = benchmark_rust = documentation = False
    for path in paths:
        path = path.replace("\\", "/")
        parts = Path(path).parts
        if not parts:
            continue
        top = parts[0]
        if path.endswith(".md") and path not in {
            "documentation/semantics.md", "documentation/ownership.md",
            "documentation/language-design.md", "documentation/remote-semantics.md",
            "documentation/effect-derivation.md",
        }:
            if top == "documentation" and Path(path).name != "README.md":
                selected.update(("library", "tooling"))
                packages.add("foster")
            if path in {"library/README.md", "library/DOCUMENTATION.md"}:
                selected.add("tooling")
                packages.add("foster")
                documentation = True
            continue
        if top in {"compiler", "bytecode", "vm", "host", "runtime", "library"}:
            full = True
            documentation |= top in {"compiler", "bytecode", "library"}
        elif path in {"Cargo.toml", "Cargo.lock", "build.rs", "src/lib.rs", "src/vm.rs"}:
            full = documentation = True
        elif path.startswith("src/bin/"):
            packages.add("foster")
            benchmarks = benchmark_rust = True
        elif top == "src":
            packages.add("foster")
            selected.add("tooling")
            editor |= path.startswith("src/debugger/")
            if path.startswith("src/foreign/"):
                selected.add("cbridge")
                parity = True
            documentation |= path.startswith("src/documentation/") or path in {
                "src/project.rs", "src/tooling.rs", "src/main.rs",
            }
            if not (path.startswith(("src/lsp/", "src/debugger/", "src/documentation/", "src/foreign/"))
                    or path in {"src/lsp.rs", "src/archive.rs", "src/formatter.rs", "src/project.rs",
                                "src/tooling.rs", "src/main.rs"}):
                full = True
        elif top == "tests":
            if len(parts) >= 3 and parts[1] in suites and path.endswith(".rs"):
                selected.add(parts[1])
                packages.add("foster")
                ownership |= Path(path).stem == "ownership_soundness"
                parity |= Path(path).stem == "backend_parity"
                documentation |= Path(path).stem == "library_documentation"
            else:
                # Shared fixtures and Foster suites can be used by any backend/target.
                full = True
        elif path.startswith("tools/cbind/"):
            selected.update(("cbridge", "tooling"))
            packages.add("foster")
            parity = True
        elif path.startswith("tools/driver/"):
            selected.add("tooling")
            packages.add("foster")
            documentation = True
        elif path.startswith("tools/profiling/"):
            packages.add("foster")
            benchmarks = benchmark_rust = True
        elif top in {".github", "tools"}:
            full = documentation = editor = benchmarks = benchmark_rust = True
        elif top == "editors":
            editor = True
        elif top == "packages":
            selected.add("library")
            packages.add("foster")
        elif top in {"benchmarks", "benches", "research"}:
            benchmarks = True
            if path.endswith(".rs") and top == "benches":
                packages.add("foster")
                benchmark_rust = True
            if path.endswith(".fos"):
                # Benchmark fixtures also feed optimizer and backend parity tests.
                full = True
        elif top == "examples":
            packages.add("foster")
        elif top == "documentation":
            full = True
        elif path not in {".gitignore", ".gitattributes", "LICENSE-APACHE", "LICENSE-MIT"}:
            full = documentation = editor = benchmarks = benchmark_rust = True
    if full:
        selected.update(SUITES)
        ownership = parity = True
        editor = True
    return {
        "rust": full or bool(packages) or bool(selected),
        "packages": "" if full else " ".join(f"-p {name}" for name in sorted(packages)),
        "suites": " ".join(name for name in SUITES if name in selected),
        "ownership": ownership, "parity": parity, "editor": editor,
        "benchmarks": benchmarks, "benchmark_rust": benchmark_rust,
        "documentation": documentation,
    }


def changed_paths(base, head):
    if not base or set(base) == {"0"}:
        return ["__full_run__"]
    try:
        result = subprocess.run(
            ["git", "diff", "--no-renames", "--name-only", "-z", base, head, "--"],
            cwd=ROOT, capture_output=True, check=True,
        )
    except subprocess.CalledProcessError:
        return ["__full_run__"]
    return result.stdout.decode("utf-8").rstrip("\0").split("\0")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base")
    parser.add_argument("--head", default="HEAD")
    parser.add_argument("--changed-file", action="append", help="preview a path; repeat as needed")
    parser.add_argument("--github-output", type=Path)
    args = parser.parse_args()
    paths = args.changed_file if args.changed_file is not None else changed_paths(args.base, args.head)
    result = plan(paths, load_suites())
    print(json.dumps({"paths": paths, **result}, indent=2))
    if args.github_output:
        with args.github_output.open("a", encoding="utf-8") as output:
            for name, value in result.items():
                rendered = str(value).lower() if isinstance(value, bool) else value
                print(f"{name}={rendered}", file=output)


if __name__ == "__main__":
    main()
