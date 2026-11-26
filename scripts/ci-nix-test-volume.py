#!/usr/bin/env python3
"""Measure a single-crate edit on fresh runners using the production test derivation."""

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import time
from datetime import datetime
from importlib import import_module
from pathlib import Path

volume = import_module("ci-nix-volume-probe")
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "server/scripts"))
from check_required_tests import InputError, TestId, executed_tests, flaky_tests

LANE = "checks.x86_64-linux.waddle-server-default-test"
PROBE = TestId("waddle-server", "namespace_volume_one_crate_edit")
EDIT = f"\n#[test]\nfn {PROBE.name}() {{ assert_eq!(env!(\"CARGO_PKG_NAME\"), \"waddle-server\"); }}\n"
PHASES = ("seed", "immediate", "delayed", "cold")


def command(*args):
    return subprocess.check_output(args, text=True).strip()


def source_hash():
    return hashlib.sha256(volume.EDIT_FILE.read_bytes()).hexdigest()


def in_store(path):
    # --check-validity queries the local store database without substituting.
    return subprocess.run(
        ["nix-store", "--check-validity", path],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    ).returncode == 0


def layer_state():
    result = {}
    attribute = LANE
    for name in ("lane", "xmpp", "base", "dependencies"):
        path = volume.evaluate(attribute)
        result[name] = {"path": path, "present": in_store(path)}
        attribute += ".cargoArtifacts"
    return result


def inventory(report, edited, build_log):
    executed, failed, skipped, _ = executed_tests([report])
    if not executed or (PROBE in executed) != edited:
        raise ValueError("JUnit inventory does not match the source mutation")
    # The warmed and cold controls must execute the same binaries and tests.
    # Removing only the injected test also lets us compare with the seed.
    # Nextest omits ignored tests from its JUnit XML. Its final console
    # summary includes their count for the same invocation.
    totals = re.findall(r"\bSummary\s+\[[^\n\]]+\] (\d+) tests? run: ([^\n]+)", build_log)
    if len(totals) != 1 or int(totals[0][0]) != len(executed):
        raise ValueError("nextest summary and JUnit executed test counts differ")
    skipped_count = re.search(r"\b(\d+) skipped\b", totals[0][1])
    entries = [(t.binary, t.name, "ran") for t in sorted(executed - {PROBE})]
    entries.extend((t.binary, t.name, "skipped") for t in sorted(skipped))
    return {
        "ran": len(executed),
        "skipped": int(skipped_count[1]) if skipped_count else 0,
        "failed": sorted(map(str, failed)),
        "flaky": sorted(map(str, flaky_tests([report]))),
        "inventory_hash": hashlib.sha256(json.dumps(entries).encode()).hexdigest(),
    }


