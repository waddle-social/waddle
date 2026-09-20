#!/usr/bin/env python3
"""Check whole-binary and explicitly shared nextest shards preserve full coverage.

Pass each phase's inventory (whole-1, shared-1, whole-2, shared-2, ...) and set
--count to the total inventory count. Repeat --shared-binary for IDs intentionally
listed in multiple shards. Every selected test must still occur exactly once.
"""

import argparse
from dataclasses import dataclass
import json
from pathlib import Path


@dataclass
class Inventory:
    binaries: dict
    listed: set
    cases: dict
    selected: set


def inventory(path):
    document = json.loads(Path(path).read_text())
    binaries = {}
    listed = set()
    cases = {}
    selected = set()
    suites = document["rust-suites"]
    for binary_id, suite in suites.items():
        if suite["binary-id"] != binary_id:
            raise ValueError(f"{path}: binary-id differs from its inventory key {binary_id}")
        binaries[binary_id] = tuple(
            suite[field] for field in ("package-name", "binary-name", "kind", "build-platform")
        )
        status = suite["status"]
        if status == "skipped":
            if suite["testcases"]:
                raise ValueError(f"{path}: skipped binary {binary_id} contains testcases")
            continue
        if status != "listed":
            raise ValueError(f"{path}: binary {binary_id} has unknown status {status}")
        listed.add(binary_id)
        for name, case in suite["testcases"].items():
            identity = (binary_id, name)
            if type(case["ignored"]) is not bool:
                raise ValueError(f"{path}: test {identity} has invalid ignored status")
            cases[identity] = (case["ignored"], case["kind"])
            status = case["filter-match"]["status"]
            if status == "matches":
                selected.add(identity)
            elif status != "mismatch":
                raise ValueError(f"{path}: unknown filter status {status}")
    if document["test-count"] != len(cases):
        raise ValueError(f"{path}: test-count disagrees with the inventory")
    return Inventory(binaries, listed, cases, selected)


def check_binary_metadata(expected, actual, path):
    for binary_id, metadata in actual.binaries.items():
        if binary_id not in expected.binaries:
            raise ValueError(f"{path}: unknown binary {binary_id}")
        if metadata != expected.binaries[binary_id]:
            raise ValueError(f"{path}: binary kind or metadata differ for {binary_id}")


def check(expected_path, shard_paths, count=4, shared_binary_ids=()):
    if len(shard_paths) != count:
        raise ValueError(f"expected {count} shards, received {len(shard_paths)}")
    expected = inventory(expected_path)
    shared = set(shared_binary_ids)
    unknown_shared = shared - set(expected.binaries)
    if unknown_shared:
        raise ValueError(f"unknown shared binary IDs: {sorted(unknown_shared)}")
    if expected.listed != set(expected.binaries):
        raise ValueError("the original inventory must list every binary")
    if not expected.selected:
        raise ValueError("the original inventory selects no tests")
    observed_binaries = set()
    observed_tests = set()
    shard_counts = []
    for path in shard_paths:
        shard = inventory(path)
        check_binary_metadata(expected, shard, path)
        duplicate_binaries = (observed_binaries & shard.listed) - shared
        if duplicate_binaries:
            raise ValueError(f"{path}: binaries assigned to multiple shards: {sorted(duplicate_binaries)[:5]}")
        expected_cases = {
            identity: classification for identity, classification in expected.cases.items()
            if identity[0] in shard.listed
        }
        if shard.cases != expected_cases:
            raise ValueError(f"{path}: test identities or ignored status differ from the original")
        if not shard.selected:
            raise ValueError(f"{path}: shard selects no tests")
        duplicate = observed_tests & shard.selected
        if duplicate:
            raise ValueError(f"{path}: tests selected by multiple shards: {sorted(duplicate)[:5]}")
        unexpected = shard.selected - expected.selected
        if unexpected:
            raise ValueError(f"{path}: unexpected selected tests: {sorted(unexpected)[:5]}")
        observed_binaries.update(shard.listed)
        observed_tests.update(shard.selected)
        shard_counts.append(len(shard.selected))
    missing_binaries = set(expected.binaries) - observed_binaries
    if missing_binaries:
        raise ValueError(f"binaries missing from the shard union: {sorted(missing_binaries)[:5]}")
    missing = expected.selected - observed_tests
    if missing:
        raise ValueError(f"tests missing from the shard union: {sorted(missing)[:5]}")
    return {
        "binaries": len(expected.binaries),
        "test_count": len(expected.cases),
        "selected_tests": len(expected.selected),
        "ignored_tests": sum(ignored for ignored, _ in expected.cases.values()),
        "shard_counts": shard_counts,
    }


def check_partition(expected_path, actual_path):
    expected = inventory(expected_path)
    actual = inventory(actual_path)
    check_binary_metadata(expected, actual, actual_path)
    # list -E retains skipped, empty suite records; archive omits those records.
    # Only listed suites belong to this partition, including its empty binaries.
    if (actual.listed, actual.cases, actual.selected) != (expected.listed, expected.cases, expected.selected):
        raise ValueError("partition identities, ignored status or selection differ from the producer")
    return {"binaries": len(actual.listed), "selected_tests": len(actual.selected)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("expected", type=Path)
    parser.add_argument("shards", nargs="+", type=Path)
    parser.add_argument("--count", type=int, default=4)
    parser.add_argument("--shared-binary", action="append", default=[])
    parser.add_argument("--compare-partition", action="store_true")
    args = parser.parse_args()
    try:
        if args.compare_partition:
            if len(args.shards) != 1:
                raise ValueError("partition comparison requires exactly one actual inventory")
            report = check_partition(args.expected, args.shards[0])
        else:
            report = check(args.expected, args.shards, args.count, args.shared_binary)
    except (KeyError, TypeError, ValueError, OSError) as error:
        parser.exit(1, f"nextest shard inventory mismatch: {error}\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
