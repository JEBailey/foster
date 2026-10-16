"""Syntax-check measurement harnesses without building or timing workloads."""
import ast
from pathlib import Path
import subprocess


ROOT = Path(__file__).resolve().parents[2]


def main():
    for directory in (ROOT / "benchmarks/harnesses", ROOT / "research/experiments"):
        for path in sorted(directory.iterdir()):
            if path.suffix == ".py":
                ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
            elif path.suffix == ".cjs":
                subprocess.run(["node", "--check", str(path)], check=True)
            else:
                continue
            print(f"PASS {path.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
