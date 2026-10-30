#!/usr/bin/env python3
import copy
import importlib.util
from pathlib import Path
import unittest
import sys

sys.dont_write_bytecode = True

spec = importlib.util.spec_from_file_location("integration_ci_project", Path(__file__).with_name("integration-ci-project.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class DiscoveryContracts(unittest.TestCase):
    def setUp(self):
        self.root = Path.cwd()
        self.summary = {"module_root": str(self.root), "project_count": 1, "projects": [{"name": "chat", "path": "chat"}]}
        self.metadata = {"module_root": str(self.root), "instances": {"chat": {"name": "chat", "ci": {"pipelines": {"pullRequest": {"tasks": [{"command": "verify"}]}}}}}}

    def verify(self):
        return module.verify_project(self.summary, self.metadata, self.root / "chat", "pullRequest")

    def test_exact_project_and_nonempty_executable_pipeline(self):
        self.assertEqual(self.verify()["tasks"], 1)

    def test_missing_empty_or_stale_project_fails(self):
        for summary in [
            {"module_root": str(self.root), "project_count": 0, "projects": []},
            {"module_root": str(self.root), "project_count": 1, "projects": [{"name": "other", "path": "other"}]},
            {"module_root": str(self.root), "project_count": 2, "projects": self.summary["projects"] * 2},
        ]:
            with self.subTest(summary=summary), self.assertRaises((ValueError, KeyError)):
                module.verify_project(summary, self.metadata, self.root / "chat", "pullRequest")

    def test_missing_empty_or_nonexecutable_pipeline_fails(self):
        for config in [{}, {"pullRequest": {}}, {"pullRequest": {"tasks": []}}, {"pullRequest": {"tasks": [{}]}}]:
            metadata = copy.deepcopy(self.metadata)
            metadata["instances"]["chat"]["ci"]["pipelines"] = config
            with self.subTest(config=config), self.assertRaises(ValueError):
                module.verify_project(self.summary, metadata, self.root / "chat", "pullRequest")

    def test_mismatched_metadata_fails(self):
        self.metadata["instances"]["chat"]["name"] = "other"
        with self.assertRaises(ValueError):
            self.verify()


if __name__ == "__main__":
    unittest.main()
