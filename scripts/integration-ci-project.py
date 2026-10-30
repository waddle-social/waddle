#!/usr/bin/env python3
"""Require an exact cuenv project and executable pipeline before nested CI."""

import argparse
from collections import Counter
from datetime import datetime
import os
import stat
import time
import json
import hashlib
import re
from pathlib import Path
import subprocess



def validated_project_path(value):
    if not isinstance(value, str) or not re.fullmatch(r"[A-Za-z0-9_./-]+", value) or value.startswith("-"):
        raise ValueError("unsupported project path syntax")
    target = Path(value).resolve(strict=True)
    target.relative_to(Path.cwd().resolve())
    if not target.is_dir():
        raise ValueError("project path is not a directory")
    return str(target)


def validated_task_name(value):
    if not isinstance(value, str) or not re.fullmatch(r"[A-Za-z0-9_-]+(?:\.[A-Za-z0-9_-]+)*", value):
        raise ValueError("unsupported task name syntax")
    return value


def verify_project(summary, metadata, requested_path, pipeline):
    if pipeline not in ("pullRequest", "deviceTests"):
        raise ValueError("only reviewed non-publishing PR pipelines are supported")
    root = Path(summary["module_root"]).resolve()
    target = Path(requested_path).resolve()
    target.relative_to(root)
    projects = summary.get("projects")
    if summary.get("project_count") != 1 or not isinstance(projects, list) or len(projects) != 1:
        raise ValueError("expected exactly one discovered project")
    project = projects[0]
    if not isinstance(project.get("name"), str) or not project["name"]:
        raise ValueError("missing project identity")
    if pipeline == "deviceTests" and (project["path"] != "apps/android" or project["name"] != "waddle-android"):
        raise ValueError("deviceTests is supported only for the reviewed Android project")
    if (root / project["path"]).resolve() != target:
        raise ValueError("discovered project differs from requested path")
    if Path(metadata["module_root"]).resolve() != root:
        raise ValueError("project metadata belongs to another module")
    config = metadata["instances"][project["path"]]
    if config.get("name") != project["name"]:
        raise ValueError("project summary and definition disagree")
    pipeline_config = config.get("ci", {}).get("pipelines", {}).get(pipeline, {})
    if pipeline_config.get("environment") or pipeline_config.get("continueOnError"):
        raise ValueError("pipeline execution options require explicit adapter support")
    tasks = pipeline_config.get("tasks")
    names_to_run = []
    if not isinstance(tasks, list) or not tasks:
        raise ValueError("requested pipeline is absent or has no tasks")
    for task in tasks:
        if not isinstance(task, dict) or not any(isinstance(task.get(key), str) and task[key] for key in ("command", "script")):
            raise ValueError("pipeline contains a non-executable or unsupported task")
        references = [task.get(key) for key in ("stdout", "stderr", "exitCode")]
        if any(not isinstance(ref, dict) or ref.get("cuenvOutputRef") is not True or not isinstance(ref.get("cuenvTask"), str) or not ref["cuenvTask"] for ref in references):
            raise ValueError("pipeline task lacks a named executable binding")
        names = {ref["cuenvTask"] for ref in references}
        if len(names) != 1:
            raise ValueError("pipeline task references disagree")
        binding = config.get("tasks", {})
        for part in references[0]["cuenvTask"].split("."):
            if not part or not isinstance(binding, dict) or part not in binding:
                raise ValueError("pipeline task is absent from the project task index")
            binding = binding[part]
        if binding != task:
            raise ValueError("pipeline task differs from its named executable binding")
        names_to_run.append(validated_task_name(references[0]["cuenvTask"]))
    return {"project": project["name"], "report_project": os.path.join(str(root), project["path"]), "path": project["path"], "pipeline": pipeline, "tasks": len(tasks), "task_names": names_to_run, "definition_sha256": hashlib.sha256(json.dumps(config, sort_keys=True).encode()).hexdigest()}


def expected_context():
    if os.environ.get("BUILDKITE"):
        raise ValueError("unsupported CI provider")
    if os.environ.get("GITHUB_ACTIONS") == "true":
        if not os.environ.get("GITHUB_REPOSITORY"):
            raise ValueError("missing GitHub repository")
        context = {key: os.environ.get(env) for key, env in (
            ("event", "GITHUB_EVENT_NAME"), ("ref_name", "GITHUB_REF_NAME"), ("sha", "GITHUB_SHA"))}
        if context["event"] not in ("pull_request", "merge_group", "workflow_dispatch") or not context["ref_name"] or not re.fullmatch(r"[0-9a-f]{40}", context["sha"] or ""):
            raise ValueError("unsupported or incomplete GitHub context")
        return {"provider": "github", "base_ref": os.environ.get("GITHUB_BASE_REF"), **context}
    return {"provider": "local", "event": "manual", "ref_name": "current", "base_ref": None, "sha": "current"}


