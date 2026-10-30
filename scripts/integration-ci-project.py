#!/usr/bin/env python3
"""Require an exact cuenv project and executable pipeline before nested CI."""

import argparse
import json
from pathlib import Path
import subprocess


def verify_project(summary, metadata, requested_path, pipeline):
    root = Path(summary["module_root"]).resolve()
    target = Path(requested_path).resolve()
    target.relative_to(root)
    projects = summary.get("projects")
    if summary.get("project_count") != 1 or not isinstance(projects, list) or len(projects) != 1:
        raise ValueError("expected exactly one discovered project")
    project = projects[0]
    if not isinstance(project.get("name"), str) or not project["name"]:
        raise ValueError("missing project identity")
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
        names_to_run.append(references[0]["cuenvTask"])
    return {"project": project["name"], "path": project["path"], "pipeline": pipeline, "tasks": len(tasks), "task_names": names_to_run}


def read_info(path, metadata=False):
    command = ["cuenv", "info", path, "--json"]
    if metadata:
        command.append("--meta")
    result = subprocess.run(command, check=True, capture_output=True, text=True)
    return json.loads(result.stdout)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--path", required=True)
    parser.add_argument("--pipeline", required=True)
    parser.add_argument("--run", action="store_true", help="execute every attested named task with dependencies")
    args = parser.parse_args()
    try:
        result = verify_project(read_info(args.path), read_info(args.path, metadata=True), args.path, args.pipeline)
        print(json.dumps(result, sort_keys=True), flush=True)
        if args.run:
            for name in result["task_names"]:
                subprocess.run(["cuenv", "task", "--path", args.path, "--", name], check=True)
    except (KeyError, TypeError, ValueError, OSError, subprocess.CalledProcessError) as error:
        raise SystemExit(f"Integration project discovery failed: {error}") from None


if __name__ == "__main__":
    main()
