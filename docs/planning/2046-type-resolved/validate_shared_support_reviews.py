#!/usr/bin/env python3
"""Validate independent reviews of production/support shared compiler sites.

Candidates are the per-profile intersection of the pinned production and
support-library streams. A shared key does not prove identical semantics across
feature contexts, and this check does not copy production classifications or
establish semantic correctness, capture authenticity, or full test coverage.
"""

from __future__ import annotations

import argparse
import csv
import pathlib
import subprocess
import tempfile

from mark_targets import SOURCE_COMMIT
from parse_diagnostics import load_manifest
from validate_inventory import read_table
import validate_support_reviews as support


def derive_candidates(root: pathlib.Path, manifest: list[dict[str, str]], tables: dict[str, dict[str, list[dict[str, str]]]], *, require_pinned_counts: bool = True) -> dict[tuple[str, ...], dict[str, str]]:
    """Intersect independently validated site keys within each feature profile."""
    if set(tables) != set(support.CAPTURES):
        raise ValueError("both shared-site feature profiles are required")
    tags = {row["tag"] for row in manifest}
    candidates = {}
    for profile, expected in support.CAPTURES.items():
        if set(tables[profile]) != {"production", "support"}:
            raise ValueError(f"missing production or support tables for {profile}")
        production = support.site_index(root, tables[profile]["production"], tags, profile, support=False)
        support_sites = support.site_index(root, tables[profile]["support"], tags, profile, support=True)
        missing = set(production) - set(support_sites)
        if missing:
            raise ValueError(f"{profile} support capture omits {len(missing)} production keys")
        shared = set(production) & set(support_sites)
        if require_pinned_counts and (len(production), len(support_sites), len(shared), len(set(support_sites) - set(production))) != (
                expected["production_count"], expected["support_count"], expected["production_count"], expected["additional_count"]):
            raise ValueError(f"{profile} shared-site counts do not match pinned captures")
        for key in shared:
            candidates[(profile, *key)] = support_sites[key]
    return candidates


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-root", required=True, type=pathlib.Path, help="clean source-equivalent checkout for checking review evidence")
    parser.add_argument("--patched-root", required=True, type=pathlib.Path, help="checkout instrumented with the pinned manifest")
    parser.add_argument("--reviews", required=True, action="append", type=pathlib.Path, help="shared-site review TSV; repeat to combine files")
    parser.add_argument("--capture-dir", type=pathlib.Path, default=pathlib.Path(__file__).resolve().parent / "capture-4219bdd")
    args = parser.parse_args()
    root, patched_root, capture_dir = args.source_root.resolve(), args.patched_root.resolve(), args.capture_dir.resolve()
    try:
        manifest_path = capture_dir / "targets.tsv"
        manifest = load_manifest(manifest_path)
        support.validate_source_tree(root, manifest)
        with tempfile.TemporaryDirectory(prefix="w2046-shared-reparse-") as directory:
            tables = support.regenerated_tables(capture_dir, patched_root, manifest_path, pathlib.Path(directory))
            candidates = derive_candidates(root, manifest, tables)
            reviews = [row for path in args.reviews for row in read_table(path, support.REVIEW_FIELDS)]
            support.validate_reviews(root, candidates, reviews)
    except (OSError, EOFError, ValueError, KeyError, TypeError, csv.Error, subprocess.CalledProcessError) as error:
        raise SystemExit(f"shared support-review validation failed: {error}") from error
    counts = {profile: sum(key[0] == profile for key in candidates) for profile in support.CAPTURES}
    print(f"validated {len(reviews)} profile-specific shared compiler-site reviews at {SOURCE_COMMIT}: {counts}")
    print("Semantic correctness or equivalence, capture authenticity, test execution and full test coverage are not established.")


if __name__ == "__main__":
    main()