def git_head():
    return subprocess.run(["git", "rev-parse", "HEAD"], check=True, capture_output=True, text=True).stdout.strip()


def report_path(target, context):
    return Path.cwd() / ".cuenv" / "reports" / context["sha"] / (target.replace("/", "-").replace("\\", "-") + ".json")


def check_report_parents(path):
    for parent in path.parents:
        if parent.is_symlink() or (parent.exists() and not parent.is_dir()):
            raise ValueError("report parent is not a real directory")


def unique_json_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON key")
        result[key] = value
    return result


def verify_report(report, result, target, context, version, started, finished):
    if report.get("version") != version or report.get("project") != target or report.get("pipeline") != result["pipeline"] or report.get("status") != "success":
        raise ValueError("report invocation identity or pipeline status differs")
    actual = report.get("context", {})
    if any(actual.get(key) != value for key, value in context.items()):
        raise ValueError("report provider/event/ref/SHA identity differs")
    dates = [datetime.fromisoformat(report[key].replace("Z", "+00:00")) for key in ("started_at", "completed_at")]
    if any(date.tzinfo is None for date in dates):
        raise ValueError("report timestamps must have a timezone")
    timestamps = [date.timestamp() for date in dates]
    if not started <= timestamps[0] <= timestamps[1] <= finished:
        raise ValueError("report timestamps are outside this invocation")
    tasks = report.get("tasks")
    if not isinstance(tasks, list) or Counter(task["name"] for task in tasks) != Counter(result["task_names"]):
        raise ValueError("report task inventory differs")
    if any(task.get("status") != "success" or type(task.get("exit_code")) is not int or task["exit_code"] != 0 for task in tasks):
        raise ValueError("report includes unsuccessful, skipped or cached tasks")


def execute_pipeline(result, requested_path):
    target = validated_project_path(requested_path)
    context = expected_context()
    head = git_head()
    if context["provider"] == "github" and context["sha"] != head:
        raise ValueError("checkout differs from GitHub candidate SHA")
    version = subprocess.run(["cuenv", "--version"], check=True, capture_output=True, text=True).stdout.strip().removeprefix("cuenv ")
    help_text = subprocess.run(["cuenv", "ci", "--help"], check=True, capture_output=True, text=True).stdout
    if not re.search(r"(?m)^\s+--all\s", help_text):
        raise ValueError("supported cuenv ci --all release required; current pin cannot activate integration")
    path = report_path(result["report_project"], context)
    check_report_parents(path)
    if path.exists() or path.is_symlink():
        raise ValueError("report already exists before invocation")
    started = time.time()
    subprocess.run(["cuenv", "ci", "--all", "--pipeline", result["pipeline"], "--path", target], check=True)
    finished = time.time()
    if git_head() != head or expected_context() != context:
        raise ValueError("candidate/context changed during invocation")
    after = verify_project(read_info(requested_path), read_info(requested_path, metadata=True), requested_path, result["pipeline"])
    if after != result:
        raise ValueError("project/pipeline inventory changed during invocation")
    check_report_parents(path)
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "r") as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode) or not 0 < info.st_size <= 4 * 1024 * 1024 or not started <= info.st_mtime <= finished:
            raise ValueError("report is not a fresh bounded regular file")
        payload = stream.read(4 * 1024 * 1024 + 1)
        if len(payload) > 4 * 1024 * 1024:
            raise ValueError("report grew beyond size limit")
        report = json.loads(payload, object_pairs_hook=unique_json_object)
    verify_report(report, result, result["report_project"], context, version, started, finished)


def read_info(path, metadata=False):
    target = validated_project_path(path)
    command = ["cuenv", "info", "--json"]
    if metadata:
        command.append("--meta")
    command.extend(["--", target])
    result = subprocess.run(command, check=True, capture_output=True, text=True)
    return json.loads(result.stdout)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--path", required=True)
    parser.add_argument("--pipeline", required=True)
    parser.add_argument("--run", action="store_true", help="execute original CI runner with --all and attest its fresh report")
    args = parser.parse_args()
    try:
        result = verify_project(read_info(args.path), read_info(args.path, metadata=True), args.path, args.pipeline)
        print(json.dumps(result, sort_keys=True), flush=True)
        if args.run:
            execute_pipeline(result, args.path)
    except (KeyError, TypeError, ValueError, OSError, subprocess.CalledProcessError, AttributeError) as error:
        raise SystemExit(f"Integration project discovery failed: {error}") from None


if __name__ == "__main__":
    main()