def probe(args):
    out = Path(".ci/nix-volume")
    out.mkdir(parents=True, exist_ok=True)
    result = {
        "phase": args.phase,
        "revision": command("git", "rev-parse", "HEAD"),
        "runner": os.environ["RUNNER_NAME"],
        "source_before": source_hash(),
    }
    try:
        # The Determinate installer appends its own substituter to nix.conf.
        # Scope every Nix client in this measurement to the upstream cache.
        config = os.environ.get("NIX_CONFIG", "")
        os.environ["NIX_CONFIG"] = config + "\nsubstituters = https://cache.nixos.org/\n"
        if args.edit != (args.phase != "seed"):
            raise ValueError("invalid mutation selection")
        unedited_path = volume.evaluate(LANE)
        result["unedited_lane"] = {"path": unedited_path, "present": in_store(unedited_path)}
        build_dir = command("nix", "config", "show", "build-dir")
        result["free_bytes"] = {p: shutil.disk_usage(p).free for p in ("/nix", build_dir)}
        if command("git", "diff", "--name-only", "HEAD"):
            raise ValueError("benchmark requires a clean tracked checkout")
        if args.edit:
            with volume.EDIT_FILE.open("a") as source:
                source.write(EDIT)
        changed = command("git", "diff", "--name-only", "HEAD").splitlines()
        if changed != ([str(volume.EDIT_FILE)] if args.edit else []):
            raise ValueError(f"unexpected files changed: {changed}")
        result["source_after"] = source_hash()
        result["substituters"] = command("nix", "config", "show", "substituters").split()
        if {url.rstrip("/") for url in result["substituters"]} != {"https://cache.nixos.org"}:
            raise ValueError("unexpected substituter in Namespace-only benchmark")
        if command("nix", "config", "show", "sandbox") != "true" or command("nix", "config", "show", "sandbox-fallback") != "false":
            raise ValueError("a required Nix sandbox is missing")
        result["start"] = layer_state()
        if result["start"]["lane"]["present"]:
            raise ValueError("complete test output already present before build")
        if args.phase in ("seed", "cold") and any(x["present"] for x in result["start"].values()):
            raise ValueError("isolated cold store contains seed outputs")
        lane = f".#{LANE}"
        dry = subprocess.run(["nix", "build", "--dry-run", "--no-link", lane], capture_output=True, text=True, check=True)
        result["plan"] = volume.parse_dry_run(dry.stderr)
        started = time.monotonic()
        log = out / "build.log"
        with log.open("w") as handle, subprocess.Popen(
            ["nix", "build", "--no-link", "--keep-failed", "--print-build-logs", lane],
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, errors="replace",
        ) as build:
            for line in build.stdout:
                handle.write(line)
                if line.startswith("building ") or any(word in line for word in ("WADDLE_CI_METRIC", "Summary [", "error:", " FAIL ")):
                    print(line, end="", flush=True)
            build.wait()
        result["build_seconds"] = round(time.monotonic() - started, 2)
        log_text = log.read_text(errors="replace")
        result["compiles_by_derivation"] = volume.checked_members(log_text)
        if build.returncode:
            print("\n".join(log_text.splitlines()[-80:]), file=sys.stderr)
            # A failed derivation has no output. On this isolated runner Nix
            # retains the failing sandbox, including any completed JUnit report.
            reports = command("sudo", "find", build_dir, "-type", "f", "-path", "*/nextest/ci/junit.xml").splitlines()
            if len(reports) == 1:
                with (out / "junit.xml").open("wb") as xml:
                    subprocess.run(["sudo", "cat", reports[0]], stdout=xml, check=True)
                result["tests"] = inventory(out / "junit.xml", args.edit, log_text)
            raise ValueError(f"Nix test build exited {build.returncode}; full failures are in build.log")
        store_out = Path(result["start"]["lane"]["path"])
        for file in ("member-reuse.json", "junit.xml"):
            shutil.copyfile(store_out / file, out / file)
        result["tests"] = inventory(out / "junit.xml", args.edit, log_text)
        result["members"] = json.loads((out / "member-reuse.json").read_text())
        if result["tests"]["failed"]:
            raise ValueError("test failures recorded in JUnit")
    except (InputError, OSError, ValueError, RuntimeError, subprocess.CalledProcessError) as exc:
        result["error"] = str(exc)
    finally:
        volume.write(str(out), "suite.json", result)
        print(json.dumps(result, indent=2))
    return int("error" in result)


def validate(rows):
    errors = []
    for phase in PHASES:
        row = rows.get(phase, {})
        if not row.get("tests") or row.get("error"):
            errors.append(f"{phase}: {row.get('error', 'missing completed measurements')}")
    if errors:
        return errors
    for key in ("revision", "source_before"):
        if len({r[key] for r in rows.values()}) != 1:
            errors.append(f"runner {key} values differ")
    seed = rows["seed"]
    if len({r["runner"] for r in rows.values()}) != len(PHASES):
        errors.append("each phase must run on a distinct runner")
    if len({r["tests"]["inventory_hash"] for r in rows.values()}) != 1:
        errors.append("executed or skipped tests differ across phases")
    if len({r["tests"]["skipped"] for r in rows.values()}) != 1:
        errors.append("skipped test counts differ across phases")
    edited = [rows[p] for p in PHASES if p != "seed"]
    if len({r["source_after"] for r in edited}) != 1 or edited[0]["source_after"] == seed["source_after"]:
        errors.append("expected identical one-file edits in all three comparisons")
    if len({r["start"]["lane"]["path"] for r in edited}) != 1 or edited[0]["start"]["lane"]["path"] == seed["start"]["lane"]["path"]:
        errors.append("the compared test derivations differ or the edit did not invalidate the seed")
    if any(r["unedited_lane"]["path"] != seed["start"]["lane"]["path"] for r in edited):
        errors.append("the unedited lane does not match the seed output path")
    for layer in ("dependencies", "base", "xmpp"):
        if any(r["start"][layer]["path"] != seed["start"][layer]["path"] for r in edited):
            errors.append(f"{layer}: output identity differs across phases")
        if not rows["delayed"]["start"][layer]["present"]:
            errors.append(f"delayed: {layer} was missing from the seeded volume")
    for phase in ("immediate", "delayed"):
        row = rows[phase]
        if all(row["start"][layer]["present"] for layer in ("dependencies", "base", "xmpp")):
            # When all required inputs are resident, only the final derivation may build.
            plan = row["plan"]["built"]
            if len(plan) != 1 or "waddle-server-default-test-nextest-" not in plan[0]:
                errors.append(f"{phase}: expected only the edited production test derivation to build, observed {plan}")
            if any("members-base" in drv or "members-xmpp" in drv for drv in row["compiles_by_derivation"]):
                errors.append(f"{phase}: observed lower-layer compilation on a warm volume")
            if row["members"]["built"] != ["crates/waddle-server"]:
                errors.append(f"{phase}: unexpected workspace builds: {row['members']['built']}")
    return errors


