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
from pathlib import Path
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


def parse_required(path: Path) -> tuple[list[TestId], str]:
    required: list[TestId] = []
    seen: set[TestId] = set()
    try:
        contents = path.read_bytes()
        lines = contents.decode("utf-8").splitlines()
    except (OSError, UnicodeDecodeError) as error:
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
    return required, hashlib.sha256(contents).hexdigest()


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
        for parent in root.iter():
            for child in parent:
                if child.tag == "testsuites":
                    raise InputError(f"{report}: testsuites must be the root element")
                if child.tag == "testcase" and parent.tag != "testsuite":
                    raise InputError(f"{report}: testcase must be a direct child of testsuite")
                if child.tag == "testsuite" and parent.tag not in {"testsuites", "testsuite"}:
                    raise InputError(f"{report}: testsuite must be inside testsuites or testsuite")
                if child.tag in (NOT_EXECUTED, *FAILED) and parent.tag != "testcase":
                    raise InputError(f"{report}: test outcome must be a direct child of testcase")
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
        else:
            outcome = "missing"
        outcomes.append({"test": str(test), "outcome": outcome})
    return outcomes


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--required", type=Path, required=True, help="committed required-test list")
    parser.add_argument("--executed-out", type=Path, help="write every executed test name here")
    parser.add_argument("reports", type=Path, nargs="+", help="nextest JUnit reports")
    args = parser.parse_args()
    try:
        required, _ = parse_required(args.required)
        executed, failed, skipped, _ = executed_tests(args.reports)
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
    print(f"All {len(required)} required tests executed and passed "
          f"({len(executed)} tests executed in {len(args.reports)} reports)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
