#!/usr/bin/env python3
"""Check the generated main-workflow publish and runner-selection contract."""

import copy
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ".github/workflows/waddle-server-default.yml"
RUNNER = (
    "${{ fromJSON(fromJSON(toJSON(needs)).fmt.result == '' "
    "&& '[\"nscloud-ubuntu-24.04-amd64-32x64-with-features\","
    "\"namespace-features:container.privileged=true;job.priority=1\"]' "
    "|| '[\"namespace-profile-linux-x86;container.privileged=true\"]') }}"
)
# The runner expression gives the 32-core builder to every job that does not
# wait for fmt, so exactly these two jobs may have no dependencies.
BUILDER_JOBS = {"fmt", "nixTestLocal"}
# Each of these waits for fmt alone and runs on the normal profile.
PROFILE_GATES = {
    "mainChecks",
    "mainClippy",
    "mainTest",
    "mainDoctest",
    "mainBuildImageStream",
    "mainBuildExtensionModules",
}
PUBLISH_GATES = BUILDER_JOBS | PROFILE_GATES
PUBLISH = "publishContainerImage"
# The short gates that share the mainChecks job.
BATCHED_CHECKS = [
    "checkCiDrift",
    "checkSwitchableAlternativeProgram",
    "checkAlertRules",
    "checkXmppClientFfiBindings",
    "renderDeployment",
]
# Each main-only gate runs the command of the task pull requests run.
ALIASES = {
    "mainClippy": "nixClippy",
    "mainTest": "nixDefaultTest",
    "mainDoctest": "nixDoctest",
    "mainBuildImageStream": "nixBuildImageStream",
    "mainBuildExtensionModules": "nixBuildExtensionModules",
}
SHARDS = ["timeout", "--kill-after=2m", "55m", "bash", "scripts/run-nextest-shards.sh"]


def cue_export(*arguments):
    result = subprocess.run(
        ["cue", "export", *arguments, "--out", "json"],
        cwd=ROOT,
        check=True,
        text=True,
        capture_output=True,
    )
    return json.loads(result.stdout)


def needs(job):
    value = job.get("needs", [])
    if isinstance(value, str):
        return {value}
    if isinstance(value, list) and all(isinstance(item, str) for item in value):
        return set(value)
    raise ValueError("job needs must be a string or a list of strings")


def run_batch(script, failing=()):
    """Run the mainChecks script against a stub cuenv; return (status, calls)."""
    with tempfile.TemporaryDirectory() as directory:
        calls = Path(directory) / "calls"
        calls.touch()
        stub = Path(directory) / "cuenv"
        stub.write_text(
            "#!/usr/bin/env bash\n"
            'test "$1" = task || exit 64\n'
            'echo "$2" >> "$CALLS"\n'
            'case " $FAILING " in *" $2 "*) exit 1 ;; esac\n'
        )
        stub.chmod(0o755)
        environment = dict(os.environ)
        environment["PATH"] = f"{directory}{os.pathsep}{environment['PATH']}"
        environment["CALLS"] = str(calls)
        environment["FAILING"] = " ".join(failing)
        result = subprocess.run(
            ["bash", *script], env=environment, capture_output=True, text=True, check=False
        )
        return result.returncode, calls.read_text().split()


