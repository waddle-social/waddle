#!/usr/bin/env python3
"""Inspect all observed Actions workflows for one exact commit, without gh.

Examples:
  python3 scripts/ci-timings.py --repo waddle-social/waddle --sha FULL_SHA
  python3 scripts/ci-timings.py --runs runs.json --jobs jobs-*.json --sha FULL_SHA

API access is read-only; GITHUB_TOKEN is optional for public repositories. Input
files accept individual records, arrays, or raw workflow_runs/jobs API responses.
All input files must be regular files inside the current working directory after
resolving symlinks. To inspect saved reports elsewhere, change to that directory
and invoke this script by its absolute path. Reports are written only to stdout.
Combine all pages into one complete response. For reruns, export the attempt-specific jobs endpoint and add
run_id/run_attempt to its response wrapper if absent from individual jobs.

Optional --dependencies JSON maps workflow paths to exact displayed job names:
{".github/workflows/ci.yml": {"Build": [], "Test": ["Build"]}}.
An empty dependency list explicitly identifies a root job. Unspecified or
ambiguous dependencies produce unknown eligible wait, never inferred queue time.

Event latency starts at --event-time, or earliest workflow created_at (a proxy
which excludes event delivery before Actions created a run). Rerun attempt time
is separate and never replaces original event latency. Timing stops at the last
job completion, excluding any later check-reporting delay. Only observed Actions
workflows are covered; this is not proof of test inventory or external checks.
"""

import argparse
from datetime import datetime
import json
import os
from pathlib import Path
import re
import sys
from urllib.error import URLError
from urllib.parse import urlencode
from urllib.request import Request, urlopen


def timestamp(value):
    if not value:
        return None
    parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    if parsed.utcoffset() is None:
        raise ValueError(f"timestamp needs a timezone: {value}")
    return parsed


def seconds(start, end):
    if start is None or end is None or end < start:
        return None
    return round((end - start).total_seconds(), 3)


def records(document, key, inherited=None):
    """Flatten raw API pages and propagate explicit attempt provenance."""
    inherited = inherited or {}
    if isinstance(document, list):
        return [item for entry in document for item in records(entry, key, inherited)]
    if not isinstance(document, dict):
        raise ValueError(f"expected {key} records or an API response")
    if key in document:
        if document.get("total_count", len(document[key])) > len(document[key]):
            raise ValueError(f"incomplete {key}; combine all pages first")
        metadata = {**inherited, **{k: document[k] for k in ("run_id", "run_attempt") if k in document}}
        return records(document[key], key, metadata)
    if "id" not in document:
        raise ValueError(f"{key} record has no id")
    return [{**inherited, **document}]


def read_json(path):
    """Keep CLI-selected records within the caller's chosen working directory."""
    root = Path.cwd().resolve()
    candidate = Path(path).resolve(strict=True)
    if not candidate.is_relative_to(root):
        raise ValueError("input file must be inside the current working directory")
    if not candidate.is_file():
        raise ValueError("input must be a regular file")
    return json.loads(candidate.read_text(encoding="utf-8"))


def read_files(paths, key):
    result = []
    for path in paths:
        document = read_json(path)
        result.extend(records(document, key))
    return result


def api_collection(repo, endpoint, key, query=None):
    headers = {"Accept": "application/vnd.github+json", "X-GitHub-Api-Version": "2026-03-10"}
    if os.environ.get("GITHUB_TOKEN"):
        headers["Authorization"] = f"Bearer {os.environ['GITHUB_TOKEN']}"
    result = []
    for page in range(1, 101):
        params = urlencode({**(query or {}), "per_page": 100, "page": page})
        request = Request(f"https://api.github.com/repos/{repo}/{endpoint}?{params}", headers=headers)
        with urlopen(request, timeout=30) as response:
            document = json.load(response)
        result.extend(document[key])
        if len(result) >= document["total_count"]:
            return result
        if not document[key]:
            break
    raise ValueError(f"incomplete API pagination for {endpoint}; no timing claim made")


def fetch(repo, sha):
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repo):
        raise ValueError("--repo must be owner/repository")
    runs = api_collection(repo, "actions/runs", "workflow_runs", {"head_sha": sha})
    jobs = []
    for run in runs:
        attempt = run.get("run_attempt", 1)
        for job in api_collection(repo, f"actions/runs/{run['id']}/attempts/{attempt}/jobs", "jobs"):
            jobs.append({**job, "run_attempt": attempt})
    return runs, jobs


