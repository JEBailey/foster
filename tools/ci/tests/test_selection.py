"""CI selection must preserve shared-contract coverage and explicit test registration."""
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import plan
from test_suites import SUITES, load_suites, select_targets


class SelectionTests(unittest.TestCase):
    def setUp(self):
        self.suites = load_suites()

    def test_every_cargo_integration_target_is_registered_once(self):
        targets = select_targets(self.suites)
        self.assertEqual(len(targets), sum(map(len, self.suites.values())))
        self.assertTrue(all(self.suites.values()))

    def test_suites_and_individual_targets_form_a_union(self):
        targets = select_targets(self.suites, ["cbridge"], ["native", "c_callbacks"])
        self.assertEqual(set(targets), set(self.suites["cbridge"]) | {"native"})

    def test_unknown_test_is_rejected_before_a_build(self):
        with self.assertRaisesRegex(ValueError, "unknown integration target"):
            select_targets(self.suites, selected_tests=["naitve"])

    def test_unregistered_test_cannot_silently_disappear_from_ci(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "Cargo.toml").write_text('[package]\nautotests = false\n', encoding="utf-8")
            (root / "tests/language").mkdir(parents=True)
            (root / "tests/language/new_rule.rs").write_text("", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "unregistered tests"):
                load_suites(root)

    def test_shared_contracts_select_all_suites_and_release_gates(self):
        for path in ["compiler/src/typecheck/mod.rs", "bytecode/src/native/abi.rs",
                     "runtime/src/host.rs", "vm/src/value.rs", "library/core/int.fos",
                     "Cargo.lock", "documentation/semantics.md", "tests/fixtures/new.fos"]:
            with self.subTest(path=path):
                result = plan.plan([path], self.suites)
                self.assertEqual(result["suites"].split(), list(SUITES))
                self.assertTrue(result["ownership"] and result["parity"])

    def test_lsp_changes_select_tooling(self):
        result = plan.plan(["src/lsp/hover.rs"], self.suites)
        self.assertEqual(result["suites"], "tooling")
        self.assertEqual(result["packages"], "-p foster")
        self.assertFalse(result["ownership"] or result["parity"])

    def test_c_bridge_changes_select_bridge_tests_and_release_parity(self):
        result = plan.plan(["tools/cbind/src/bridge.fos"], self.suites)
        self.assertEqual(set(result["suites"].split()), {"cbridge", "tooling"})
        self.assertTrue(result["parity"])

    def test_debugger_changes_also_exercise_the_editor_adapter(self):
        result = plan.plan(["src/debugger/server.rs"], self.suites)
        self.assertEqual(result["suites"], "tooling")
        self.assertTrue(result["editor"])

    def test_changes_in_multiple_components_are_combined(self):
        result = plan.plan(["tests/language/type_branches.rs", "editors/vscode/src/extension.ts"], self.suites)
        self.assertEqual(result["suites"], "language")
        self.assertTrue(result["editor"])

    def test_unknown_sources_and_ci_changes_fail_closed(self):
        for path in ["new-component/source.rs", "src/new_shared.rs", ".github/workflows/ci.yml"]:
            self.assertEqual(plan.plan([path], self.suites)["suites"].split(), list(SUITES))

    def test_reports_and_readmes_do_not_trigger_runtime_tests(self):
        result = plan.plan(["research/results/new.json", "compiler/README.md"], self.suites)
        self.assertFalse(result["rust"])
        self.assertTrue(result["benchmarks"])

    def test_missing_diff_base_runs_everything(self):
        self.assertEqual(plan.changed_paths(None, "HEAD"), ["__full_run__"])
        self.assertEqual(plan.changed_paths("0" * 40, "HEAD"), ["__full_run__"])
        with patch.object(subprocess, "run", side_effect=subprocess.CalledProcessError(1, "git")):
            self.assertEqual(plan.changed_paths("missing", "HEAD"), ["__full_run__"])

    def test_changed_paths_include_both_sides_of_renames(self):
        result = subprocess.CompletedProcess([], 0, stdout=b"tests/language/old.rs\0src/shared.rs\0")
        with patch.object(subprocess, "run", return_value=result) as run:
            self.assertEqual(plan.changed_paths("base", "head"), ["tests/language/old.rs", "src/shared.rs"])
        self.assertIn("--no-renames", run.call_args.args[0])


if __name__ == "__main__":
    unittest.main()
