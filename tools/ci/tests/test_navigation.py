"""Keep component entry guides and moved research references navigable."""
from pathlib import Path
import re
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from test_suites import ROOT, SUITES


class NavigationTests(unittest.TestCase):
    def test_component_guides_and_research_links_exist(self):
        guides = [ROOT / directory / "README.md" for directory in (
            "compiler", "bytecode", "vm", "runtime", "host", "src", "library",
            "packages", "editors", "tests", "tools", "tools/ci", "tools/profiling",
            "benchmarks", "benches", "research",
        )]
        guides.extend(ROOT / "tests" / suite / "README.md" for suite in SUITES)
        guides.extend(ROOT / "documentation" / name for name in (
            "README.md", "workspace.md", "testing.md", "benchmarking.md", "native.md",
        ))
        guides.append(ROOT / "README.md")
        guides.extend((ROOT / "research").rglob("*.md"))
        broken = []
        for guide in sorted(set(guides)):
            self.assertTrue(guide.is_file(), f"missing component guide: {guide}")
            for literal in re.findall(r"\]\(([^)]+)\)", guide.read_text(encoding="utf-8")):
                if ":" in literal or literal.startswith("#"):
                    continue
                path = literal.split("#", 1)[0].strip("<>")
                if not (guide.parent / path).exists():
                    broken.append(f"{guide.relative_to(ROOT)} -> {literal}")
        self.assertEqual(broken, [])


if __name__ == "__main__":
    unittest.main()
