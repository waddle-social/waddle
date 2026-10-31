#!/usr/bin/env python3
import copy
import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch
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


def local_fixture_environment(environment):
    return {key: value for key, value in environment.items()
            if not key.startswith(("GITHUB_", "WADDLE_CI_")) and key not in ("BUILDKITE", "GH_TOKEN")}


class FixtureEnvironmentContracts(unittest.TestCase):
    def test_scrubs_all_github_candidate_and_base_variables(self):
        environment = {"PATH": "/tools", "GITHUB_ACTIONS": "true", "GITHUB_SHA": "candidate", "GITHUB_BASE_REF": "main", "GITHUB_EVENT_PATH": "/event.json", "GITHUB_FUTURE_CONTEXT": "unknown", "WADDLE_CI_HEAD_SHA": "candidate", "WADDLE_CI_BASE_SHA": "base", "BUILDKITE": "true", "GH_TOKEN": "secret"}
        self.assertEqual(local_fixture_environment(environment), {"PATH": "/tools"})


class DiscoveryContracts(unittest.TestCase):
    def setUp(self):
        self.root = Path.cwd()
        self.summary = {"module_root": str(self.root), "project_count": 1, "projects": [{"name": "chat", "path": "chat"}]}
        task = {"command": "verify", **{key: {"cuenvOutputRef": True, "cuenvTask": "verify", "cuenvOutput": key} for key in ("stdout", "stderr", "exitCode")}}
        self.metadata = {"module_root": str(self.root), "instances": {"chat": {"name": "chat", "tasks": {"verify": copy.deepcopy(task)}, "ci": {"pipelines": {"pullRequest": {"tasks": [task]}}}}}}

    def verify(self):
        return module.verify_project(self.summary, self.metadata, self.root / "chat", "pullRequest")

    def test_cli_paths_and_names_are_validated(self):
        self.assertEqual(module.validated_project_path("."), str(self.root))
        for value in ("--help", "x;command", "x\ncommand", "$(command)"):
            with self.subTest(path=value), self.assertRaises((ValueError, OSError)):
                module.validated_project_path(value)
        for value in ("test", "nested.test", "--help", "--dry-run"):
            self.assertEqual(module.validated_task_name(value), value)
        for value in ("x --help", "../test", "x;command", "$(command)", "x\ncommand", ""):
            with self.subTest(task=value), self.assertRaises(ValueError):
                module.validated_task_name(value)

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

    def test_supported_pipelines_are_narrowly_scoped(self):
        for pipeline in ("default", "publish", "--help", "deviceTests"):
            with self.subTest(pipeline=pipeline), self.assertRaises(ValueError):
                module.verify_project(self.summary, self.metadata, self.root / "chat", pipeline)
        self.summary["projects"][0].update(name="waddle-android", path="apps/android")
        config = self.metadata["instances"].pop("chat")
        config["name"] = "waddle-android"
        config["ci"]["pipelines"]["deviceTests"] = config["ci"]["pipelines"]["pullRequest"]
        self.metadata["instances"]["apps/android"] = config
        self.assertEqual(module.verify_project(self.summary, self.metadata, self.root / "apps/android", "deviceTests")["tasks"], 1)

    def test_mismatched_metadata_fails(self):
        self.metadata["instances"]["chat"]["name"] = "other"
        with self.assertRaises(ValueError):
            self.verify()

    def test_continuing_independent_builds_is_scoped_to_android_pr(self):
        config = self.metadata["instances"]["chat"]
        config["ci"]["pipelines"]["pullRequest"]["continueOnError"] = True
        with self.assertRaises(ValueError):
            self.verify()
        self.summary["projects"][0].update(name="waddle-android", path="apps/android")
        self.metadata["instances"]["apps/android"] = self.metadata["instances"].pop("chat")
        config["name"] = "waddle-android"
        result = module.verify_project(self.summary, self.metadata, self.root / "apps/android", "pullRequest")
        self.assertEqual(result["task_names"], ["verify"])
        config["ci"]["pipelines"]["deviceTests"] = copy.deepcopy(config["ci"]["pipelines"]["pullRequest"])
        with self.assertRaises(ValueError):
            module.verify_project(self.summary, self.metadata, self.root / "apps/android", "deviceTests")
        for invalid in (1, "true", None):
            config["ci"]["pipelines"]["pullRequest"]["continueOnError"] = invalid
            with self.subTest(value=invalid), self.assertRaises(ValueError):
                module.verify_project(self.summary, self.metadata, self.root / "apps/android", "pullRequest")


