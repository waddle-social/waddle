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
from pathlib import Path
import sys
import xml.etree.ElementTree as ElementTree

NOT_EXECUTED = "skipped"
FAILED = ("failure", "error", "flakyFailure", "flakyError", "rerunFailure", "rerunError")


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
    for number, raw in enumerate(path.read_text().splitlines(), start=1):
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


def executed_tests(reports: list[Path]) -> tuple[set[TestId], set[TestId]]:
    """Return (executed, failed) test ids across JUnit reports."""
    executed: set[TestId] = set()
    failed: set[TestId] = set()
    for report in reports:
        try:
            root = ElementTree.parse(report).getroot()
        except (ElementTree.ParseError, OSError) as error:
            raise InputError(f"{report}: unreadable JUnit report: {error}") from error
        for case in root.iter("testcase"):
            binary, name = case.get("classname"), case.get("name")
            if not binary or not name:
                raise InputError(f"{report}: testcase without classname or name")
            test = TestId(binary, name)
            if case.find(NOT_EXECUTED) is not None:
                continue
            executed.add(test)
            if any(case.find(tag) is not None for tag in FAILED):
                failed.add(test)
    return executed, failed


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--required", type=Path, required=True, help="committed required-test list")
    parser.add_argument("--executed-out", type=Path, help="write every executed test name here")
    parser.add_argument("reports", type=Path, nargs="+", help="nextest JUnit reports")
    args = parser.parse_args()
    try:
        required = parse_required(args.required)
        executed, failed = executed_tests(args.reports)
    except InputError as error:
        print(error, file=sys.stderr)
        return 2
    if args.executed_out:
        args.executed_out.write_text("".join(f"{test}\n" for test in sorted(executed)))
    missing = [test for test in required if test not in executed]
    failing = [test for test in required if test in failed]
    for test in missing:
        print(f"required test did not run: {test}", file=sys.stderr)
    for test in failing:
        print(f"required test failed: {test}", file=sys.stderr)
    if missing or failing:
        print(f"{len(missing) + len(failing)} of {len(required)} required tests did not pass "
              f"({len(executed)} tests executed in {len(args.reports)} reports)", file=sys.stderr)
        return 1
    print(f"All {len(required)} required tests executed and passed "
          f"({len(executed)} tests executed in {len(args.reports)} reports)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
