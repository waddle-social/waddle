#!/usr/bin/env python3
"""Qualify Nix cache restores without permitting a local or remote build."""

import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys
import tempfile
import time
from urllib.parse import urlparse
from urllib.request import Request, urlopen


TARGETS = {
    "check-deps": "checks.x86_64-linux.waddle-server-check-deps",
    "vendor": "checks.x86_64-linux.waddle-server-cargo-vendor",
}
WORKSPACE_ROOT = Path(__file__).resolve().parent.parent
ARTIFACT_DIRECTORIES = {
    "probe": ".ci/cache-benchmark",
    "input": ".ci/cache-results",
    "summary": ".ci/cache-summary",
}
# Exact archive produced by the completed 02c406d3 trial, not an evaluation of
# today's flake. Hestia intentionally excludes archives, so its miss is useful.
ARCHIVE_SOURCE_SHA = "02c406d38d31976c4c5bed78cfddc0763e460330"
ARCHIVE_PATH = "/nix/store/rbb1dwfaldnawr2pnsb5263hgw2pqf9k-waddle-server-test-archive-0.1.0-shard1"
# FlakeHub's origin and edge endpoints are both observed in the pinned CI
# setup. Keep an exact host allowlist, not a FlakeHub wildcard.
FLAKEHUB_CACHE_HOSTS = {"cache.flakehub.com", "edge.cache.flakehub.com"}
PROVIDERS = {
    "fh-h2": ("fh", "hestia"),
    "fh": ("fh",),
    "h2": ("hestia",),
    "h3": ("hestia",),
    "fh-h3": ("fh", "hestia"),
    "namespace": (),
    "cache-nix": (),
    "magic": ("magic",),
    "magic-seed": ("fh", "magic"),
}


def command(args, **kwargs):
    return subprocess.run(args, text=True, capture_output=True, check=False, **kwargs)


def received_bytes():
    return sum(int(p.read_text()) for p in Path("/sys/class/net").glob("*/statistics/rx_bytes")
               if p.parents[1].name != "lo")


def artifact_path(path):
    """Confine report I/O to the workspace's real, non-symlink .ci tree."""
    path = Path(path)
    if ".." in path.parts:
        raise ValueError("artifact paths cannot contain parent traversal")
    root = WORKSPACE_ROOT.resolve()
    candidate = path if path.is_absolute() else root / path
    try:
        parts = candidate.relative_to(root / ".ci").parts
    except ValueError as error:
        raise ValueError("artifact path must remain inside the workspace .ci directory") from error
    current = root / ".ci"
    for part in (None, *parts):
        if part is not None:
            current = current / part
        if current.is_symlink():
            raise ValueError("artifact paths cannot contain symlinks")
    resolved = candidate.resolve()
    if not resolved.is_relative_to(root / ".ci"):
        raise ValueError("artifact path escaped the workspace .ci directory")
    return resolved


def artifact_directory(value, purpose):
    # CLI paths select a fixed location; they never become a filesystem path.
    expected = ARTIFACT_DIRECTORIES[purpose]
    if value != expected:
        raise ValueError(f"expected artifact directory {expected}")
    return artifact_path(expected)


def write_artifact(path, contents):
    destination = artifact_path(path)
    destination.parent.mkdir(parents=True, exist_ok=True)
    # Replace a completed private file: never truncate a preexisting inode
    # that could be hard-linked elsewhere, or follow a raced leaf symlink.
    descriptor, temporary = tempfile.mkstemp(prefix=".cache-benchmark-", dir=destination.parent)
    try:
        with os.fdopen(descriptor, "w") as stream:
            stream.write(contents)
        os.replace(temporary, destination)
    finally:
        Path(temporary).unlink(missing_ok=True)