class ReportContracts(unittest.TestCase):
    def setUp(self):
        self.context = {"provider": "github", "event": "merge_group", "ref_name": "gh-readonly-queue/main/example", "base_ref": "", "sha": "a" * 40}
        self.result = {"pipeline": "pullRequest", "task_names": ["verify"]}
        self.report = {"version": "0.63.0", "project": "/fixture", "pipeline": "pullRequest", "context": copy.deepcopy(self.context), "status": "success", "started_at": "1970-01-01T00:00:11Z", "completed_at": "1970-01-01T00:00:12Z", "tasks": [{"name": "verify", "status": "success", "exit_code": 0}]}

    def verify(self, report):
        module.verify_report(report, self.result, "/fixture", self.context, "0.63.0", 10, 13)

    def test_android_apk_success_cannot_hide_an_independent_check_failure(self):
        self.result["task_names"] = ["lint", "build"]
        self.report["tasks"] = [
            {"name": "lint", "status": "failed", "exit_code": 1},
            {"name": "build", "status": "success", "exit_code": 0},
        ]
        for status in ("failed", "success"):
            self.report["status"] = status
            with self.subTest(pipeline_status=status), self.assertRaises(ValueError):
                self.verify(self.report)

    def test_exact_report_and_invalid_reports(self):
        self.verify(self.report)
        variants = []
        for key, value in (("version", "old"), ("project", "/other"), ("pipeline", "default"), ("status", "failed"), ("started_at", "1970-01-01T00:00:09Z"), ("completed_at", "1970-01-01T00:00:14Z"), ("tasks", []), ("tasks", self.report["tasks"] * 2)):
            report = copy.deepcopy(self.report); report[key] = value; variants.append(report)
        for key in self.context:
            report = copy.deepcopy(self.report); report["context"][key] = "wrong"; variants.append(report)
        for status, code in (("skipped", 0), ("cached", 0), ("failed", 1), ("success", 1), ("success", False), ("success", None)):
            report = copy.deepcopy(self.report); report["tasks"][0].update(status=status, exit_code=code); variants.append(report)
        for report in variants:
            with self.subTest(report=report), self.assertRaises(ValueError):
                self.verify(report)
        for text in ('{"status":"success","status":"failed"}', '{invalid', 'null'):
            with self.subTest(text=text), self.assertRaises((ValueError, AttributeError)):
                self.verify(json.loads(text, object_pairs_hook=module.unique_json_object))

    def test_report_parent_symlinks_fail(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "link").symlink_to(root, target_is_directory=True)
            with self.assertRaises(ValueError):
                module.check_report_parents(root / "link" / "report.json")


