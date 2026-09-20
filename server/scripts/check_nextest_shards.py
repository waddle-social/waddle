#!/usr/bin/env python3
"""Check that successful nextest shards select the complete original inventory."""

import argparse
import json
from pathlib import Path


def inventory(path):
    document = json.loads(Path(path).read_text())
    cases = {}
    selected = set()
    suites = document["rust-suites"]
    for binary_id, suite in suites.items():
        if suite["status"] != "listed":
            raise ValueError(f"{path}: binary {binary_id} was not listed")
        for name, case in suite["testcases"].items():
            identity = (binary_id, name)
            cases[identity] = (case["ignored"], case.get("kind"))
            status = case["filter-match"]["status"]
            if status == "matches":
                selected.add(identity)
            elif status != "mismatch":
                raise ValueError(f"{path}: unknown filter status {status}")
    if document["test-count"] != len(cases):
        raise ValueError(f"{path}: test-count disagrees with the inventory")
    return set(suites), cases, selected


def check(expected_path, shard_paths, count=4):
    if len(shard_paths) != count:
        raise ValueError(f"expected {count} shards, received {len(shard_paths)}")
    binaries, expected_cases, expected = inventory(expected_path)
    if not expected:
        raise ValueError("the original inventory selects no tests")
    observed = set()
    shard_counts = []
    for path in shard_paths:
        shard_binaries, shard_cases, selected = inventory(path)
        if shard_binaries != binaries or shard_cases != expected_cases:
            raise ValueError(f"{path}: test identities or ignored status differ from the original")
        if not selected:
            raise ValueError(f"{path}: shard selects no tests")
        duplicate = observed & selected
        if duplicate:
            raise ValueError(f"{path}: tests selected by multiple shards: {sorted(duplicate)[:5]}")
        unexpected = selected - expected
        if unexpected:
            raise ValueError(f"{path}: unexpected selected tests: {sorted(unexpected)[:5]}")
        observed.update(selected)
        shard_counts.append(len(selected))
    missing = expected - observed
    if missing:
        raise ValueError(f"tests missing from the shard union: {sorted(missing)[:5]}")
    return {
        "binaries": len(binaries),
        "test_count": len(expected_cases),
        "selected_tests": len(expected),
        "ignored_tests": sum(ignored for ignored, _ in expected_cases.values()),
        "shard_counts": shard_counts,
    }


def check_partition(expected_path, actual_path):
    expected = inventory(expected_path)
    actual = inventory(actual_path)
    if actual != expected:
        raise ValueError("partition identities, ignored status or selection differ from the producer")
    return {"binaries": len(actual[0]), "selected_tests": len(actual[2])}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("expected", type=Path)
    parser.add_argument("shards", nargs="+", type=Path)
    parser.add_argument("--count", type=int, default=4)
    parser.add_argument("--compare-partition", action="store_true")
    args = parser.parse_args()
    try:
        if args.compare_partition:
            if len(args.shards) != 1:
                raise ValueError("partition comparison requires exactly one actual inventory")
            report = check_partition(args.expected, args.shards[0])
        else:
            report = check(args.expected, args.shards, args.count)
    except (KeyError, TypeError, ValueError, OSError) as error:
        parser.exit(1, f"nextest shard inventory mismatch: {error}\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