def summary(args):
    jobs = {j["name"]: j for page in json.loads(Path(args.jobs).read_text()) for j in page["jobs"]}
    roots = Path(args.input)
    rows = {p: volume.read(roots / f"nix-test-volume-{p}" / "suite.json") for p in PHASES}
    errors = validate(rows)
    lines = [
        "## Default-feature workspace test suite",
        "",
        "| Phase | Unedited lane | Layer hits (deps / base / XMPP) | Final crate builds / reuse | Ran / skipped / flaky | Nix build | Job wall |",
        "|---|---|---|---|---|---:|---:|",
    ]
    wall_times = {}
    for phase, row in rows.items():
        job = jobs.get(f"nix-test-volume/{phase}", {})
        wall = "—"
        if job.get("completed_at") and job.get("started_at"):
            wall_times[phase] = int((datetime.fromisoformat(job["completed_at"]) - datetime.fromisoformat(job["started_at"])).total_seconds())
            wall = f"{wall_times[phase]}s"
        if job.get("conclusion") != "success" or job.get("runner_name") != row.get("runner"):
            errors.append(f"{phase}: runner identity or successful job completion could not be confirmed")
        inputs = row.get("start", {})
        hits = " / ".join(("hit" if inputs[k]["present"] else "miss") if k in inputs else "?" for k in ("dependencies", "base", "xmpp"))
        tests, members = row.get("tests"), row.get("members")
        crates = f"{len(members['built'])} / {len(members['fresh'])}" if members else "—"
        cases = f"{tests['ran']} / {tests['skipped']} / {len(tests['flaky'])}" if tests else "—"
        build = f"{row['build_seconds']}s" if "build_seconds" in row else "—"
        exact = {True: "hit", False: "miss"}.get(row.get("unedited_lane", {}).get("present"), "?")
        if tests and tests["failed"]:
            errors.append(f"{phase}: failed tests: {', '.join(tests['failed'])}")
        lines.append(f"| [{phase}]({job.get('html_url', '')}) | {exact} | {hits} | {crates} | {cases} | {build} | {wall} |")
    if "cold" in wall_times and "delayed" in wall_times:
        saved = wall_times["cold"] - wall_times["delayed"]
        lines += ["", f"Cold job minus delayed warm job: {saved}s, including runner setup, restore and post-job work."]
        if saved <= 0:
            errors.append("the delayed warm job did not beat the cold job on end-to-end runner time")
    else:
        errors.append("cold or delayed job timing is missing")
    if errors:
        lines += ["", *[f"- {error}" for error in errors]]
    else:
        cold, warm = (rows[phase]["build_seconds"] for phase in ("cold", "delayed"))
        lines += ["", f"Warm volume saved {cold - warm:.1f}s versus an isolated cold volume for the same edited source."]
    text = "\n".join(lines) + "\n"
    print(text)
    with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as output:
        output.write(text)
    return int(bool(errors))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    trial = sub.add_parser("probe")
    trial.add_argument("--phase", choices=PHASES, required=True)
    trial.add_argument("--edit", action="store_true")
    report = sub.add_parser("summary")
    report.add_argument("--input", required=True)
    report.add_argument("--jobs", required=True)
    opts = parser.parse_args()
    sys.exit(probe(opts) if opts.command == "probe" else summary(opts))
