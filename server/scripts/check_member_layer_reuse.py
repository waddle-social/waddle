#!/usr/bin/env python3
"""Check that a layered Cargo build reused its workspace member layers.

Nix member layers (flake.nix `mkMemberLayers`) compile every workspace member
except the ones a lane is expected to rebuild. If the layer and the lane ever
disagree about flags, features, profile or environment, Cargo fingerprints
stop matching and the lane quietly recompiles every member again. This reads
the lane's Cargo JSON messages and fails when a workspace member outside the
allowed set was compiled instead of reused.

Third-party and vendored dependencies come from the separate dependency
artifact; their rebuilt units are reported but do not fail the check.
"""

from __future__ import annotations

import argparse
import json
import sys
import tomllib
from pathlib import Path, PurePosixPath


def classify(manifest: str, root: PurePosixPath) -> str | None:
    """Return the member directory relative to root, or None for dependencies."""
    path = PurePosixPath(manifest)
    try:
        relative = path.parent.relative_to(root)
    except ValueError:
        return None
    if relative.parts and relative.parts[0] == "vendor":
        return None
    return str(relative)


def check(lines, root: PurePosixPath) -> tuple[dict, dict[str, bool]]:
    stats = {
        "member_units_built": 0,
        "member_units_fresh": 0,
        "dependency_units_built": 0,
        "dependency_units_fresh": 0,
    }
    member_compiled: dict[str, bool] = {}
    artifacts = 0
    for number, line in enumerate(lines, 1):
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            message = json.loads(line)
        except json.JSONDecodeError as error:
            raise SystemExit(f"line {number}: invalid Cargo JSON: {error}") from error
        if message.get("reason") != "compiler-artifact":
            continue
        artifacts += 1
        member = classify(message["manifest_path"], root)
        fresh = bool(message["fresh"])
        kind = "member" if member is not None else "dependency"
        stats[f"{kind}_units_{'fresh' if fresh else 'built'}"] += 1
        if member is not None:
            member_compiled[member] = member_compiled.get(member, False) or not fresh
    if artifacts == 0:
        raise SystemExit("no compiler-artifact messages: was the build run with JSON output?")
    return stats, member_compiled


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("messages", type=Path, help="Cargo --message-format json output")
    parser.add_argument("--root", required=True, help="workspace source root, e.g. /build/source")
    parser.add_argument(
        "--rebuild-allowed",
        action="append",
        default=[],
        metavar="DIR",
        help="member directory relative to root that may be compiled (repeatable)",
    )
    parser.add_argument("--lane", default="compile", help="metric label")
    parser.add_argument(
        "--output", type=Path, help="write built and fresh workspace members to JSON"
    )
    parser.add_argument(
        "--report-only",
        action="store_true",
        help="print the metrics without failing (lanes built without member layers)",
    )
    args = parser.parse_args(argv)

    with args.messages.open(encoding="utf-8") as handle:
        stats, member_compiled = check(handle, PurePosixPath(args.root))
    if args.output:
        with (Path(args.root) / "Cargo.toml").open("rb") as manifest:
            members = set(tomllib.load(manifest)["workspace"]["members"])
        missing = members.difference(member_compiled)
        if missing:
            raise SystemExit(f"missing workspace member artifacts: {', '.join(sorted(missing))}")
        report = {
            "built": sorted(m for m in members if member_compiled[m]),
            "fresh": sorted(m for m in members if not member_compiled[m]),
        }
        args.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        for state, paths in report.items():
            print(f"Workspace members {state} ({len(paths)}): {', '.join(paths) or 'none'}")
    fields = " ".join(f"{key}={value}" for key, value in stats.items())
    print(f"WADDLE_CI_METRIC phase={args.lane} member_layer_reuse=1 {fields}")
    unexpected = sorted(m for m, built in member_compiled.items() if built and m not in args.rebuild_allowed)
    if unexpected and not args.report_only:
        print(
            "Workspace members were recompiled although their member layer should have "
            f"provided them: {', '.join(unexpected)}.\n"
            "The layer and this lane no longer share Cargo fingerprints. Compare their "
            "compile command, features, profile and environment in flake.nix "
            "(mkMemberLayers).",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