def eligible_wait(job, jobs, graph, origin):
    dependencies = graph.get(job.get("name"))
    if dependencies is None or origin is None:
        return None
    ready = [origin]
    for dependency in dependencies:
        matches = [candidate for candidate in jobs if candidate.get("name") == dependency]
        if len(matches) != 1 or matches[0].get("status") != "completed":
            return None
        finished = timestamp(matches[0].get("completed_at"))
        if finished is None:
            return None
        ready.append(finished)
    return seconds(max(ready), timestamp(job.get("started_at")))


def job_summary(job, jobs, graph, origin):
    steps = [{"name": step.get("name"), "seconds": seconds(timestamp(step.get("started_at")), timestamp(step.get("completed_at")))} for step in job.get("steps", [])]
    return {
        "id": job["id"], "name": job.get("name", str(job["id"])),
        "status": job.get("status"), "conclusion": job.get("conclusion"),
        "started_at": job.get("started_at"), "completed_at": job.get("completed_at"),
        "seconds": seconds(timestamp(job.get("started_at")), timestamp(job.get("completed_at"))),
        "eligible_wait_seconds": eligible_wait(job, jobs, graph, origin),
        "steps": steps,
    }


def summarize(runs, jobs, sha, event_time=None, dependencies=None, budget=900):
    # A later snapshot of the same run supersedes older attempts/statuses.
    latest = {}
    for run in runs:
        if run.get("head_sha") != sha:
            continue
        previous = latest.get(run["id"])
        if previous is None or (run.get("run_attempt", 1), run.get("updated_at", "")) >= (previous.get("run_attempt", 1), previous.get("updated_at", "")):
            latest[run["id"]] = run
    if not latest:
        raise ValueError(f"no workflow runs found for exact SHA {sha}")
    origins = [timestamp(run.get("created_at")) for run in latest.values()]
    if None in origins:
        raise ValueError("workflow run is missing created_at")
    origin = timestamp(event_time) if event_time else min(origins)
    if origin > min(origins):
        raise ValueError("--event-time must not be later than the first workflow creation")
    workflows = []
    for run in latest.values():
        attempt = run.get("run_attempt", 1)
        selected = {job["id"]: job for job in jobs if job.get("run_id") == run["id"] and job.get("run_attempt", 1) == attempt}
        current = list(selected.values())
        created = timestamp(run["created_at"])
        attempt_start = timestamp(run.get("run_started_at"))
        graph = (dependencies or {}).get(run.get("path"), {})
        summaries = [job_summary(job, current, graph, created if attempt == 1 else attempt_start) for job in current]
        finished = [timestamp(job.get("completed_at")) for job in current if job.get("completed_at")]
        end = max(finished) if finished else None
        complete = bool(current) and run.get("status") == "completed" and all(job.get("status") == "completed" and job.get("completed_at") for job in current)
        valid = complete and seconds(created, end) is not None and all(job["seconds"] is not None for job in summaries if job["conclusion"] != "skipped")
        successful = valid and run.get("conclusion") == "success" and all(job.get("conclusion") in ("success", "skipped", "neutral") for job in current)
        workflows.append({
            "id": run["id"], "name": run.get("name", str(run["id"])),
            "path": run.get("path"), "event": run.get("event"),
            "url": run.get("html_url"), "attempt": attempt,
            "status": run.get("status"), "conclusion": run.get("conclusion"),
            "measurement_complete": valid, "successful": successful,
            "created_at": run["created_at"], "last_job_completed_at": end.isoformat() if end else None,
            "event_to_last_job_seconds": seconds(created, end) if valid else None,
            "attempt_seconds": seconds(attempt_start, end) if valid else None,
            "jobs": summaries,
        })
    workflows.sort(key=lambda run: timestamp(run["last_job_completed_at"]) or origin, reverse=True)
    complete = all(run["measurement_complete"] for run in workflows)
    successful = complete and all(run["successful"] for run in workflows)
    end = max((timestamp(run["last_job_completed_at"]) for run in workflows if run["last_job_completed_at"]), default=None)
    elapsed = seconds(origin, end) if complete else None
    return {
        "sha": sha, "origin": origin.isoformat(),
        "origin_source": "supplied event timestamp" if event_time else "earliest workflow created_at proxy",
        "scope": "All supplied Actions workflows for this SHA; external checks, missing workflows, test inventory and check-reporting delay are not verified.",
        "eligible_wait_note": "Dependency-ready to start delay includes orchestration and scheduling; unknown without an explicit dependency graph.",
        "measurement_complete": complete, "successful": successful,
        "first_attempt_successful": successful and all(run["attempt"] == 1 for run in workflows),
        "event_to_last_job_seconds": elapsed, "budget_seconds": budget,
        "under_budget": elapsed < budget if successful else None,
        "critical_finish": {"workflow": workflows[0]["name"], "run_id": workflows[0]["id"]} if complete else None,
        "workflows": workflows,
    }