@unittest.skipUnless(os.environ.get("WADDLE_TEST_ALL_CUENV") == "1", "requires reviewed --all CLI in PATH")
class PinnedCuenvContracts(unittest.TestCase):
    def test_inline_false_noop_is_rejected_and_named_false_executes(self):
        version = subprocess.run(["cuenv", "--version"], check=True, capture_output=True, text=True).stdout.strip()
        self.assertTrue(version.startswith("cuenv "))
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
            environment = local_fixture_environment(os.environ)
            command = ["cuenv", "ci", "--pipeline", "pullRequest", "--path", ".", "--from", base]
            self.assertNotEqual(subprocess.run(command + ["--all"], cwd=root, env=environment, capture_output=True).returncode, 0)
            attest = [sys.executable, str(helper), "--path", ".", "--pipeline", "pullRequest"]
            self.assertNotEqual(subprocess.run(attest, cwd=root, env=environment, capture_output=True).returncode, 0)
            bound_inline = inline.replace('hermetic: false}], tasks: unused:', 'hermetic: false, stdout: cuenvTask: "reject", stderr: cuenvTask: "reject", exitCode: cuenvTask: "reject"}], tasks: reject:').replace(true_command, false_command)
            (root / "env.cue").write_text(bound_inline)
            self.assertNotEqual(subprocess.run(command + ["--all"], cwd=root, env=environment, capture_output=True).returncode, 0)
            self.assertEqual(subprocess.run(attest, cwd=root, env=environment, capture_output=True).returncode, 0)
            bound_result = subprocess.run(attest + ["--run"], cwd=root, env=environment, capture_output=True, text=True)
            self.assertNotEqual(bound_result.returncode, 0)
            self.assertIn("named task references", bound_result.stderr)
            (root / "env.cue").write_text(named)
            self.assertEqual(subprocess.run(attest, cwd=root, env=environment, capture_output=True).returncode, 0)
            result = subprocess.run(command, cwd=root, env=environment, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("Executing reject", result.stdout)
            shutil.rmtree(root / ".cuenv/reports", ignore_errors=True)
            shell = json.dumps(shutil.which("sh"))
            dependent = 'package cuenv\nimport "github.com/cuenv/cuenv/schema"\nschema.#Project & {let _tasks = tasks, name: "fixture", ci: pipelines: pullRequest: tasks: [_tasks.second], tasks: {first: {command: ' + shell + ', args: ["-c", "printf before >> trace"], hermetic: false}, second: {command: ' + shell + ', args: ["-c", "printf after >> trace"], hermetic: false, dependsOn: [_tasks.first]}}}\n'
            (root / "env.cue").write_text(dependent)
            dependency_result = subprocess.run(attest + ["--run"], cwd=root, env=environment, capture_output=True, text=True)
            self.assertEqual(dependency_result.returncode, 0, dependency_result.stderr)
            self.assertEqual((root / "trace").read_text(), "beforeafter")
            stale = subprocess.run(attest + ["--run"], cwd=root, env=environment, capture_output=True, text=True)
            self.assertNotEqual(stale.returncode, 0)
            self.assertIn("report already exists", stale.stderr)
            shutil.rmtree(root / ".cuenv/reports")
            (root / "env.cue").write_text(dependent.replace("printf before >> trace", "exit 7"))
            dependency_failure = subprocess.run(attest + ["--run"], cwd=root, env=environment, capture_output=True, text=True)
            self.assertNotEqual(dependency_failure.returncode, 0)
            shutil.rmtree(root / ".cuenv/reports", ignore_errors=True)
            for option_name in ("--help", "--dry-run"):
                with self.subTest(task_name=option_name):
                    quoted_name = json.dumps(option_name)
                    option_task = named.replace("_tasks.reject", "_tasks[" + quoted_name + "]").replace("tasks: reject:", "tasks: " + quoted_name + ":")
                    (root / "env.cue").write_text(option_task)
                    self.assertEqual(subprocess.run(attest, cwd=root, env=environment, capture_output=True).returncode, 0)
                    option_result = subprocess.run(attest + ["--run"], cwd=root, env=environment, capture_output=True, text=True)
                    self.assertNotEqual(option_result.returncode, 0)
                    self.assertIn("failed", option_result.stdout + option_result.stderr)
                    shutil.rmtree(root / ".cuenv/reports", ignore_errors=True)
            for invalid in ('package cuenv\n', named.replace('tasks: [_tasks.reject]', 'tasks: []'), named.replace('pullRequest:', 'missing:'), 'package cuenv\ninvalid: ['):
                (root / "env.cue").write_text(invalid)
                with self.subTest(invalid=invalid):
                    self.assertNotEqual(subprocess.run(attest + ["--run"], cwd=root, env=environment, capture_output=True).returncode, 0)


    def test_every_inline_adapter_subprocess_receives_scrubbed_environment(self):
        helper = str(Path(__file__).with_name("integration-ci-project.py").resolve())
        poisoned = {key: "leaked" for key in ("GITHUB_REPOSITORY", "GITHUB_SHA", "GITHUB_REF", "GITHUB_REF_NAME", "GITHUB_BASE_REF", "GITHUB_HEAD_REF", "GITHUB_EVENT_PATH", "GITHUB_BEFORE", "GITHUB_TOKEN", "WADDLE_CI_HEAD_SHA", "WADDLE_CI_BASE_SHA")}
        poisoned.update(GITHUB_ACTIONS="true", GITHUB_EVENT_NAME="merge_group", BUILDKITE="true", GH_TOKEN="secret")
        original_run = subprocess.run
        adapter_calls = []
        def checked_run(command, **options):
            if command[:2] == [sys.executable, helper]:
                adapter_calls.append(command)
                self.assertIn("env", options)
                self.assertEqual(options["env"], local_fixture_environment(options["env"]))
                self.assertTrue(all(key not in options["env"] for key in poisoned))
            return original_run(command, **options)
        with patch.dict(os.environ, poisoned), patch.object(subprocess, "run", side_effect=checked_run):
            self.test_inline_false_noop_is_rejected_and_named_false_executes()
        self.assertGreaterEqual(len(adapter_calls), 15)


    def test_real_event_hooks_and_missing_or_forged_reports(self):
        helper = Path(__file__).with_name("integration-ci-project.py").resolve()
        real_cli = shutil.which("cuenv")
        with tempfile.TemporaryDirectory(prefix="waddle-report-all-") as temporary:
            root = Path(temporary)
            (root / "cue.mod").mkdir()
            (root / "cue.mod/module.cue").write_text('module: "example.test/integration"\nlanguage: version: "v0.14.0"\ndeps: "github.com/cuenv/cuenv@v0": v: "v0.57.3"\n')
            shell = json.dumps(shutil.which("sh"))
            source = 'package cuenv\nimport "github.com/cuenv/cuenv/schema"\nschema.#Project & {\n  let _tasks = tasks\n  name: "fixture"\n  hooks: onEnter: observe: {\n    command: SHELL\n    args: ["-c", #"printf \'export OBSERVED_EVENT=%s\\n\' "$GITHUB_EVENT_NAME""#]\n    source: true\n  }\n  ci: pipelines: pullRequest: tasks: [_tasks.check]\n  tasks: check: {\n    command: SHELL\n    args: ["-c", #"test "$OBSERVED_EVENT" = merge_group && printf checked > checked"#]\n    hermetic: false\n  }\n}\n'.replace("SHELL", shell)
            (root / "env.cue").write_text(source)
            def git(*args):
                return subprocess.run(["git", "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null", *args], cwd=root, check=True, capture_output=True, text=True).stdout.strip()
            git("init", "-q"); git("config", "user.name", "Report contracts"); git("config", "user.email", "integration@example.test"); git("add", "."); git("commit", "-qm", "base")
            head = git("rev-parse", "HEAD")
            environment = dict(os.environ, GITHUB_ACTIONS="true", GITHUB_EVENT_NAME="merge_group", GITHUB_REPOSITORY="example/fixture", GITHUB_REF="refs/heads/gh-readonly-queue/main/example", GITHUB_REF_NAME="gh-readonly-queue/main/example", GITHUB_SHA=head, GITHUB_BASE_REF="")
            for key in ("BUILDKITE", "GITHUB_TOKEN", "GH_TOKEN"):
                environment.pop(key, None)
            command = [sys.executable, str(helper), "--path", ".", "--pipeline", "pullRequest", "--run"]
            result = subprocess.run(command, cwd=root, env=environment, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual((root / "checked").read_text(), "checked")
            reports = root / ".cuenv/reports"
            report = next(reports.rglob("*.json"))
            value = json.loads(report.read_text())
            self.assertEqual(value["context"]["event"], "merge_group")
            self.assertEqual(value["context"]["sha"], head)
            stale = subprocess.run(command, cwd=root, env=environment, capture_output=True, text=True)
            self.assertNotEqual(stale.returncode, 0)
            self.assertIn("report already exists", stale.stderr)
            # Delegate to the real CLI, then corrupt only its on-disk evidence.
            binary_dir = root / "bin"; binary_dir.mkdir()
            wrapper = binary_dir / "cuenv"
            wrapper.write_text("#!" + sys.executable + "\n" + "import json, os, pathlib, subprocess, sys, time\n" + "result = subprocess.run([" + repr(real_cli) + "] + sys.argv[1:])\n" + "if sys.argv[1:3] == ['ci', '--all'] and result.returncode == 0:\n" + "    path = next(pathlib.Path('.cuenv/reports').rglob('*.json'))\n" + "    mode = os.environ['REPORT_TAMPER']\n" + "    if mode == 'missing': path.unlink()\n" + "    elif mode == 'symlink':\n" + "        other = path.with_suffix('.saved'); path.rename(other); path.symlink_to(other.name)\n" + "    elif mode == 'future': os.utime(path, (time.time() + 60, time.time() + 60))\n" + "    elif mode == 'parse': path.write_text('{invalid')\n" + "    else:\n" + "        value = json.loads(path.read_text())\n" + "        if mode == 'identity': value['context']['sha'] = 'b' * 40\n" + "        if mode == 'duplicate': value['tasks'] *= 2\n" + "        if mode == 'skipped': value['tasks'][0]['status'] = 'skipped'\n" + "        if mode == 'failed': value['tasks'][0]['exit_code'] = 1\n" + "        path.write_text(json.dumps(value))\n" + "sys.exit(result.returncode)\n")
            wrapper.chmod(0o755)
            for mode in ("missing", "identity", "duplicate", "skipped", "failed", "parse", "symlink", "future"):
                shutil.rmtree(reports)
                altered = dict(environment, PATH=str(binary_dir) + os.pathsep + environment["PATH"], REPORT_TAMPER=mode)
                with self.subTest(mode=mode):
                    result = subprocess.run(command, cwd=root, env=altered, capture_output=True, text=True)
                    self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
