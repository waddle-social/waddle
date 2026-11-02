#!/usr/bin/env python3
"""Fail unless every required test executed and passed in the nextest runs.

A Postgres-backed test that returns early reports as a pass, and a test that a
filter, partition or feature gate drops does not report at all. Neither shows
up in an exit code, so this check reads the executed test names from the
nextest JUnit reports of every shard and compares them with a committed list.

The required list holds one `<binary-id> <test-name>` per line, exactly as
nextest prints them (for example `waddle-server::clustering_cluster_e2e
lone_survivor_and_isolation_fencing`, or `waddle-server
ingress::xep0045_room_manifest_tests::postgres_...` for the library's unit
tests). `#` starts a comment; blank lines are ignored.
"""
from __future__ import annotations

import argparse
from dataclasses import dataclass
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys
import xml.etree.ElementTree as ElementTree

NOT_EXECUTED = "skipped"
FAILED = ("failure", "error", "flakyFailure", "flakyError", "rerunFailure", "rerunError")
LANE_ID = "server.required-postgres-multiprocess-websocket-nextest"

# These anchors keep the release-evidence lane tied to its real service and
# transport boundaries. The full required list remains the source of test
# outcomes; these IDs make an accidental removal of one of those boundaries a
# hard error rather than a smaller passing inventory.
PREREQUISITE_ANCHORS = {
    "postgres": (
        "waddle-server::sqlx_cancelled_begin "
        "cancelled_ordinary_begin_never_reuses_an_open_postgres_transaction",
    ),
    "multiprocess": (
        "waddle-server::db_lineage_multiprocess two_replicas_same_database_both_ready",
    ),
    "cross_node_resume_websocket": (
        "waddle-server::clustering_cluster_e2e cross_node_resume_live_steal_handshake",
    ),
    "relay_websocket": (
        "waddle-server::clustering_cluster_e2e muc_join_routes_to_foreign_room_owner",
        "waddle-server::clustering_cluster_e2e "
        "groupchat_fanout_reaches_foreign_node_occupant_and_terminalizes",
    ),
}


@dataclass(frozen=True, order=True)
class TestId:
    binary: str
    name: str

    def __str__(self) -> str:
        return f"{self.binary} {self.name}"


class InputError(Exception):
    pass


def parse_required(path: Path) -> list[TestId]:
    required: list[TestId] = []
    seen: set[TestId] = set()
    try:
        lines = path.read_text().splitlines()
    except OSError as error:
        raise InputError(f"{path}: cannot read required-test list") from error
    for number, raw in enumerate(lines, start=1):
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        fields = line.split()
        if len(fields) != 2:
            raise InputError(f"{path}:{number}: expected '<binary-id> <test-name>', got {raw!r}")
        test = TestId(*fields)
        if test in seen:
            raise InputError(f"{path}:{number}: duplicate required test {test}")
        seen.add(test)
        required.append(test)
    if not required:
        raise InputError(f"{path}: the required test list is empty")
    return required


def executed_tests(reports: list[Path]) -> tuple[set[TestId], set[TestId], set[TestId], dict[Path, str]]:
    """Return (executed, failed, skipped) test ids across JUnit reports."""
    executed: set[TestId] = set()
    failed: set[TestId] = set()
    skipped: set[TestId] = set()
    report_digests: dict[Path, str] = {}
    for report in reports:
        try:
            contents = report.read_bytes()
            root = ElementTree.fromstring(contents)
        except (ElementTree.ParseError, OSError) as error:
            raise InputError(f"{report}: unreadable JUnit report: {error}") from error
        report_digests[report.resolve()] = hashlib.sha256(contents).hexdigest()
        if root.tag not in {"testsuites", "testsuite"}:
            raise InputError(f"{report}: expected a JUnit <testsuites> or <testsuite> document")
        for case in root.iter("testcase"):
            binary, name = case.get("classname"), case.get("name")
            if not binary or not name:
                raise InputError(f"{report}: testcase without classname or name")
            test = TestId(binary, name)
            if case.find(NOT_EXECUTED) is not None:
                skipped.add(test)
                continue
            executed.add(test)
            if any(case.find(tag) is not None for tag in FAILED):
                failed.add(test)
    return executed, failed, skipped, report_digests


