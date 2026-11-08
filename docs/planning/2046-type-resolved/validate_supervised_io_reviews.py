#!/usr/bin/env python3
"""Validate reviews of the captured supervised_io integration-test sites.

Only warnings attributed to the test/supervised_io/true target in the retained
default-profile capture are reviewed here. The check does not establish the
correctness of semantic labels, execution, other test targets, or semantic
coverage of the libraries present in the same capture. Hashes identify retained
bytes; they do not attest the capture's origin or completeness.
"""

from __future__ import annotations

import argparse
import csv
import json
import pathlib
import subprocess
import sys
import tempfile

from mark_targets import SOURCE_COMMIT
from parse_diagnostics import load_manifest, read_capture
from parse_test_diagnostics import SCHEMA_VERSION, USE_FIELDS
from validate_inventory import SOURCE_CITATION, check_source, read_table, unique_index, validate_review
from validate_support_reviews import CAPTURE_ROOT, METADATA_SHA256, SITE_KEY, validate_source_tree

CAPTURE_MODE = "server-support-integration"
FEATURE_PROFILE = "default"
CAPTURE_SHA256 = "a71d09a670dd532995514204cde907ddb1cf07a855f9c28ba5b7010cb5dbbcdf"
TARGET = ("test", "supervised_io", "true")
REVIEW_KEY = ("capture_mode", "feature_profile", *SITE_KEY)
REVIEW_FIELDS = ("source_commit", *REVIEW_KEY, "operation", "classification", "rationale", "evidence")


def regenerated_uses(capture_dir: pathlib.Path, patched_root: pathlib.Path, manifest: pathlib.Path, output_dir: pathlib.Path) -> list[dict[str, str]]:
    """Reparse fixed hash-checked inputs and compare every retained output."""
    base = capture_dir / "test-support-supervised"
    capture, metadata = base / "diagnostics.jsonl.gz", capture_dir / "cargo-metadata.json.gz"
    read_capture(capture, CAPTURE_SHA256)
    read_capture(metadata, METADATA_SHA256)
    uses, coverage, inventory = (output_dir / name for name in ("uses.tsv", "coverage.tsv", "inventory.tsv"))
    command = [sys.executable, "-B", str(pathlib.Path(__file__).resolve().with_name("parse_test_diagnostics.py")),
               "--capture-mode", CAPTURE_MODE, "--feature-profile", FEATURE_PROFILE, "--test-target", TARGET[1],
               "--diagnostics", str(capture), "--capture-sha256", CAPTURE_SHA256,
               "--metadata", str(metadata), "--metadata-sha256", METADATA_SHA256,
               "--patched-root", str(patched_root), "--capture-root", CAPTURE_ROOT, "--manifest", str(manifest),
               "--output", str(uses), "--coverage", str(coverage), "--inventory", str(inventory)]
    result = subprocess.run(command, text=True, capture_output=True)
    if result.returncode:
        raise ValueError(f"supervised_io capture reparse failed: {result.stderr.strip()}")
    prefix = "w2046-4219-support-integration-supervised"
    for generated, suffix in ((uses, "uses"), (coverage, "coverage"), (inventory, "inventory")):
        retained = base / f"{prefix}-{suffix}.tsv"
        if generated.read_bytes() != retained.read_bytes():
            raise ValueError(f"regenerated table differs from retained {retained.name}")
    return read_table(uses, USE_FIELDS)


