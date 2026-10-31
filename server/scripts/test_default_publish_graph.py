#!/usr/bin/env python3
"""Check the generated main-workflow publish and runner-selection contract."""

import copy
import json
from pathlib import Path
import subprocess
import unittest


ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ".github/workflows/waddle-server-default.yml"
RUNNER = (
    "${{ fromJSON(fromJSON(toJSON(needs)).ciHelperContracts.result == 'success' "
    "&& fromJSON(toJSON(needs)).nixTestLocal.result == '' "
    "&& '[\"nscloud-ubuntu-24.04-amd64-32x64-with-features\","
    "\"namespace-features:container.privileged=true;job.priority=1\"]' "
    "|| '[\"namespace-profile-linux-x86\"]') }}"
)
PUBLISH_GATES = {
    "checkCiDrift",
    "checkSwitchableAlternativeProgram",
    "fmt",
    "clippy",
    "test",
    "doctest",
    "checkXmppClientFfiBindings",
    "renderDeployment",
    "nixBuildImageStream",
    "nixBuildExtensionModules",
    "ciHelperContracts",
    "nixTestLocal",
}


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


def validate(workflow, helper_task, test_task, drift_task, helper_script):
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
        and concurrency.get("cancel-in-progress") is True,
        "main workflow concurrency changed",
    )

    jobs = workflow.get("jobs", {})
    for name in ("ciHelperContracts", "nixTestLocal", "publishContainerImage"):
        require(name in jobs, f"missing job: {name}")
    require(not needs(jobs["ciHelperContracts"]), "helpers unexpectedly have dependencies")
    require(
        needs(jobs["nixTestLocal"]) == {"ciHelperContracts"},
        "test job must directly require helpers",
    )
    require(
        PUBLISH_GATES.issubset(needs(jobs["publishContainerImage"])),
        "publication is missing a direct gate",
    )
    consumers = {name for name, job in jobs.items() if "ciHelperContracts" in needs(job)}
    require(
        consumers == {"nixTestLocal", "publishContainerImage"},
        "unexpected helper dependency changes runner selection",
    )
    for name, job in jobs.items():
        require(job.get("runs-on") == RUNNER, f"unexpected runner selector: {name}")

    for name in ("ciHelperContracts", "nixTestLocal", "publishContainerImage"):
        job = jobs[name]
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
        require(
            step.get("run") == f"cuenv task {name} --skip-dependencies"
            and step.get("working-directory") == "server"
            and "if" not in step
            and not step.get("continue-on-error", False),
            f"task step can skip or tolerate failure: {name}",
        )

    require(
        helper_task.get("command") == "bash"
        and helper_task.get("args") == ["scripts/check-ci-helper-contracts.sh"]
        and helper_task.get("hermetic") is False,
        "helper task no longer runs the uncached contracts",
    )
    require(
        test_task.get("command") == "timeout"
        and test_task.get("args")
        == ["--kill-after=2m", "55m", "bash", "scripts/run-nextest-shards.sh"]
        and test_task.get("hermetic") is False,
        "test task no longer runs the bounded shard lane",
    )
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
        if "CUE language version v0.16.1" not in version.stdout.splitlines():
            raise RuntimeError("the publish graph requires CUE 0.16.1")
        cls.workflow = cue_export(WORKFLOW)
        cls.helper = cue_export("./server", "-e", "tasks.ciHelperContracts")
        cls.test = cue_export("./server", "-e", "tasks.nixTestLocal")
        cls.drift = cue_export("./server", "-e", "tasks.checkCiDrift")
        cls.helper_script = (ROOT / "server/scripts/check-ci-helper-contracts.sh").read_text()

    def check(self, workflow=None, helper=None, test=None, drift=None, helper_script=None):
        validate(
            self.workflow if workflow is None else workflow,
            self.helper if helper is None else helper,
            self.test if test is None else test,
            self.drift if drift is None else drift,
            self.helper_script if helper_script is None else helper_script,
        )

    def test_generated_workflow_preserves_the_contract(self):
        self.check()

    def test_missing_or_added_dependency_is_rejected(self):
        for source, target, add in (
            ("nixTestLocal", "ciHelperContracts", False),
            ("publishContainerImage", "ciHelperContracts", False),
            ("publishContainerImage", "nixTestLocal", False),
            ("checkCiDrift", "ciHelperContracts", True),
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

    def test_permissive_conditions_and_error_tolerance_are_rejected(self):
        for name in ("ciHelperContracts", "nixTestLocal", "publishContainerImage"):
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
        test_task = copy.deepcopy(self.test)
        test_task["args"][-1] = "scripts/other.sh"
        with self.assertRaises(ValueError):
            self.check(test=test_task)

    def test_removing_either_contract_invocation_is_rejected(self):
        drift = copy.deepcopy(self.drift)
        drift["args"] = [argument.replace("test_default_publish_graph.py", "other.py")
                         for argument in drift["args"]]
        with self.assertRaises(ValueError):
            self.check(drift=drift)
        with self.assertRaises(ValueError):
            self.check(helper_script=self.helper_script.replace("test_default_publish_graph", "other"))


if __name__ == "__main__":
    unittest.main()