EXECUTED_PER_BINARY_LIMIT = 500


def report_failure(missing: list[TestId], failing: list[TestId], executed: set[TestId]) -> None:
    for test in missing:
        print(f"required test did not run: {test}", file=sys.stderr)
    for test in failing:
        print(f"required test failed: {test}", file=sys.stderr)
    # A renamed or moved test shows up as a near-identical executed name.
    for binary in sorted({test.binary for test in missing}):
        names = sorted(test.name for test in executed if test.binary == binary)
        print(f"executed in {binary}: {len(names)} tests", file=sys.stderr)
        for name in names[:EXECUTED_PER_BINARY_LIMIT]:
            print(f"  {binary} {name}", file=sys.stderr)
        if len(names) > EXECUTED_PER_BINARY_LIMIT:
            print(f"  ... {len(names) - EXECUTED_PER_BINARY_LIMIT} more", file=sys.stderr)


def git_output(root: Path, *arguments: str) -> str:
    try:
        result = subprocess.run(
            ["git", "-C", str(root), *arguments],
            text=True,
            capture_output=True,
            check=False,
        )
    except OSError as error:
        raise InputError(f"cannot inspect candidate Git state: {error}") from error
    if result.returncode != 0:
        raise InputError("cannot inspect candidate Git state")
    return result.stdout


def candidate_sha(root: Path, expected: str) -> str:
    if not re.fullmatch(r"[0-9a-f]{40}", expected):
        raise InputError("candidate SHA must be a full lowercase 40-character Git SHA")
    actual = git_output(root, "rev-parse", "--verify", "HEAD^{commit}").strip()
    if actual != expected:
        raise InputError(f"candidate SHA mismatch: expected {expected}, found {actual}")

    # The shard runner writes its JUnit and sidecar outputs under this exact
    # generated directory after tests start. Exclude those artifacts, while
    # still checking every tracked file and every other untracked source file.
    for arguments, label in ((["diff", "--cached", "--quiet", "HEAD", "--"], "index"),
                             (["diff", "--quiet", "--"], "worktree")):
        try:
            diff = subprocess.run(
                ["git", "-C", str(root), *arguments],
                check=False,
            )
        except OSError as error:
            raise InputError(f"cannot inspect candidate Git state: {error}") from error
        if diff.returncode != 0:
            if diff.returncode == 1:
                raise InputError(f"candidate {label} is dirty")
            raise InputError(f"cannot inspect candidate {label}")
    untracked = git_output(
        root,
        "ls-files",
        "--others",
        "--exclude-standard",
        "-z",
        "--",
        ".",
        ":(exclude)server/.ci/nextest-archive/**",
    )
    if untracked:
        raise InputError("candidate worktree has untracked files outside the nextest evidence directory")
    return actual


def repo_relative_file(path: Path, root: Path) -> tuple[Path, str, str]:
    try:
        resolved = path.resolve(strict=True)
    except OSError as error:
        raise InputError(f"{path}: cannot read evidence input") from error
    try:
        relative = resolved.relative_to(root)
    except ValueError as error:
        raise InputError("evidence inputs must be inside the candidate repository") from error
    if not resolved.is_file():
        raise InputError(f"{path}: evidence input is not a regular file")
    return resolved, relative.as_posix(), sha256(resolved)


