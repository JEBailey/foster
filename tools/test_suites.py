"""Read and validate the component integration targets declared in Cargo.toml."""
from pathlib import Path
import tomllib


ROOT = Path(__file__).resolve().parent.parent
SUITES = ("language", "runtime", "library", "tooling", "cbridge")


def load_suites(root=ROOT):
    manifest = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    if manifest["package"].get("autotests", True):
        raise ValueError("integration targets must be explicitly registered")
    suites = {name: [] for name in SUITES}
    names, declared = set(), set()
    for target in manifest.get("test", []):
        name, path = target["name"], Path(target["path"])
        if (len(path.parts) != 3 or path.parts[0] != "tests"
                or path.parts[1] not in suites or path.suffix != ".rs"):
            raise ValueError(f"invalid integration target path: {path}")
        if name in names or path in declared:
            raise ValueError(f"duplicate integration target: {name}: {path}")
        names.add(name)
        declared.add(path)
        suites[path.parts[1]].append(name)
    actual = {path.relative_to(root) for path in (root / "tests").rglob("*.rs")
              if "fixtures" not in path.relative_to(root / "tests").parts}
    if actual != declared:
        raise ValueError(f"unregistered tests: {actual - declared}; missing files: {declared - actual}")
    return {name: sorted(targets) for name, targets in suites.items()}


def select_targets(suites, selected_suites=None, selected_tests=None):
    known = {target for targets in suites.values() for target in targets}
    selected = set()
    for suite in selected_suites or []:
        if suite not in suites:
            raise ValueError(f"unknown test suite: {suite}")
        selected.update(suites[suite])
    for name in selected_tests or []:
        if name not in known:
            raise ValueError(f"unknown integration target: {name}")
        selected.add(name)
    return sorted(selected if selected_suites or selected_tests else known)