def validate(workflow, tasks, helper_script):
    def require(condition, message):
        if not condition:
            raise ValueError(message)

    events = workflow.get("on", {})
    push = events.get("push", {})
    require(push.get("branches") == ["main"], "main push trigger changed")
    require("workflow_dispatch" in events, "manual trigger missing")
    require("paths-ignore" not in push, "main push adds path exclusions")
    paths = set(push.get("paths", []))
    require(
        {
            ".github/workflows/waddle-server-default.yml",
            "server/env.cue",
            "server/.config/required-cluster-tests.txt",
            "server/crates/**",
            "server/scripts/**",
            "flake.nix",
            "flake.lock",
        }.issubset(paths),
        "main push paths omit publish-gate inputs",
    )
    concurrency = workflow.get("concurrency", {})
    require(
        concurrency.get("group") == "${{ github.workflow }}-${{ github.head_ref || github.ref }}"
        and concurrency.get("cancel-in-progress") is False,
        "main must finish the active publish run and coalesce pending pushes",
    )

    def command(name):
        task = tasks.get(name, {})
        return [task.get("command"), *task.get("args", [])]

    jobs = workflow.get("jobs", {})
    require(set(jobs) == PUBLISH_GATES | {PUBLISH}, "unexpected job inventory")
    for name in BUILDER_JOBS:
        require(not needs(jobs[name]), f"builder job unexpectedly has dependencies: {name}")
    for name in PROFILE_GATES:
        require(needs(jobs[name]) == {"fmt"}, f"gate must wait for fmt alone: {name}")
    require(
        needs(jobs[PUBLISH]) == PUBLISH_GATES,
        "publication must directly require every gate",
    )
    for name, job in jobs.items():
        require(job.get("runs-on") == RUNNER, f"unexpected runner selector: {name}")
        require("if" not in job, f"conditional can bypass dependency success: {name}")
        require(not job.get("continue-on-error", False), f"job tolerates errors: {name}")
        steps = job.get("steps", [])
        checkout = [step for step in steps if step.get("name") == "Checkout"]
        require(len(checkout) == 1, f"checkout missing or duplicated: {name}")
        require(
            checkout[0].get("uses", "").startswith("actions/checkout@")
            and "ref" not in checkout[0].get("with", {})
            and "if" not in checkout[0]
            and not checkout[0].get("continue-on-error", False),
            f"checkout can skip or select a different revision: {name}",
        )
        task_steps = [step for step in steps if step.get("name") == name]
        require(len(task_steps) == 1, f"task step missing or duplicated: {name}")
        step = task_steps[0]
        # cuenv emits a task whose command is nix as that command line.
        expected = (
            " ".join(command(name))
            if tasks.get(name, {}).get("command") == "nix"
            else f"cuenv task {name} --skip-dependencies"
        )
        require(
            step.get("run") == expected
            and step.get("working-directory") == "server"
            and "if" not in step
            and not step.get("continue-on-error", False),
            f"task step can skip or tolerate failure: {name}",
        )
    require(
        jobs[PUBLISH].get("permissions", {}).get("packages") == "write",
        "publication cannot push the image",
    )

    require(
        command("ciHelperContracts") == ["bash", "scripts/check-ci-helper-contracts.sh"]
        and tasks["ciHelperContracts"].get("hermetic") is False,
        "helper task no longer runs the uncached contracts",
    )
    require(
        command("nixTestShards") == SHARDS and tasks["nixTestShards"].get("hermetic") is False,
        "shard task no longer runs the bounded shard lane",
    )
    require(
        command("nixTestLocal")
        == [
            "bash",
            "-euo",
            "pipefail",
            "-c",
            "bash scripts/check-ci-helper-contracts.sh && exec " + " ".join(SHARDS),
        ]
        and tasks["nixTestLocal"].get("hermetic") is False,
        "test task no longer runs the helper contracts and then the bounded shard lane",
    )
    for alias, source in ALIASES.items():
        require(
            command(alias) == command(source)
            and tasks[alias].get("hermetic") is tasks[source].get("hermetic"),
            f"main gate no longer runs its source task: {alias}",
        )
    batch = tasks.get("mainChecks", {})
    require(
        batch.get("command") == "bash" and batch.get("hermetic") is False,
        "batched checks no longer run uncached in the checkout",
    )
    status, calls = run_batch(batch.get("args", []))
    require(status == 0 and calls == BATCHED_CHECKS, "batched checks changed")
    for check in BATCHED_CHECKS:
        status, calls = run_batch(batch["args"], failing=[check])
        require(
            status != 0 and calls == BATCHED_CHECKS,
            f"batched checks tolerate or stop at a failure: {check}",
        )
    drift_task = tasks.get("checkCiDrift", {})
    require(
        drift_task.get("command") == "bash"
        and any(
            "python3 server/scripts/test_default_publish_graph.py" in argument
            for argument in drift_task.get("args", [])
        ),
        "direct drift gate no longer runs the graph contract",
    )
    helper_lines = {line.strip() for line in helper_script.splitlines()}
    require(
        "python3 scripts/test_default_publish_graph.py" in helper_lines
        and "(cd .. && nix develop --no-write-lock-file --command python3 server/scripts/test_default_publish_graph.py)" in helper_lines,
        "direct helper gate no longer runs the graph contract with pinned CUE",
    )


class PublishGraphTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        version = subprocess.run(["cue", "version"], check=True, text=True, capture_output=True)
        if "CUE language version v0.18.0" not in version.stdout.splitlines():
            raise RuntimeError("the publish graph requires CUE 0.18.0")
        cls.workflow = cue_export(WORKFLOW)
        cls.tasks = cue_export("./server", "-e", "tasks")
        cls.helper_script = (ROOT / "server/scripts/check-ci-helper-contracts.sh").read_text()

    def check(self, workflow=None, tasks=None, helper_script=None):
        validate(
            self.workflow if workflow is None else workflow,
            self.tasks if tasks is None else tasks,
            self.helper_script if helper_script is None else helper_script,
        )

    def test_generated_workflow_preserves_the_contract(self):
        self.check()

    def test_new_push_cannot_cancel_an_active_publish_run(self):
        workflow = copy.deepcopy(self.workflow)
        workflow["concurrency"]["cancel-in-progress"] = True
        with self.assertRaisesRegex(ValueError, "finish the active publish run"):
            self.check(workflow=workflow)

    def test_missing_or_added_dependency_is_rejected(self):
        for source, target, add in (
            ("mainTest", "fmt", False),
            ("publishContainerImage", "fmt", False),
            ("publishContainerImage", "mainChecks", False),
            ("publishContainerImage", "mainTest", False),
            ("publishContainerImage", "nixTestLocal", False),
            ("nixTestLocal", "fmt", True),
            ("mainChecks", "nixTestLocal", True),
        ):
            with self.subTest(source=source, target=target, add=add):
                workflow = copy.deepcopy(self.workflow)
                dependencies = workflow["jobs"][source].setdefault("needs", [])
                if add:
                    dependencies.append(target)
                else:
                    dependencies.remove(target)
                with self.assertRaises(ValueError):
                    self.check(workflow=workflow)

    def test_added_or_removed_job_is_rejected(self):
        workflow = copy.deepcopy(self.workflow)
        workflow["jobs"]["extra"] = copy.deepcopy(workflow["jobs"]["fmt"])
        with self.assertRaises(ValueError):
            self.check(workflow=workflow)
        workflow = copy.deepcopy(self.workflow)
        del workflow["jobs"]["mainChecks"]
        workflow["jobs"]["publishContainerImage"]["needs"].remove("mainChecks")
        with self.assertRaises(ValueError):
            self.check(workflow=workflow)

    def test_permissive_conditions_and_error_tolerance_are_rejected(self):
        for name in ("fmt", "mainChecks", "nixTestLocal", "publishContainerImage"):
            for field, value in (("if", "${{ always() }}"), ("continue-on-error", True)):
                with self.subTest(name=name, field=field):
                    workflow = copy.deepcopy(self.workflow)
                    workflow["jobs"][name][field] = value
                    with self.assertRaises(ValueError):
                        self.check(workflow=workflow)
            with self.subTest(name=name, field="step"):
                workflow = copy.deepcopy(self.workflow)
                step = next(s for s in workflow["jobs"][name]["steps"] if s.get("name") == name)
                step["continue-on-error"] = True
                with self.assertRaises(ValueError):
                    self.check(workflow=workflow)

    def test_changed_runner_or_test_command_is_rejected(self):
        workflow = copy.deepcopy(self.workflow)
        workflow["jobs"]["nixTestLocal"]["runs-on"] = "namespace-profile-linux-x86"
        with self.assertRaises(ValueError):
            self.check(workflow=workflow)
        for task, argument in (("nixTestLocal", "scripts/other.sh"), ("nixTestShards", "scripts/other.sh")):
            tasks = copy.deepcopy(self.tasks)
            tasks[task]["args"][-1] = argument
            with self.assertRaises(ValueError):
                self.check(tasks=tasks)
        tasks = copy.deepcopy(self.tasks)
        tasks["nixTestLocal"]["args"][-1] = " ".join(SHARDS)
        with self.assertRaises(ValueError):
            self.check(tasks=tasks)
        tasks = copy.deepcopy(self.tasks)
        tasks["mainTest"]["args"] = tasks["mainClippy"]["args"]
        with self.assertRaises(ValueError):
            self.check(tasks=tasks)

    def test_batched_checks_that_drop_or_tolerate_a_check_are_rejected(self):
        tasks = copy.deepcopy(self.tasks)
        tasks["mainChecks"]["args"][-1] = tasks["mainChecks"]["args"][-1].replace(" checkAlertRules", "")
        with self.assertRaises(ValueError):
            self.check(tasks=tasks)
        tasks = copy.deepcopy(self.tasks)
        tasks["mainChecks"]["args"][-1] = tasks["mainChecks"]["args"][-1].replace("exit 1", "exit 0")
        with self.assertRaises(ValueError):
            self.check(tasks=tasks)
        tasks = copy.deepcopy(self.tasks)
        tasks["mainChecks"]["args"][-1] = tasks["mainChecks"]["args"][-1].replace(
            '|| failed+=("${check}")', "|| exit 1"
        )
        with self.assertRaises(ValueError):
            self.check(tasks=tasks)

    def test_removing_either_contract_invocation_is_rejected(self):
        tasks = copy.deepcopy(self.tasks)
        tasks["checkCiDrift"]["args"] = [
            argument.replace("test_default_publish_graph.py", "other.py")
            for argument in tasks["checkCiDrift"]["args"]
        ]
        with self.assertRaises(ValueError):
            self.check(tasks=tasks)
        with self.assertRaises(ValueError):
            self.check(helper_script=self.helper_script.replace("test_default_publish_graph", "other"))


if __name__ == "__main__":
    unittest.main()