def repo_relative_output(path: Path, root: Path) -> Path:
    resolved = path.resolve(strict=False)
    try:
        relative = resolved.relative_to(root)
    except ValueError as error:
        raise InputError("evidence output must be inside the candidate repository") from error
    if relative.as_posix() != "server/.ci/nextest-archive/qualification-evidence.json":
        raise InputError("evidence output must use the nextest qualification-evidence path")
    if not resolved.parent.is_dir():
        raise InputError("evidence output directory does not exist")
    if resolved.exists():
        raise InputError("evidence output already exists")
    return resolved


def validate_lane_inputs(root: Path, required_path: Path, report_paths: list[Path]) -> None:
    expected_required = root / "server/.config/required-cluster-tests.txt"
    try:
        actual_required = required_path.resolve(strict=True)
        expected_required = expected_required.resolve(strict=True)
    except OSError as error:
        raise InputError("required PostgreSQL lane inventory is missing") from error
    if actual_required != expected_required:
        raise InputError("evidence must use the committed PostgreSQL lane test inventory")

    expected_reports = {
        root / f"server/.ci/nextest-archive/shard-{partition}/junit-{selection}.xml"
        for partition in range(1, 5)
        for selection in ("whole", "shared")
    }
    try:
        actual_reports = {path.resolve(strict=True) for path in report_paths}
    except OSError as error:
        raise InputError("one or more required nextest JUnit reports are missing") from error
    if len(report_paths) != len(expected_reports) or actual_reports != expected_reports:
        raise InputError("evidence requires all eight shard JUnit reports from the required lane")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    try:
        with path.open("rb") as source:
            for chunk in iter(lambda: source.read(1024 * 1024), b""):
                digest.update(chunk)
    except OSError as error:
        raise InputError(f"{path}: cannot hash evidence input") from error
    return digest.hexdigest()


def parse_test_id(value: str) -> TestId:
    binary, separator, name = value.partition(" ")
    if not separator:
        raise InputError("invalid required-test prerequisite anchor")
    return TestId(binary, name)


def validate_anchor_inventory(required: list[TestId]) -> None:
    required_set = set(required)
    for name, anchor_ids in PREREQUISITE_ANCHORS.items():
        for anchor_id in anchor_ids:
            test = parse_test_id(anchor_id)
            if test not in required_set:
                raise InputError(f"required test inventory is missing {name} prerequisite anchor: {test}")


def required_test_outcomes(
    required: list[TestId], executed: set[TestId], failed: set[TestId], skipped: set[TestId]
) -> list[dict[str, str]]:
    outcomes = []
    for test in sorted(required):
        if test in skipped:
            outcome = "skipped"
        elif test in failed:
            outcome = "failed"
        elif test in executed:
            outcome = "passed"
        elif test in skipped:
            outcome = "skipped"
        else:
            outcome = "missing"
        outcomes.append({"test": str(test), "outcome": outcome})
    return outcomes


