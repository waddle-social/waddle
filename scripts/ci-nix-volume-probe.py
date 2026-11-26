#!/usr/bin/env python3
"""Measure whether a Namespace Nix cache volume keeps Rust layers warm.

The experiment uses no FlakeHub or Hestia: the workflow configures
cache.nixos.org as the only binary cache (toolchains and upstream packages). A seed job builds the
Clippy lane on a tagged Namespace volume; later probe jobs land on whatever
machine Namespace allocates, record which Waddle outputs the volume already
holds, then build the same lane after a one-line waddle-server edit. A warm
volume builds only the lane; a cold one rebuilds dependencies and layers.

Subcommands:
  volume  before Nix is installed: what the mounted /nix contains
  probe   evaluate targets, record local presence, build the lane, time it
  summary join every job's result into one Markdown table
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

SYSTEM = "x86_64-linux"
LANE = f"checks.{SYSTEM}.waddle-server-clippy"
# Each target is the lane attribute path plus an attribute suffix to its
# output. Evaluated, never hardcoded: the probe follows the checked-out tree.
TARGETS = {
    "deps": f"{LANE}.cargoArtifacts.cargoArtifacts.cargoArtifacts",
    "layer-base": f"{LANE}.cargoArtifacts.cargoArtifacts",
    "layer-xmpp": f"{LANE}.cargoArtifacts",
    "vendor": f"checks.{SYSTEM}.waddle-server-cargo-vendor",
}
EDIT_FILE = Path("server/crates/waddle-server/src/lib.rs")
CHECKED = re.compile(r"^(?P<drv>[\w.+-]+)>\s+(?:Checking|Compiling) (?P<crate>[\w-]+) v")


def run(args: list[str], timeout: int | None = None, **kwargs) -> subprocess.CompletedProcess:
    return subprocess.run(args, text=True, capture_output=True, timeout=timeout, **kwargs)


def store_stats(root: Path) -> dict:
    store = root / "store"
    if not store.is_dir():
        return {"store_present": False, "store_paths": 0}
    try:
        names = os.listdir(store)
    except PermissionError:
        listing = run(["sudo", "ls", "-1", str(store)])
        names = listing.stdout.split() if listing.returncode == 0 else []
    paths = [name for name in names if not name.startswith(".")]
    return {
        "store_present": True,
        "store_paths": len(paths),
        "waddle_paths": sum(1 for name in paths if "waddle-server" in name),
    }


def volume(args: argparse.Namespace) -> int:
    result = {"phase": args.phase, **store_stats(Path("/nix"))}
    usage = shutil.disk_usage("/nix") if Path("/nix").exists() else None
    if usage:
        result["volume_used_bytes"] = usage.used
        result["volume_total_bytes"] = usage.total
    write(args.output, "volume.json", result)
    print(json.dumps(result))
    return 0


def evaluate(attribute: str) -> str:
    result = run(["nix", "eval", "--raw", f".#{attribute}.outPath"], timeout=600)
    if result.returncode != 0:
        raise SystemExit(f"cannot evaluate {attribute}: {result.stderr.strip()}")
    return result.stdout.strip()


def present(path: str) -> bool:
    # path-info consults only the local store; it never substitutes.
    return run(["nix", "path-info", path], timeout=120).returncode == 0


def parse_dry_run(text: str) -> dict:
    """Read `nix build --dry-run` output: derivations to build and paths to fetch."""
    built, fetched, section = [], [], None
    for line in text.splitlines():
        if "will be built" in line:
            section = built
        elif "will be fetched" in line:
            section = fetched
        elif section is not None and line.startswith("  /nix/store/"):
            section.append(line.strip().split("/nix/store/", 1)[1].split("-", 1)[1])
        elif line and not line.startswith(" "):
            section = None
    return {"built": built, "fetched": fetched}


def checked_members(log: str) -> dict[str, list[str]]:
    """Workspace crates each derivation compiled or checked, from the build log."""
    members: dict[str, set[str]] = {}
    for line in log.splitlines():
        match = CHECKED.match(line)
        if match and (match["crate"].startswith("waddle-") or match["crate"] in {
            "ai-chatbot", "decision-polls", "github", "jev-judgments", "link-board",
        }):
            members.setdefault(match["drv"], set()).add(match["crate"])
    return {drv: sorted(crates) for drv, crates in sorted(members.items())}


def probe(args: argparse.Namespace) -> int:
    started = time.monotonic()
    result: dict = {"phase": args.phase, "edited": args.edit}
    outputs = {name: evaluate(attribute) for name, attribute in TARGETS.items()}
    result["targets"] = {name: {"path": path, "present": present(path)} for name, path in outputs.items()}
    result["eval_seconds"] = round(time.monotonic() - started, 1)

    if args.edit:
        with EDIT_FILE.open("a", encoding="utf-8") as handle:
            handle.write(f"\n// cache volume probe {args.phase} {os.environ.get('GITHUB_RUN_ID', 'local')}\n")

    # The workflow pins substituters at installation; record what applied.
    config = run(["nix", "config", "show", "substituters"], timeout=60)
    result["substituters"] = config.stdout.strip()
    options: list[str] = []
    dry = run(["nix", "build", "--dry-run", "--no-link", *options, f".#{LANE}"], timeout=900)
    result["plan"] = parse_dry_run(dry.stderr)

    build_started = time.monotonic()
    log_path = Path(args.output) / f"build-{args.phase}.log"
    log_path.parent.mkdir(parents=True, exist_ok=True)
    with log_path.open("w", encoding="utf-8") as log:
        build = subprocess.run(
            ["nix", "build", "--no-link", "--print-build-logs", *options, f".#{LANE}"],
            stdout=log, stderr=subprocess.STDOUT, text=True,
        )
    result["build_seconds"] = round(time.monotonic() - build_started, 1)
    result["build_success"] = build.returncode == 0
    log_text = log_path.read_text(encoding="utf-8", errors="replace")
    result["members"] = checked_members(log_text)
    # Without the sandbox each derivation builds in a different directory and
    # Cargo cannot reuse any member layer; record it so a result is not misread.
    result["sandboxed"] = "auto-disabling sandboxing" not in log_text
    result["total_seconds"] = round(time.monotonic() - started, 1)
    write(args.output, "probe.json", result)
    print(json.dumps(result, indent=2))
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as handle:
            handle.write(render([{"volume": read(Path(args.output) / "volume.json"), "probe": result}]))
    return 0 if build.returncode == 0 else 1


def write(directory: str, name: str, value: dict) -> None:
    path = Path(directory)
    path.mkdir(parents=True, exist_ok=True)
    (path / name).write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")


def read(path: Path) -> dict:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {}


def render(rows: list[dict]) -> str:
    lines = [
        "| Phase | Sandbox | Volume paths at start | deps | layer-base | layer-xmpp | Derivations built | Lane compiled | Build s | Result |",
        "|---|---|---:|---|---|---|---:|---|---:|---|",
    ]
    for row in rows:
        volume_state, result = row.get("volume", {}), row.get("probe", {})
        targets = result.get("targets", {})
        mark = lambda name: "hit" if targets.get(name, {}).get("present") else "miss"  # noqa: E731
        # Crates the lane itself compiled; a warm layer leaves only waddle-server.
        compiled = result.get("members", {}).get("waddle-server-clippy", [])
        sandbox = {True: "yes", False: "no"}.get(result.get("sandboxed"), "?")
        lines.append(
            f"| {result.get('phase', volume_state.get('phase', '?'))} "
            f"| {sandbox} "
            f"| {volume_state.get('store_paths', 0)} "
            f"| {mark('deps')} | {mark('layer-base')} | {mark('layer-xmpp')} "
            f"| {len(result.get('plan', {}).get('built', []))} "
            f"| {', '.join(compiled) or 'none'} "
            f"| {result.get('build_seconds', '')} "
            f"| {'ok' if result.get('build_success') else 'FAILED'} |"
        )
    return "\n".join(lines) + "\n"


def summary(args: argparse.Namespace) -> int:
    rows = []
    for directory in sorted(Path(args.input).iterdir()):
        if directory.is_dir():
            rows.append({"volume": read(directory / "volume.json"), "probe": read(directory / "probe.json")})
    rows.sort(key=lambda row: row["probe"].get("phase", row["volume"].get("phase", "")))
    text = "# Namespace Nix volume probe\n\n" + render(rows)
    print(text)
    target = os.environ.get("GITHUB_STEP_SUMMARY")
    if target:
        with open(target, "a", encoding="utf-8") as handle:
            handle.write(text)
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    for name in ("volume", "probe"):
        command = sub.add_parser(name)
        command.add_argument("--phase", required=True)
        command.add_argument("--output", default=".ci/nix-volume")
        if name == "probe":
            command.add_argument("--edit", action="store_true", help="append a comment to waddle-server first")
    command = sub.add_parser("summary")
    command.add_argument("--input", required=True)
    args = parser.parse_args(argv)
    return {"volume": volume, "probe": probe, "summary": summary}[args.command](args)


if __name__ == "__main__":
    sys.exit(main())
