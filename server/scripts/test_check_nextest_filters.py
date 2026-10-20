import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

from check_nextest_filters import dead_atoms

SCRIPT = Path(__file__).with_name("check_nextest_filters.py").resolve()


class NextestFilterLivenessTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        environment = patch.dict(os.environ, {"out": str(self.root)})
        environment.start()
        self.addCleanup(environment.stop)
        self.inventory = self.root / "inventory.json"
        self.inventory.write_text(json.dumps({
            "rust-suites": {
                "waddle-server": {
                    "binary-name": "waddle_server", "package-name": "waddle-server",
                    "testcases": {"ingress::execute::tests::commits": {}, "db::pool::opens": {}},
                },
                "waddle-server::xep0198_resume": {
                    "binary-name": "xep0198_resume", "package-name": "waddle-server",
                    "testcases": {"resumes": {}},
                },
            },
        }))
        self.config = self.root / "nextest.toml"

    def write_filter(self, expression):
        self.config.write_text(
            "[[profile.default.overrides]]\n"
            f"filter = '''{expression}'''\n"
            "test-group = 'postgres'\n"
        )

    def test_live_atoms_pass(self):
        self.write_filter(
            "package(waddle-server) & (binary(xep0198_resume) | test(ingress::) | test(=db::pool::opens)"
            " | binary_id(waddle-server::*) | test(/^db::/))"
        )
        self.assertEqual(dead_atoms(self.config, self.inventory), [])

    def test_renamed_module_is_reported(self):
        self.write_filter("test(ingress_substrate::) | test(ingress::)")
        self.assertEqual(
            dead_atoms(self.config, self.inventory),
            ["profile.default.overrides[0]: test(ingress_substrate::) matches nothing"],
        )

    def test_renamed_binary_is_reported(self):
        self.write_filter("binary(xep0198_cross_node_resume)")
        self.assertEqual(len(dead_atoms(self.config, self.inventory)), 1)

    def test_binary_atom_is_exact_by_default(self):
        self.write_filter("binary(xep0198)")
        self.assertEqual(len(dead_atoms(self.config, self.inventory)), 1)

    def test_paths_outside_build_roots_are_rejected(self):
        self.write_filter("test(ingress::)")
        with patch.dict(os.environ, {"out": ""}), patch.object(Path, "cwd", return_value=SCRIPT.parent):
            with self.assertRaises(ValueError):
                dead_atoms(self.config, self.inventory)

    def test_cli_exits_nonzero_on_dead_atom(self):
        self.write_filter("test(missing::)")
        result = subprocess.run(
            [sys.executable, SCRIPT, self.config, self.inventory],
            capture_output=True, text=True, env={**os.environ, "out": str(self.root)},
        )
        self.assertEqual(result.returncode, 1)
        self.assertIn("test(missing::) matches nothing", result.stderr)


if __name__ == "__main__":
    unittest.main()