def read_artifact_json(path):
    source = artifact_path(path)
    descriptor = os.open(source, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(descriptor) as stream:
        return json.load(stream)


def write_json(path, document):
    write_artifact(path, json.dumps(document, indent=2) + "\n")


def provider_urls(provider, configured):
    """Keep authentication in Nix's existing netrc, never in a report or URL."""
    selected = ["https://cache.nixos.org"]
    for kind in PROVIDERS[provider]:
        matches = []
        for value in configured:
            parsed = urlparse(value)
            if parsed.username or parsed.password:
                raise ValueError("credential-bearing substituter URLs are unsupported")
            if kind == "fh" and parsed.scheme == "https" and parsed.hostname in FLAKEHUB_CACHE_HOSTS:
                matches.append(value)
            elif kind in ("hestia", "magic") and parsed.scheme == "http":
                port = 37516 if kind == "hestia" else 37515
                if parsed.hostname in ("127.0.0.1", "localhost") and parsed.port == port:
                    matches.append(value)
        if not matches:
            raise ValueError(f"required {kind} substituter was not configured")
        selected.extend(matches)
    return selected


def restore_options(urls):
    # Both local and configured remote builds are forbidden. A missing cache
    # result must fail instead of turning this benchmark into a compilation.
    return ["--max-jobs", "0", "--builders", "", "--option", "substitute", "true",
            "--option", "substituters", " ".join(urls),
            "--option", "extra-substituters", "", "--option", "post-build-hook", ""]


def path_sizes(path):
    result = command(["nix", "path-info", "--recursive", "--json", path])
    if result.returncode:
        raise ValueError("could not inspect restored closure")
    values = json.loads(result.stdout)
    entries = list(values.values()) if isinstance(values, dict) else values
    return sum(item["narSize"] for item in entries), len(entries)


def target_output(name, attribute):
    if attribute is None:
        return ARCHIVE_PATH
    evaluated = command(["nix", "eval", "--raw", "--no-write-lock-file", f".#{attribute}.outPath"])
    if evaluated.returncode:
        raise ValueError(f"could not evaluate {name}; no build attempted")
    path = evaluated.stdout.strip()
    if not re.fullmatch(r"/nix/store/[0-9abcdfghijklmnpqrsvwxyz]{32}-[A-Za-z0-9+._?=-]+", path):
        raise ValueError("unexpected evaluated store path")
    return path


def read_archive(path):
    """Materialize the complete archive payload, separately from store validity."""
    archive = Path(path) / "archive.tar.zst"
    before = received_bytes()
    started = time.monotonic()
    report = {"success": False, "bytes_read": 0, "timeout_seconds": 180}
    try:
        report["file_bytes"] = archive.stat().st_size
        # sha256sum streams the file with bounded memory. A process deadline
        # also bounds reads stalled by a remote volume's lazy block fetches.
        result = command(["sha256sum", str(archive)], timeout=180)
        digest = result.stdout.split()[0] if result.stdout.split() else ""
        if result.returncode or not re.fullmatch(r"[0-9a-f]{64}", digest):
            raise ValueError("full archive read did not produce a SHA256 digest")
        report.update(success=True, bytes_read=report["file_bytes"], sha256=digest)
    except (OSError, ValueError, subprocess.TimeoutExpired) as error:
        report["error"] = str(error)
    finally:
        report["seconds"] = round(time.monotonic() - started, 3)
        report["observed_network_rx_bytes"] = received_bytes() - before
    return report


def restore(args):
    directory = artifact_directory(args.output, "probe")
    directory.mkdir(parents=True, exist_ok=True)
    report = {"schema": 1, "variant": args.variant, "phase": args.phase,
              "provider": args.provider, "complete": False, "paths": [],
              "measurement": "restore qualification, not changed-code CI acceptance",
              "run_id": os.environ.get("GITHUB_RUN_ID"),
              "run_attempt": os.environ.get("GITHUB_RUN_ATTEMPT"),
              "archive_source_sha": ARCHIVE_SOURCE_SHA,
              "source_sha": command(["git", "rev-parse", "HEAD"]).stdout.strip()}
    started = time.monotonic()
    try:
        configured = command(["nix", "config", "show", "substituters"])
        if configured.returncode:
            raise ValueError("could not read configured substituters")
        urls = provider_urls(args.provider, shlex.split(configured.stdout))
        options = restore_options(urls)
        # Verify that no extra configured substituter survives CLI overrides.
        effective = command(["nix", *options, "config", "show", "substituters"])
        if effective.returncode or set(shlex.split(effective.stdout)) != set(urls):
            raise ValueError("provider isolation did not take effect")
        for name, attr in [*TARGETS.items(), ("trial-shard1-archive", None)]:
            path = target_output(name, attr)
            present = command(["nix-store", "--check-validity", path]).returncode == 0
            before = received_bytes()
            begin = time.monotonic()
            # Realise the already evaluated output, never a mutable flake ref.
            result = command(["nix-store", "--realise", path, *options])
            duration = time.monotonic() - begin
            restored = result.returncode == 0 and path in result.stdout.splitlines()
            item = {"name": name, "attribute": attr, "path": path,
                    "present_before_restore": present, "success": restored,
                    "outcome": ("local-hit" if present else "substituted") if restored else "miss-or-error",
                    "seconds": round(duration, 3), "exit_code": result.returncode,
                    "observed_network_rx_bytes": received_bytes() - before}
            # Standard Nix build diagnostics contain store paths and source
            # cache hosts, not the credential-bearing configuration or netrc.
            write_artifact(directory / f"{name}.log", result.stderr)
            if restored:
                item["closure_nar_bytes"], item["closure_paths"] = path_sizes(path)
            report["paths"].append(item)
            if restored and name == "trial-shard1-archive":
                item["archive_read"] = read_archive(path)
                if not item["archive_read"]["success"]:
                    raise ValueError("restored archive payload could not be fully read")
        report["complete"] = all(item["success"] for item in report["paths"])
    except (OSError, ValueError, KeyError) as error:
        report["error"] = str(error)
    finally:
        report["probe_seconds"] = round(time.monotonic() - started, 3)
        marker = directory / "started.json"
        if marker.exists():
            initial = read_artifact_json(marker)
            report["setup_and_probe_seconds"] = round(time.monotonic() - initial["monotonic"], 3)
            report["setup_and_probe_network_rx_bytes"] = received_bytes() - initial["network_rx_bytes"]
        write_json(directory / "result.json", report)
        print(json.dumps(report, indent=2))
    # A measured miss is data, distinct from a broken harness/provider setup.
    # The workflow accepts exit 2 but the report keeps complete=false.
    return 1 if "error" in report else (0 if report["complete"] else 2)


def api_json(endpoint):
    headers = {"Accept": "application/vnd.github+json", "X-GitHub-Api-Version": "2022-11-28"}
    if os.environ.get("GITHUB_TOKEN"):
        headers["Authorization"] = f"Bearer {os.environ['GITHUB_TOKEN']}"
    request = Request(f"https://api.github.com/repos/{os.environ['GITHUB_REPOSITORY']}/{endpoint}", headers=headers)
    with urlopen(request, timeout=30) as response:
        return json.load(response)


def seed_hestia(args):
    """Register only successfully restored outputs, then explicitly drain."""
    directory = artifact_directory(args.output, "probe")
    report = {"schema": 1, "variant": "h3-seeded", "phase": "seed", "success": False,
              "cache_save": "unverified until fresh-runner warm restoration"}
    started = time.monotonic()
    try:
        restored = read_artifact_json(directory / "result.json")
        expected = set(TARGETS) | {"trial-shard1-archive"}
        paths = restored["paths"]
        if (not restored["complete"] or len(paths) != len(expected)
                or {p["name"] for p in paths} != expected or not all(p["success"] for p in paths)):
            raise ValueError("all three exact outputs must restore before Hestia seeding")
        # Reports can confirm the expected outputs, never choose upload args.
        # Evaluate only these fixed attributes with builds still prohibited.
        expected_outputs = {name: target_output(name, attr)
                            for name, attr in [*TARGETS.items(), ("trial-shard1-archive", None)]}
        if any(p["path"] != expected_outputs[p["name"]] for p in paths):
            raise ValueError("seed report does not match the exact expected outputs")
        outputs = list(expected_outputs.values())
        binary, socket = os.environ["HESTIA_BIN"], os.environ["HESTIA_SOCKET"]
        hook = command([binary, "hook", "--socket", socket, *outputs], timeout=15)
        hook_log = hook.stdout + hook.stderr
        write_artifact(directory / "hestia-hook.log", hook_log)
        # The supported hook intentionally exits zero even on daemon errors.
        acknowledgement = rf"^hestia hook: registered {len(outputs)} path\(s\), \d+ buffered for upload$"
        if hook.returncode or not re.search(acknowledgement, hook_log, re.MULTILINE):
            raise ValueError("Hestia did not acknowledge every seed output")
        report["registered_paths"] = outputs
        drain_started = time.monotonic()
        drain = command([binary, "drain", "--socket", socket, "--timeout", "300"], timeout=310)
        report["drain_seconds"] = round(time.monotonic() - drain_started, 3)
        report["drain_exit_code"] = drain.returncode
        drain_log = drain.stdout + drain.stderr
        write_artifact(directory / "hestia-drain.log", drain_log)
        manifest = re.search(r"; manifest m3#([1-9][0-9]*)", drain_log)
        if drain.returncode or not manifest or re.search(r"\b(?:invalid|FAILED)\b", drain_log):
            raise ValueError("Hestia seed upload did not report a successful manifest commit")
        report["manifest_version"] = int(manifest.group(1))
        report["success"] = True
        if os.environ.get("GITHUB_OUTPUT"):
            with open(os.environ["GITHUB_OUTPUT"], "a") as stream:
                stream.write(f"manifest-version={report['manifest_version']}\n")
    except (OSError, ValueError, KeyError, subprocess.TimeoutExpired) as error:
        report["error"] = str(error)
    finally:
        report["seconds"] = round(time.monotonic() - started, 3)
        write_json(directory / "hestia-seed.json", report)
        print(json.dumps(report, indent=2))
    return 0 if report["success"] else 1


def api_collection(endpoint, key):
    items = []
    separator = "&" if "?" in endpoint else "?"
    for page in range(1, 101):
        data = api_json(f"{endpoint}{separator}per_page=100&page={page}")
        items.extend(data[key])
        if len(items) >= data["total_count"]:
            return items
    raise ValueError("incomplete API pagination")


def wait_for_ci(args):
    if not re.fullmatch(r"[0-9a-f]{40}", args.sha):
        raise ValueError("expected a full source SHA")
    started = time.monotonic()
    quiet_since = None
    while time.monotonic() - started < args.timeout:
        runs = api_collection(f"actions/runs?head_sha={args.sha}", "workflow_runs")
        other = [r for r in runs if str(r["id"]) != os.environ["GITHUB_RUN_ID"]
                 and r["path"] != ".github/workflows/waddle-ci-cache-benchmark.yml"]
        active = [r for r in other if r["status"] != "completed"]
        if active:
            quiet_since = None
            print("Waiting for normal CI: " + ", ".join(r["name"] for r in active), flush=True)
        else:
            quiet_since = quiet_since or time.monotonic()
            if time.monotonic() - quiet_since >= 60:
                print(f"Normal CI complete ({len(other)} observed workflows); starting serial cache qualification.")
                return 0
        time.sleep(20)
    raise ValueError("normal CI did not finish before the qualification wait deadline")


def elapsed(start, end):
    if not start or not end:
        return None
    return round((datetime.fromisoformat(end.replace("Z", "+00:00")) -
                  datetime.fromisoformat(start.replace("Z", "+00:00"))).total_seconds(), 3)


def summarize(args):
    source = artifact_directory(args.input, "input")
    output = artifact_directory(args.output, "summary")
    results = [read_artifact_json(p) for p in sorted(source.rglob("result.json"))]
    jobs = api_collection(f"actions/runs/{os.environ['GITHUB_RUN_ID']}/jobs?filter=latest", "jobs")
    rows = []
    for job in jobs:
        match = re.fullmatch(r"cache-benchmark/(existing|seed|warm)/([a-z0-9-]+)", job["name"])
        if not match:
            continue
        phase, variant = match.groups()
        matches = [r for r in results if r["phase"] == phase and r["variant"] == variant]
        report = matches[0] if len(matches) == 1 else None
        rows.append({"phase": phase, "variant": variant, "job_id": job["id"],
                     "conclusion": job["conclusion"],
                     "queue_seconds": elapsed(job.get("created_at"), job.get("started_at")),
                     "job_seconds_including_cache_post": elapsed(job.get("started_at"), job.get("completed_at")),
                     "steps": [{"name": s["name"], "conclusion": s["conclusion"],
                                "seconds": elapsed(s.get("started_at"), s.get("completed_at"))}
                               for s in job["steps"]],
                     "complete": bool(report and report["complete"] and job["conclusion"] == "success"),
                     # Cache actions may warn and exit successfully after a
                     # failed upload. Their job conclusion cannot prove a save.
                     "cache_post_status": "unverified; inspect post logs and fresh-runner seed restore",
                     "eligible_for_cache_selection": False,
                     "result": report})
    mismatched_payload = False
    for row in rows:
        if row["phase"] != "warm":
            continue
        seed = next((r for r in rows if r["phase"] == "seed" and r["variant"] == row["variant"]), None)
        def archive_payload(candidate):
            paths = ((candidate or {}).get("result") or {}).get("paths", [])
            return next((p for p in paths if p["name"] == "trial-shard1-archive"), {})
        seed_archive, warm_archive = archive_payload(seed), archive_payload(row)
        seed_read, warm_read = seed_archive.get("archive_read", {}), warm_archive.get("archive_read", {})
        comparison = "unverified; no successful seed and warm payload reads"
        if seed_read.get("success") and warm_read.get("success"):
            matches = (seed_archive["path"] == warm_archive["path"]
                       and seed_read["bytes_read"] == warm_read["bytes_read"]
                       and seed_read["sha256"] == warm_read["sha256"])
            comparison = "matching seed/warm bytes and SHA256" if matches else "MISMATCH"
            if not matches:
                row["complete"] = False
                mismatched_payload = True
        row["archive_seed_warm_comparison"] = comparison
    write_json(output / "summary.json", {"schema": 1, "rows": rows})
    lines = ["# Nix cache restore qualification", "",
             "These probes never compile Waddle and do not prove the 15-minute changed-code CI goal.", "",
             "All probes use 8 vCPU / 16 GB Namespace runners. Serial scheduling intentionally adds queue time.", "",
             "| Phase | Cache | Restore complete | Queue (s) | Job including cache post (s) | Setup + probe (s) |",
             "|---|---|---|---:|---:|---:|"]
    for row in rows:
        report = row["result"] or {}
        values = [row["phase"], row["variant"], "yes" if row["complete"] else "NO",
                  row["queue_seconds"], row["job_seconds_including_cache_post"], report.get("setup_and_probe_seconds")]
        lines.append("| " + " | ".join("unknown" if v is None else str(v) for v in values) + " |")
    lines.extend(["", "## Exact archive transfer", "",
                  f"Archive from `{ARCHIVE_SOURCE_SHA}`: `{ARCHIVE_PATH}`.", "",
                  "Original Actions payload: 1,950,935,616 NAR bytes. Production Hestia excludes this output; the explicit H3 seed pair registers it.", "",
                  "| Phase | Cache | Archive outcome | Restore (s) | Closure NAR bytes | Full payload read (s) | Bytes read | Seed/warm comparison |",
                  "|---|---|---|---:|---:|---:|---:|---|"])
    for row in rows:
        report = row["result"] or {}
        item = next((p for p in report.get("paths", []) if p["name"] == "trial-shard1-archive"), {})
        payload = item.get("archive_read", {})
        lines.append(f"| {row['phase']} | {row['variant']} | {item.get('outcome', 'no result')} | "
                     f"{item.get('seconds', 'unknown')} | {item.get('closure_nar_bytes', 'unknown')} | "
                     f"{payload.get('seconds', 'not measured')} | {payload.get('bytes_read', 'unknown')} | "
                     f"{row.get('archive_seed_warm_comparison', 'not paired')} |")
    lines.extend(["", "Cache-save success is unverified: actions may warn and still succeed. Inspect post logs and confirm each seed through its fresh-runner warm restore before selecting a cache.",
                  "A miss, setup failure, incomplete report or failed cache upload is not a winning result.",
                  "Payload reads force archive blocks to be read; they measure neither extraction nor compilation. Matching seed/warm hashes establish consistency, not an independently trusted digest.",
                  "Closure NAR bytes are uncompressed logical bytes. Network counters include concurrent traffic; they are not compressed cache payload measurements."])
    markdown = "\n".join(lines) + "\n"
    write_artifact(output / "summary.md", markdown)
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as stream:
            stream.write(markdown)
    print(markdown)
    return 1 if mismatched_payload else 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    mark = sub.add_parser("mark")
    mark.add_argument("--output", required=True, choices=[ARTIFACT_DIRECTORIES["probe"]])
    wait = sub.add_parser("wait-for-ci")
    wait.add_argument("--sha", required=True)
    wait.add_argument("--timeout", type=int, default=3600)
    probe = sub.add_parser("restore")
    probe.add_argument("--provider", choices=PROVIDERS, required=True)
    probe.add_argument("--variant", required=True)
    probe.add_argument("--phase", choices=("existing", "seed", "warm"), required=True)
    probe.add_argument("--output", required=True, choices=[ARTIFACT_DIRECTORIES["probe"]])
    seed = sub.add_parser("seed-hestia")
    seed.add_argument("--output", required=True, choices=[ARTIFACT_DIRECTORIES["probe"]])
    summary = sub.add_parser("summarize")
    summary.add_argument("--input", required=True, choices=[ARTIFACT_DIRECTORIES["input"]])
    summary.add_argument("--output", required=True, choices=[ARTIFACT_DIRECTORIES["summary"]])
    args = parser.parse_args()
    if args.command == "wait-for-ci":
        return wait_for_ci(args)
    if args.command == "seed-hestia":
        return seed_hestia(args)
    if args.command == "mark":
        write_json(artifact_directory(args.output, "probe") / "started.json", {"monotonic": time.monotonic(),
                   "utc": datetime.now(timezone.utc).isoformat(), "network_rx_bytes": received_bytes()})
        return 0
    return restore(args) if args.command == "restore" else summarize(args)


if __name__ == "__main__":
    sys.exit(main())