def duration(value):
    if value is None:
        return "unknown"
    return f"{int(value // 60)}m{int(value % 60):02d}s"


def cell(value):
    return str(value).replace("|", "\\|").replace("\n", " ")


def markdown(report):
    print(f"CI timing for `{report['sha']}`")
    print(f"\nObserved elapsed: **{duration(report['event_to_last_job_seconds'])}**. Successful: **{str(report['successful']).lower()}**. Under {duration(report['budget_seconds'])}: **{report['under_budget'] if report['under_budget'] is not None else 'unproven'}**.")
    print(f"\nOrigin: {report['origin_source']}. {report['scope']}")
    print("\n| Workflow | Outcome | Attempt | Event to last job | Attempt elapsed |")
    print("| --- | --- | ---: | ---: | ---: |")
    for run in report["workflows"]:
        outcome = run["conclusion"] or run["status"] or "unknown"
        if not run["measurement_complete"]:
            outcome += " (incomplete measurement)"
        print(f"| {cell(run['name'])} | {cell(outcome)} | {run['attempt']} | {duration(run['event_to_last_job_seconds'])} | {duration(run['attempt_seconds'])} |")
    if report["critical_finish"]:
        print(f"\nLast workflow to finish: **{cell(report['critical_finish']['workflow'])}**.")
    print("\nLongest 12 observed jobs. " + report["eligible_wait_note"])
    print("\n| Workflow / job | Outcome | Runtime | Eligible wait | Slowest timed step |")
    print("| --- | --- | ---: | ---: | --- |")
    jobs = [(run["name"], job) for run in report["workflows"] for job in run["jobs"]]
    for name, job in sorted(jobs, key=lambda item: item[1]["seconds"] or 0, reverse=True)[:12]:
        steps = [step for step in job["steps"] if step["seconds"] is not None]
        slowest = max(steps, key=lambda step: step["seconds"]) if steps else None
        label = f"{slowest['name']} ({duration(slowest['seconds'])})" if slowest else "unknown"
        print(f"| {cell(name)} / {cell(job['name'])} | {cell(job['conclusion'] or job['status'])} | {duration(job['seconds'])} | {duration(job['eligible_wait_seconds'])} | {cell(label)} |")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--sha", required=True, help="exact head SHA, including the PR merge SHA if appropriate")
    parser.add_argument("--repo", help="fetch all observed workflows and their latest attempt jobs")
    parser.add_argument("--runs", nargs="+", default=[], help="raw workflow run JSON files")
    parser.add_argument("--jobs", nargs="+", default=[], help="raw job JSON files")
    parser.add_argument("--event-time", help="ISO 8601 event timestamp with timezone")
    parser.add_argument("--dependencies", help="JSON mapping workflow paths to job dependency lists")
    parser.add_argument("--budget-minutes", type=float, default=15)
    parser.add_argument("--format", choices=("markdown", "json"), default="markdown")
    args = parser.parse_args()
    if bool(args.repo) == bool(args.runs) or (args.repo and args.jobs):
        parser.error("use either --repo or --runs with --jobs")
    if args.budget_minutes <= 0:
        parser.error("--budget-minutes must be positive")
    try:
        runs, jobs = fetch(args.repo, args.sha) if args.repo else (read_files(args.runs, "workflow_runs"), read_files(args.jobs, "jobs"))
        graph = read_json(args.dependencies) if args.dependencies else None
        report = summarize(runs, jobs, args.sha, args.event_time, graph, args.budget_minutes * 60)
        if args.format == "json":
            print(json.dumps(report, indent=2))
        else:
            markdown(report)
    except (ValueError, KeyError, TypeError, OSError, URLError) as error:
        print(f"ci-timings: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