def build_evidence(
    *,
    root: Path,
    expected_sha: str,
    required_path: Path,
    report_paths: list[Path],
    required: list[TestId],
    executed: set[TestId],
    failed: set[TestId],
    skipped: set[TestId],
    report_digests: dict[Path, str],
) -> dict[str, object]:
    actual_sha = candidate_sha(root, expected_sha)
    _, required_relative, required_digest = repo_relative_file(required_path, root)
    report_files: list[tuple[str, str]] = []
    for path in report_paths:
        resolved, relative, _ = repo_relative_file(path, root)
        digest = report_digests.get(resolved)
        if digest is None:
            raise InputError(f"{path}: report bytes were not captured during parsing")
        if sha256(resolved) != digest:
            raise InputError(f"{path}: JUnit report changed after parsing")
        report_files.append((relative, digest))
    report_files.sort()

    required_set = set(required)
    executed_outcomes = {
        parse_test_id(outcome["test"]): outcome["outcome"]
        for outcome in required_test_outcomes(required, executed, failed, skipped)
    }
    anchors: dict[str, dict[str, object]] = {}
    for name, anchor_ids in PREREQUISITE_ANCHORS.items():
        anchor_tests = [parse_test_id(anchor) for anchor in anchor_ids]
        if any(test not in required_set for test in anchor_tests):
            validate_anchor_inventory(required)
        incomplete = [test for test in anchor_tests if executed_outcomes.get(test) != "passed"]
        if incomplete:
            raise InputError(f"required reports did not pass {name} prerequisite anchor: {incomplete[0]}")
        anchors[name] = {"outcome": "passed", "tests": [str(test) for test in anchor_tests]}

    return {
        "schema_version": 1,
        "lane_id": LANE_ID,
        "candidate": {"commit_sha": actual_sha, "worktree": "clean"},
        "inputs": {
            "required_tests": {
                "path": required_relative,
                "sha256": required_digest,
            },
            "junit_reports": [
                {"path": path, "sha256": digest} for path, digest in report_files
            ],
        },
        "required_test_gate": {
            "outcome": "passed",
            "required_count": len(required),
            "executed_count": len(executed),
            "required_tests": required_test_outcomes(required, executed, failed, skipped),
            "executed_inventory": [str(test) for test in sorted(executed)],
        },
        "prerequisite_anchors": anchors,
        "qualification": "not_assessed",
        "unverified": {
            "image": "unverified",
            "topology": "unverified",
            "resource_enforcement": "unverified",
            "workload": "unverified",
            "faults": "unverified",
            "deployment": "unverified",
        },
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--required", type=Path, required=True, help="committed required-test list")
    parser.add_argument("--executed-out", type=Path, help="write every executed test name here")
    parser.add_argument("--evidence-out", type=Path,
                        help="write a clean-candidate evidence sidecar after a complete pass")
    parser.add_argument("--candidate-sha",
                        help="expected full Git SHA; required with --evidence-out")
    parser.add_argument("reports", type=Path, nargs="+", help="nextest JUnit reports")
    args = parser.parse_args()
    try:
        required = parse_required(args.required)
        executed, failed, skipped, report_digests = executed_tests(args.reports)
        evidence_output = None
        evidence_root = None
        if args.evidence_out is not None:
            if not args.candidate_sha:
                raise InputError("--candidate-sha is required with --evidence-out")
            evidence_root = Path(
                git_output(Path.cwd(), "rev-parse", "--show-toplevel").strip()
            ).resolve()
            evidence_output = repo_relative_output(args.evidence_out, evidence_root)
            candidate_sha(evidence_root, args.candidate_sha)
            validate_lane_inputs(evidence_root, args.required, args.reports)
            validate_anchor_inventory(required)
    except InputError as error:
        print(error, file=sys.stderr)
        return 2
    if args.executed_out:
        args.executed_out.write_text("".join(f"{test}\n" for test in sorted(executed)))
    missing = [test for test in required if test not in executed or test in skipped]
    failing = [test for test in required if test in failed]
    if missing or failing:
        # On main the diagnostics artifact is uploaded only when the lane
        # passes, so the log must carry what is needed to act on a failure.
        report_failure(missing, failing, executed)
        print(f"{len(missing) + len(failing)} of {len(required)} required tests did not pass "
              f"({len(executed)} tests executed in {len(args.reports)} reports)", file=sys.stderr)
        return 1
    if evidence_output is not None:
        try:
            assert evidence_root is not None
            record = build_evidence(
                root=evidence_root,
                expected_sha=args.candidate_sha,
                required_path=args.required,
                report_paths=args.reports,
                required=required,
                executed=executed,
                failed=failed,
                skipped=skipped,
                report_digests=report_digests,
            )
            evidence_output.write_text(json.dumps(record, indent=2, sort_keys=True) + "\n")
        except (InputError, OSError) as error:
            print(f"{evidence_output}: cannot write evidence sidecar: {error}", file=sys.stderr)
            return 2
    print(f"All {len(required)} required tests executed and passed "
          f"({len(executed)} tests executed in {len(args.reports)} reports)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
