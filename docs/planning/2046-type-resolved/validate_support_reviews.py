#!/usr/bin/env python3
"""Validate reviews for the additional support-library compiler sites at 4219.

The candidate keys are independently regenerated from the pinned production and
support-library Cargo streams, separately for each feature profile. This checks
review coverage and source evidence, not the correctness of semantic judgments,
the classification of reused production sites, or complete test coverage.
The pinned hashes identify retained bytes; they do not attest capture origin.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import pathlib
import subprocess
import sys
import tempfile

from mark_targets import SOURCE_COMMIT
from parse_diagnostics import USE_FIELDS as PRODUCTION_FIELDS
from parse_diagnostics import load_manifest, read_capture, read_source, validate_source_hash
from parse_test_diagnostics import SCHEMA_VERSION, USE_FIELDS as SUPPORT_FIELDS
from validate_inventory import SOURCE_CITATION, check_source, group_manifest, read_table, unique_index, validate_review

CAPTURE_ROOT = "/Users/rawkode/.codex/worktrees/2046-remote-ordering/waddle"
METADATA_SHA256 = "63cc1f27ada3ecdf2a56a7dc4e28d7e52924e5ff45bb87f9e2b178439e56832f"
CAPTURES = {
    "default": {
        "production_sha256": "4820e5eb2fa0847093d8a633558100e5ba6a538c7d6a9e897e9dbe0295fd985c",
        "support_sha256": "04cfc6e03f2638f705efae18d49b27c6cf7d8c51e47c62ea6cd1a550f7ec64a5",
        "production_count": 2899, "support_count": 3251, "additional_count": 352,
    },
    "clustering": {
        "production_sha256": "cb531e83acacd87d00bb5471e0ff50fe76f88180e5b4c25ff1c9cf9a73593c5f",
        "support_sha256": "b0efee8634657f6a6bca7f9b222a433ee3ff0a1ffe73b3aefc826a7464f018a4",
        "production_count": 5059, "support_count": 5411, "additional_count": 352,
    },
}
SITE_KEY = ("tag", "path", "line", "column", "compiled_target_kind", "compiled_target_name", "compiled_target_test_mode")
REVIEW_KEY = ("feature_profile", *SITE_KEY)
REVIEW_FIELDS = ("source_commit", *REVIEW_KEY, "operation", "classification", "rationale", "evidence")


def validate_source_tree(root: pathlib.Path, manifest: list[dict[str, str]]) -> None:
    """Require clean server/classifier sources equivalent to the pinned commit."""
    scope = ["server/Cargo.toml", "server/Cargo.lock", "server/crates", "docs/planning/connection_caller_regions.py"]
    subprocess.run(["git", "-C", str(root), "diff", "--quiet", SOURCE_COMMIT, "HEAD", "--", *scope], check=True)
    status = subprocess.check_output(["git", "-C", str(root), "status", "--porcelain", "--untracked-files=all", "--", *scope], text=True)
    if status:
        raise ValueError("source tree or classifier has local changes")
    for rel, entries in group_manifest(manifest).items():
        hashes = {row["sha256_before"] for row in entries}
        if len(hashes) != 1:
            raise ValueError(f"inconsistent source hashes for {rel}")
        validate_source_hash(rel, next(iter(hashes)), hashlib.sha256(read_source(root / rel).encode()).hexdigest())


def verify_capture_hashes(capture_dir: pathlib.Path) -> None:
    """Hash uncompressed capture and metadata bytes against fixed receipts."""
    read_capture(capture_dir / "cargo-metadata.json.gz", METADATA_SHA256)
    for profile, item in CAPTURES.items():
        read_capture(capture_dir / f"{profile}.jsonl.gz", item["production_sha256"])
        read_capture(capture_dir / f"test-support-{profile}/diagnostics.jsonl.gz", item["support_sha256"])


def regenerated_tables(capture_dir: pathlib.Path, patched_root: pathlib.Path, manifest_path: pathlib.Path, output_dir: pathlib.Path) -> dict[str, dict[str, list[dict[str, str]]]]:
    """Run both existing parsers and require byte-identical retained tables."""
    verify_capture_hashes(capture_dir)
    scripts = pathlib.Path(__file__).resolve().parent
    tables = {}
    for profile, item in CAPTURES.items():
        tables[profile] = {}
        for kind in ("production", "support"):
            out = output_dir / f"{kind}-{profile}-uses.tsv"
            coverage = output_dir / f"{kind}-{profile}-coverage.tsv"
            if kind == "production":
                capture = capture_dir / f"{profile}.jsonl.gz"
                command = [sys.executable, "-B", str(scripts / "parse_diagnostics.py"),
                           "--diagnostics", str(capture), "--capture-sha256", item["production_sha256"],
                           "--profile", profile, "--output", str(out), "--coverage", str(coverage)]
                retained = {out: capture_dir / f"uses-{profile}.tsv", coverage: capture_dir / f"coverage-{profile}.tsv"}
                fields = PRODUCTION_FIELDS
            else:
                base = capture_dir / f"test-support-{profile}"
                capture = base / "diagnostics.jsonl.gz"
                artifacts = output_dir / f"{kind}-{profile}-inventory.tsv"
                command = [sys.executable, "-B", str(scripts / "parse_test_diagnostics.py"),
                           "--capture-mode", "support-lib", "--feature-profile", profile,
                           "--diagnostics", str(capture), "--capture-sha256", item["support_sha256"],
                           "--metadata", str(capture_dir / "cargo-metadata.json.gz"), "--metadata-sha256", METADATA_SHA256,
                           "--output", str(out), "--coverage", str(coverage), "--inventory", str(artifacts)]
                prefix = f"w2046-4219-support-{profile}"
                retained = {out: base / f"{prefix}-uses.tsv", coverage: base / f"{prefix}-coverage.tsv", artifacts: base / f"{prefix}-inventory.tsv"}
                fields = SUPPORT_FIELDS
            command.extend(["--patched-root", str(patched_root), "--capture-root", CAPTURE_ROOT, "--manifest", str(manifest_path)])
            result = subprocess.run(command, text=True, capture_output=True)
            if result.returncode:
                raise ValueError(f"{profile} {kind} capture reparse failed: {result.stderr.strip()}")
            for generated, original in retained.items():
                if generated.read_bytes() != original.read_bytes():
                    raise ValueError(f"{profile} {kind} regenerated table differs from retained {original.name}")
            tables[profile][kind] = read_table(out, fields)
    return tables


def site_index(root: pathlib.Path, rows: list[dict[str, str]], tags: set[str], profile: str, *, support: bool) -> dict[tuple[str, ...], dict[str, str]]:
    indexed_rows = []
    for row in rows:
        fields = SUPPORT_FIELDS if support else PRODUCTION_FIELDS
        if set(row) != set(fields) or any(not value for value in row.values()):
            raise ValueError("invalid compiler use-site schema")
        actual_profile = row["feature_profile"] if support else row["profile"]
        if actual_profile != profile or row["tag"] not in tags:
            raise ValueError("unknown compiler use-site profile or target tag")
        if row["kind"] != row["tag"].split(".", 1)[0].split("::", 1)[0]:
            raise ValueError("compiler use-site tag kind mismatch")
        if (row["compiled_target_kind"] != "lib" or row["compiled_target_name"] not in {"waddle_server", "waddle_xmpp"}
                or row["compiled_target_test_mode"] != "false" or row["source_class_hint"] not in {"prod", "test", "test-support"}
                or row["rw_classification"] != "manual-review-required"):
            raise ValueError("invalid compiler use-site provenance")
        if support:
            if (row["schema_version"] != SCHEMA_VERSION or row["capture_mode"] != "support-lib"
                    or row["capture_sha256"] != CAPTURES[profile]["support_sha256"] or row["metadata_sha256"] != METADATA_SHA256):
                raise ValueError("invalid support capture provenance")
            expected = ["test-utils"] if row["compiled_target_name"] == "waddle_xmpp" else (["clustering", "test-support"] if profile == "clustering" else ["test-support"])
            if json.loads(row["compiled_target_features"]) != expected:
                raise ValueError("invalid support compiler feature set")
        if row["source"] != check_source(root, row["file"], row["line"], row["column"]).strip():
            raise ValueError(f"stale compiler source evidence: {row['file']}:{row['line']}")
        indexed_rows.append({**row, "path": row["file"]})
    return unique_index(indexed_rows, SITE_KEY, f"{profile} compiler site")


def derive_candidates(root: pathlib.Path, manifest: list[dict[str, str]], tables: dict[str, dict[str, list[dict[str, str]]]], *, require_pinned_counts: bool = True) -> dict[tuple[str, ...], dict[str, str]]:
    if set(tables) != set(CAPTURES):
        raise ValueError("both support-library profiles are required")
    tags = {row["tag"] for row in manifest}
    candidates = {}
    for profile, item in CAPTURES.items():
        if set(tables[profile]) != {"production", "support"}:
            raise ValueError(f"missing production or support tables for {profile}")
        production = site_index(root, tables[profile]["production"], tags, profile, support=False)
        support = site_index(root, tables[profile]["support"], tags, profile, support=True)
        missing = set(production) - set(support)
        if missing:
            raise ValueError(f"{profile} support capture omits {len(missing)} production keys")
        additional = set(support) - set(production)
        if require_pinned_counts and (len(production), len(support), len(additional)) != (item["production_count"], item["support_count"], item["additional_count"]):
            raise ValueError(f"{profile} compiler site counts do not match pinned capture")
        for key in additional:
            candidates[(profile, *key)] = support[key]
    return candidates


def validate_reviews(root: pathlib.Path, candidates: dict[tuple[str, ...], dict[str, str]], reviews: list[dict[str, str]]) -> None:
    for row in reviews:
        if set(row) != set(REVIEW_FIELDS) or any(not value for value in row.values()):
            raise ValueError("invalid support-review schema")
    reviewed = unique_index(reviews, REVIEW_KEY, "support semantic review")
    if set(reviewed) != set(candidates):
        raise ValueError(f"support review coverage mismatch: missing={len(set(candidates) - set(reviewed))}, extra={len(set(reviewed) - set(candidates))}")
    for row in reviews:
        validate_review(row, root)
        if (row["path"], row["line"]) not in SOURCE_CITATION.findall(row["evidence"]):
            raise ValueError(f"support review evidence must cite the reviewed source location: {row['path']}:{row['line']}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-root", required=True, type=pathlib.Path, help="clean source-equivalent checkout for checking review evidence")
    parser.add_argument("--patched-root", required=True, type=pathlib.Path, help="separate checkout instrumented with the pinned manifest")
    parser.add_argument("--reviews", required=True, type=pathlib.Path, help="combined default and clustering review TSV")
    parser.add_argument("--capture-dir", type=pathlib.Path, default=pathlib.Path(__file__).resolve().parent / "capture-4219bdd")
    args = parser.parse_args()
    root, patched_root, capture_dir = args.source_root.resolve(), args.patched_root.resolve(), args.capture_dir.resolve()
    try:
        manifest_path = capture_dir / "targets.tsv"
        manifest = load_manifest(manifest_path)
        validate_source_tree(root, manifest)
        with tempfile.TemporaryDirectory(prefix="w2046-support-reparse-") as directory:
            tables = regenerated_tables(capture_dir, patched_root, manifest_path, pathlib.Path(directory))
            candidates = derive_candidates(root, manifest, tables)
            reviews = read_table(args.reviews, REVIEW_FIELDS)
            validate_reviews(root, candidates, reviews)
    except (OSError, EOFError, ValueError, KeyError, TypeError, csv.Error, subprocess.CalledProcessError) as error:
        raise SystemExit(f"support-review validation failed: {error}") from error
    print(f"validated {len(reviews)} support-only compiler-site reviews at {SOURCE_COMMIT} (352 per profile)")
    print("Semantic correctness, reused production classifications, capture authenticity, test execution and full test coverage are not established.")


if __name__ == "__main__":
    main()
