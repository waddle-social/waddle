#!/usr/bin/env python3
import copy
import importlib.util
from pathlib import Path
import unittest
import sys
import os
import json
import shutil
import subprocess
import tempfile

sys.dont_write_bytecode = True

spec = importlib.util.spec_from_file_location("integration_ci_project", Path(__file__).with_name("integration-ci-project.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class DiscoveryContracts(unittest.TestCase):
    def setUp(self):
        self.root = Path.cwd()
        self.summary = {"module_root": str(self.root), "project_count": 1, "projects": [{"name": "chat", "path": "chat"}]}
        task = {"command": "verify", **{key: {"cuenvOutputRef": True, "cuenvTask": "verify", "cuenvOutput": key} for key in ("stdout", "stderr", "exitCode")}}
        self.metadata = {"module_root": str(self.root), "instances": {"chat": {"name": "chat", "tasks": {"verify": copy.deepcopy(task)}, "ci": {"pipelines": {"pullRequest": {"tasks": [task]}}}}}}

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

    def test_inline_and_forged_named_tasks_fail(self):
        task = self.metadata["instances"]["chat"]["ci"]["pipelines"]["pullRequest"]["tasks"][0]
        for key in ("stdout", "stderr", "exitCode"):
            task[key]["cuenvTask"] = ""
        with self.assertRaises(ValueError):
            self.verify()
        for key in ("stdout", "stderr", "exitCode"):
            task[key]["cuenvTask"] = "verify"
        task["command"] = "different executable"
        with self.assertRaises(ValueError):
            self.verify()

    def test_mismatched_metadata_fails(self):
        self.metadata["instances"]["chat"]["name"] = "other"
        with self.assertRaises(ValueError):
            self.verify()


@unittest.skipUnless(os.environ.get("WADDLE_TEST_PINNED_CUENV") == "1", "requires explicit pinned-cuenv regression run")
class PinnedCuenvContracts(unittest.TestCase):
    def test_inline_false_noop_is_rejected_and_named_false_executes(self):
        version = subprocess.run(["cuenv", "--version"], check=True, capture_output=True, text=True).stdout.strip()
        self.assertEqual(version, "cuenv 0.57.3")
        helper = Path(__file__).with_name("integration-ci-project.py").resolve()
        with tempfile.TemporaryDirectory(prefix="waddle-discovery-pinned-") as temporary:
            root = Path(temporary)
            (root / "cue.mod").mkdir()
            (root / "cue.mod/module.cue").write_text('module: "example.test/integration"\nlanguage: version: "v0.14.0"\ndeps: "github.com/cuenv/cuenv@v0": v: "v0.57.3"\n')
            false_command = json.dumps(shutil.which("false"))
            true_command = json.dumps(shutil.which("true"))
            inline = 'package cuenv\nimport "github.com/cuenv/cuenv/schema"\nschema.#Project & {name: "fixture", ci: pipelines: pullRequest: tasks: [{command: ' + false_command + ', hermetic: false}], tasks: unused: {command: ' + true_command + ', hermetic: false}}\n'
            named = 'package cuenv\nimport "github.com/cuenv/cuenv/schema"\nschema.#Project & {let _tasks = tasks, name: "fixture", ci: pipelines: pullRequest: tasks: [_tasks.reject], tasks: reject: {command: ' + false_command + ', hermetic: false}}\n'
            def git(*args):
                return subprocess.run(["git", "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null", *args], cwd=root, check=True, capture_output=True, text=True).stdout.strip()
            (root / "env.cue").write_text(inline)
            (root / "input").write_text("base")
            git("init", "-q"); git("config", "user.name", "Integration contracts"); git("config", "user.email", "integration@example.test")
            git("add", "."); git("commit", "-qm", "base"); base = git("rev-parse", "HEAD")
            (root / "input").write_text("changed"); git("add", "."); git("commit", "-qm", "head")
            environment = dict(os.environ)
            environment.pop("GITHUB_ACTIONS", None); environment.pop("BUILDKITE", None)
            command = ["cuenv", "ci", "--pipeline", "pullRequest", "--path", ".", "--from", base]
            self.assertEqual(subprocess.run(command, cwd=root, env=environment, capture_output=True).returncode, 0)
            attest = [sys.executable, str(helper), "--path", ".", "--pipeline", "pullRequest"]
            self.assertNotEqual(subprocess.run(attest, cwd=root, capture_output=True).returncode, 0)
            (root / "env.cue").write_text(named)
            self.assertEqual(subprocess.run(attest, cwd=root, capture_output=True).returncode, 0)
            result = subprocess.run(command, cwd=root, env=environment, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("Executing reject", result.stdout)


if __name__ == "__main__":
    unittest.main()