def derive_candidates(root: pathlib.Path, manifest: list[dict[str, str]], rows: list[dict[str, str]], *, require_pinned_counts: bool = True) -> dict[tuple[str, ...], dict[str, str]]:
    tags = {row["tag"] for row in manifest}
    if require_pinned_counts and len(rows) != 3356:
        raise ValueError("compiler use-site count does not match pinned capture")
    selected = []
    for row in rows:
        if set(row) != set(USE_FIELDS) or any(not value for value in row.values()):
            raise ValueError("invalid compiler use-site schema")
        if (row["schema_version"] != SCHEMA_VERSION or row["capture_mode"] != CAPTURE_MODE
                or row["feature_profile"] != FEATURE_PROFILE or row["capture_sha256"] != CAPTURE_SHA256
                or row["metadata_sha256"] != METADATA_SHA256 or row["tag"] not in tags):
            raise ValueError("invalid supervised_io capture provenance or target tag")
        actual_target = (row["compiled_target_kind"], row["compiled_target_name"], row["compiled_target_test_mode"])
        if actual_target != TARGET:
            if actual_target[0] == "test" or actual_target[1] == TARGET[1]:
                raise ValueError("unexpected integration-test target identity")
            continue
        if (row["kind"] != row["tag"].split(".", 1)[0].split("::", 1)[0]
                or row["source_class_hint"] not in {"prod", "test", "test-support"}
                or row["rw_classification"] != "manual-review-required"
                or json.loads(row["compiled_target_features"]) != ["test-support"]):
            raise ValueError("invalid supervised_io use-site provenance")
        if row["source"] != check_source(root, row["file"], row["line"], row["column"]).strip():
            raise ValueError(f"stale supervised_io source evidence: {row['file']}:{row['line']}")
        selected.append({**row, "path": row["file"]})
    candidates = unique_index(selected, REVIEW_KEY, "supervised_io compiler site")
    if require_pinned_counts and len(candidates) != 105:
        raise ValueError("supervised_io test-site count does not match pinned capture")
    return candidates


def validate_reviews(root: pathlib.Path, candidates: dict[tuple[str, ...], dict[str, str]], reviews: list[dict[str, str]]) -> None:
    for row in reviews:
        if set(row) != set(REVIEW_FIELDS) or any(not value for value in row.values()):
            raise ValueError("invalid supervised_io review schema")
    reviewed = unique_index(reviews, REVIEW_KEY, "supervised_io semantic review")
    if set(reviewed) != set(candidates):
        raise ValueError(f"supervised_io review coverage mismatch: missing={len(set(candidates) - set(reviewed))}, extra={len(set(reviewed) - set(candidates))}")
    for row in reviews:
        validate_review(row, root)
        if (row["path"], row["line"]) not in SOURCE_CITATION.findall(row["evidence"]):
            raise ValueError(f"supervised_io review evidence must cite the reviewed source location: {row['path']}:{row['line']}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-root", required=True, type=pathlib.Path, help="clean source-equivalent checkout for checking review evidence")
    parser.add_argument("--patched-root", required=True, type=pathlib.Path, help="checkout instrumented with the pinned manifest")
    parser.add_argument("--reviews", required=True, type=pathlib.Path, help="supervised_io test-target review TSV")
    parser.add_argument("--capture-dir", type=pathlib.Path, default=pathlib.Path(__file__).resolve().parent / "capture-4219bdd")
    args = parser.parse_args()
    root, patched_root, capture_dir = args.source_root.resolve(), args.patched_root.resolve(), args.capture_dir.resolve()
    try:
        manifest_path = capture_dir / "targets.tsv"
        manifest = load_manifest(manifest_path)
        validate_source_tree(root, manifest)
        with tempfile.TemporaryDirectory(prefix="w2046-supervised-reparse-") as directory:
            rows = regenerated_uses(capture_dir, patched_root, manifest_path, pathlib.Path(directory))
            candidates = derive_candidates(root, manifest, rows)
            reviews = read_table(args.reviews, REVIEW_FIELDS)
            validate_reviews(root, candidates, reviews)
    except (OSError, EOFError, ValueError, KeyError, TypeError, csv.Error, subprocess.CalledProcessError) as error:
        raise SystemExit(f"supervised_io review validation failed: {error}") from error
    print(f"validated {len(reviews)} supervised_io test-target compiler-site reviews at {SOURCE_COMMIT}")
    print("Semantic correctness, library reviews, other tests, capture authenticity and test execution are not established.")


if __name__ == "__main__":
    main()
