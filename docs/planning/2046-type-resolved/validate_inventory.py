#!/usr/bin/env python3
"""Check exact compiler-site and independently supplied indirect-edge reviews.

This validates completeness against the supplied candidates, source files and
coverage tables. It cannot establish that candidate discovery found every
indirect edge or that a human's semantic classification is correct.
"""

from __future__ import annotations

import argparse
import collections
import csv
import pathlib
import re
import subprocess
import sys
import tempfile

from mark_targets import HANDLER_FIELDS, HANDLERS_SHA256, SOURCE_COMMIT, handlers_fingerprint
from parse_diagnostics import COVERAGE_FIELDS, USE_FIELDS, load_manifest, read_capture, read_source, validate_source_hash

OPERATIONS = {"read", "write", "read-write", "borrow-transfer", "call", "construct", "type-only", "name-only"}
CLASSES = {"authority", "derived-index", "transport", "migration-glue", "obsolete", "excluded-type-only", "excluded-name-only"}
HANDLER_CFG_LABELS = {"clustering", "default", "test", "test-utils", "other-feature"}
SITE_KEY = ("profile", "tag", "path", "line", "column", "compiled_target_kind", "compiled_target_name", "compiled_target_test_mode")
SITE_FIELDS = ("source_commit", *SITE_KEY, "operation", "classification", "rationale", "evidence")
EDGE_KEY = ("edge_id", "from_path", "from_line", "to_path", "to_line", "kind")
EDGE_CANDIDATE_FIELDS = ("source_commit", *EDGE_KEY)
EDGE_REVIEW_FIELDS = (*EDGE_CANDIDATE_FIELDS, "operation", "classification", "rationale", "evidence")
HANDLER_KEY = ("path", "line", "column", "actor", "message")
HANDLER_REVIEW_FIELDS = ("source_commit", *HANDLER_KEY, "cfg_test_classification", "operation", "classification", "rationale", "evidence")
SOURCE_CITATION = re.compile(r"(server/crates/[^\s:()]+\.rs):(\d+)")


def read_table(path: pathlib.Path, expected: tuple[str, ...] | None = None) -> list[dict[str, str]]:
    text = path.read_text(encoding="utf-8")
    if not text.endswith("\n"):
        raise ValueError(f"missing final newline in {path}")
    with path.open(encoding="utf-8", newline="") as source:
        reader = csv.DictReader(source, delimiter="\t", strict=True)
        fields = reader.fieldnames
        if not fields or len(fields) != len(set(fields)) or (expected is not None and fields != list(expected)):
            raise ValueError(f"invalid columns in {path}: {fields}")
        rows = list(reader)
    if not rows:
        raise ValueError(f"empty evidence table: {path}")
    if any(None in row or any(not value for value in row.values()) for row in rows):
        raise ValueError(f"missing or extra fields in {path}")
    return rows


def check_source(root: pathlib.Path, path: str, line: str, column: str | None = None) -> str:
    relative = pathlib.PurePosixPath(path)
    if not path.startswith("server/crates/") or relative.is_absolute() or ".." in relative.parts:
        raise ValueError(f"invalid source path: {path}")
    resolved = (root / path).resolve()
    if not resolved.is_relative_to(root.resolve()) or not line.isdecimal() or int(line) < 1:
        raise ValueError(f"invalid source location: {path}:{line}")
    lines = read_source(resolved).splitlines()
    if int(line) > len(lines):
        raise ValueError(f"source line out of bounds: {path}:{line}")
    text = lines[int(line) - 1]
    if column is not None and (not column.isdecimal() or not 0 < int(column) <= len(text) + 1):
        raise ValueError(f"source column out of bounds: {path}:{line}:{column}")
    return text


def validate_review(row: dict[str, str], root: pathlib.Path) -> None:
    if row["source_commit"] != SOURCE_COMMIT:
        raise ValueError("review refers to a different source commit")
    operations = canonical_values(row["operation"], OPERATIONS, "operation")
    classes = canonical_values(row["classification"], CLASSES, "classification")
    for operation, classification in (("type-only", "excluded-type-only"), ("name-only", "excluded-name-only")):
        if (operation in operations or classification in classes) and (operations != {operation} or classes != {classification}):
            raise ValueError(f"{classification} and {operation} must be the sole classification and operation")
    rationale = row["rationale"].strip()
    evidence = row["evidence"].strip()
    for label, value in (("rationale", rationale), ("evidence", evidence)):
        if len(value) < 20 or len(value.split()) < 3 or re.search(r"\b(?:TODO|TBD|unknown|unclassified|manual-review-required)\b", value, re.I):
            raise ValueError(f"missing or insubstantial {label}")
    if "obsolete" in classes and not re.search(r"\b(?:obsolete|unused|unreachable|retired|removed|superseded)\b", rationale, re.I):
        raise ValueError("obsolete classification requires an explicit obsolescence rationale")
    citations = SOURCE_CITATION.findall(evidence)
    if not citations:
        raise ValueError("evidence must cite a server/crates/*.rs:line source location")
    for path, line in citations:
        check_source(root, path, line)


def canonical_values(value: str, allowed: set[str], label: str) -> set[str]:
    """Accept unique, lexically sorted labels separated by semicolons."""
    parts = value.split(";")
    if not parts or any(part not in allowed for part in parts) or parts != sorted(set(parts)):
        raise ValueError(f"missing, unrecognized or noncanonical {label}")
    return set(parts)


def unique_index(rows: list[dict[str, str]], key_fields: tuple[str, ...], name: str) -> dict[tuple[str, ...], dict[str, str]]:
    result = {}
    for row in rows:
        key = tuple(row[field] for field in key_fields)
        if key in result:
            raise ValueError(f"duplicate {name} key: {key}")
        result[key] = row
    return result


def validate_site_reviews(root: pathlib.Path, manifest: list[dict[str, str]], uses: list[dict[str, str]], coverage: list[dict[str, str]], reviews: list[dict[str, str]]) -> None:
    tags = {row["tag"] for row in manifest}
    raw = []
    for row in uses:
        if row["profile"] not in {"default", "clustering"} or row["tag"] not in tags:
            raise ValueError("unknown use-site profile or target tag")
        if row["compiled_target_test_mode"] != "false" or row["source_class_hint"] not in {"prod", "test", "test-support"} or row["rw_classification"] != "manual-review-required":
            raise ValueError("invalid raw compiler use-site provenance")
        if row["compiled_target_kind"] != "lib" or row["compiled_target_name"] not in {"waddle_server", "waddle_xmpp"}:
            raise ValueError("unrecognized raw compiler target kind or name")
        if row["kind"] != row["tag"].split(".", 1)[0].split("::", 1)[0]:
            raise ValueError("raw compiler tag kind does not match the target")
        if row["source"] != check_source(root, row["file"], row["line"], row["column"]).strip():
            raise ValueError(f"stale or truncated source evidence: {row['file']}:{row['line']}")
        raw.append({**row, "path": row["file"]})
    raw_by_key = unique_index(raw, SITE_KEY, "compiler site")
    if {row["profile"] for row in raw} != {"default", "clustering"}:
        raise ValueError("both default and clustering compiler profiles are required")
    coverage_by_key = unique_index(coverage, ("profile", "tag"), "coverage")
    expected_coverage = {(profile, tag) for profile in ("default", "clustering") for tag in tags}
    if set(coverage_by_key) != expected_coverage:
        raise ValueError("coverage tags do not exactly match both manifest profiles")
    counts = collections.Counter((row["profile"], row["tag"]) for row in raw)
    for key, row in coverage_by_key.items():
        if row["target_declarations"] != "1" or row["typed_use_sites"] != str(counts[key]):
            raise ValueError(f"compiler use-site count does not match coverage: {key}")
    reviewed = unique_index(reviews, SITE_KEY, "semantic review")
    if set(reviewed) != set(raw_by_key):
        raise ValueError(f"semantic site coverage mismatch: missing={len(set(raw_by_key) - set(reviewed))}, extra={len(set(reviewed) - set(raw_by_key))}")
    for row in reviews:
        validate_review(row, root)


def validate_edges(root: pathlib.Path, candidates: list[dict[str, str]], reviews: list[dict[str, str]]) -> None:
    candidate_ids: set[str] = set()
    for row in candidates:
        if row["source_commit"] != SOURCE_COMMIT or not row["edge_id"].strip() or not row["kind"].strip():
            raise ValueError("invalid indirect-edge candidate identity")
        if row["edge_id"] in candidate_ids:
            raise ValueError(f"duplicate indirect edge ID: {row['edge_id']}")
        candidate_ids.add(row["edge_id"])
        check_source(root, row["from_path"], row["from_line"])
        check_source(root, row["to_path"], row["to_line"])
    discovered = unique_index(candidates, EDGE_KEY, "indirect candidate")
    reviewed = unique_index(reviews, EDGE_KEY, "indirect review")
    if set(discovered) != set(reviewed):
        raise ValueError(f"indirect-edge coverage mismatch: missing={len(set(discovered) - set(reviewed))}, extra={len(set(reviewed) - set(discovered))}")
    for row in reviews:
        validate_review(row, root)


def validate_handlers(root: pathlib.Path, candidates: list[dict[str, str]], reviews: list[dict[str, str]], *, expected_digest: str = HANDLERS_SHA256) -> None:
    import hashlib

    if handlers_fingerprint(candidates) != expected_digest:
        raise ValueError("handler candidates do not match the pinned discovery fingerprint")
    discovered = unique_index(candidates, HANDLER_KEY, "handler candidate")
    reviewed = unique_index(reviews, HANDLER_KEY, "handler review")
    if set(discovered) != set(reviewed):
        raise ValueError(f"handler review coverage mismatch: missing={len(set(discovered) - set(reviewed))}, extra={len(set(reviewed) - set(discovered))}")
    for row in candidates:
        if row["source_commit"] != SOURCE_COMMIT or row["review_status"] != "manual-review-required" or row["cfg_test_classification"] != "manual-review-required":
            raise ValueError("invalid handler candidate provenance")
        source = check_source(root, row["path"], row["line"], row["column"])
        if not source[int(row["column"]) - 1:].startswith("handle"):
            raise ValueError("handler candidate does not point to handle")
        digest = hashlib.sha256(read_source(root / row["path"]).encode()).hexdigest()
        validate_source_hash(row["path"], row["sha256_before"], digest)
    for row in reviews:
        # These are human-reviewed source conditions, not evidence of compiler
        # selection or runtime reachability for the handler.
        canonical_values(row["cfg_test_classification"], HANDLER_CFG_LABELS, "handler cfg/test classification")
        validate_review(row, root)


def profile_tables(paths: list[pathlib.Path], fields: tuple[str, ...], label: str) -> tuple[dict[str, pathlib.Path], list[dict[str, str]]]:
    tables: dict[str, pathlib.Path] = {}
    all_rows: list[dict[str, str]] = []
    for path in paths:
        rows = read_table(path, fields)
        profiles = {row["profile"] for row in rows}
        if len(profiles) != 1:
            raise ValueError(f"{label} file must contain one profile: {path}")
        profile = next(iter(profiles))
        if profile not in {"default", "clustering"} or profile in tables:
            raise ValueError(f"duplicate or unrecognized {label} profile: {profile}")
        tables[profile] = path.resolve()
        all_rows.extend(rows)
    if set(tables) != {"default", "clustering"}:
        raise ValueError(f"both {label} profiles are required")
    return tables, all_rows


def validate_capture_outputs(patched_root: pathlib.Path, capture_root: str, manifest: pathlib.Path, capture: pathlib.Path, expected_sha256: str, profile: str, uses: pathlib.Path, coverage: pathlib.Path) -> None:
    """Reparse a hash-checked snapshot and compare the retained TSVs byte-for-byte.

    The externally retained hash identifies the supplied capture; it does not
    establish that the original producer or capture was authentic or complete.
    """
    raw = read_capture(capture, expected_sha256)
    with tempfile.TemporaryDirectory(prefix="w2046-reparse-") as directory:
        tmp = pathlib.Path(directory)
        snapshot, regenerated_uses, regenerated_coverage = (tmp / name for name in ("capture.jsonl", "uses.tsv", "coverage.tsv"))
        snapshot.write_bytes(raw)
        command = [sys.executable, "-B", str(pathlib.Path(__file__).with_name("parse_diagnostics.py")),
                   "--diagnostics", str(snapshot), "--capture-sha256", expected_sha256,
                   "--profile", profile, "--patched-root", str(patched_root), "--capture-root", capture_root, "--manifest", str(manifest),
                   "--output", str(regenerated_uses), "--coverage", str(regenerated_coverage)]
        result = subprocess.run(command, text=True, capture_output=True)
        if result.returncode != 0:
            raise ValueError(f"{profile} capture reparse failed: {result.stderr.strip()}")
        if uses.read_bytes() != regenerated_uses.read_bytes() or coverage.read_bytes() != regenerated_coverage.read_bytes():
            raise ValueError(f"{profile} use-site or coverage table does not match retained Cargo capture")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-root", type=pathlib.Path, required=True, help="clean source checkout whose server tree matches the pinned commit")
    parser.add_argument("--manifest", type=pathlib.Path, required=True)
    parser.add_argument("--patched-root", type=pathlib.Path, required=True, help="current marker-patched checkout matching the pinned manifest")
    parser.add_argument("--capture-root", required=True, help="absolute checkout path recorded in the Cargo captures")
    parser.add_argument("--default-capture", type=pathlib.Path, required=True, help="retained Cargo JSONL or .gz capture")
    parser.add_argument("--default-capture-sha256", required=True, help="independently recorded hash of uncompressed default capture")
    parser.add_argument("--clustering-capture", type=pathlib.Path, required=True, help="retained Cargo JSONL or .gz capture")
    parser.add_argument("--clustering-capture-sha256", required=True, help="independently recorded hash of uncompressed clustering capture")
    parser.add_argument("--uses", type=pathlib.Path, action="append", required=True, help="raw compiler TSV; pass once per profile")
    parser.add_argument("--coverage", type=pathlib.Path, action="append", required=True, help="compiler coverage TSV; pass once per profile")
    parser.add_argument("--site-reviews", type=pathlib.Path, required=True)
    parser.add_argument("--handlers", type=pathlib.Path, required=True, help="handler candidates generated by mark_targets.py")
    parser.add_argument("--handler-reviews", type=pathlib.Path, required=True)
    parser.add_argument("--edge-candidates", type=pathlib.Path, required=True)
    parser.add_argument("--edge-reviews", type=pathlib.Path, required=True)
    args = parser.parse_args()
    root = args.source_root.resolve()
    try:
        paths = ["server/crates", "docs/planning/connection_caller_regions.py"]
        subprocess.run(["git", "-C", str(root), "diff", "--exit-code", "--quiet", SOURCE_COMMIT, "HEAD", "--", *paths], check=True)
        status = subprocess.check_output(["git", "-C", str(root), "status", "--porcelain", "--untracked-files=all", "--", *paths], text=True)
        if status:
            raise ValueError("source tree or classifier has local changes")
        manifest = load_manifest(args.manifest)
        import hashlib
        for rel, entries in group_manifest(manifest).items():
            hashes = {row["sha256_before"] for row in entries}
            if len(hashes) != 1:
                raise ValueError(f"inconsistent source hashes for {rel}")
            validate_source_hash(rel, next(iter(hashes)), hashlib.sha256(read_source(root / rel).encode()).hexdigest())
        use_tables, uses = profile_tables(args.uses, USE_FIELDS, "use-site")
        coverage_tables, coverage = profile_tables(args.coverage, COVERAGE_FIELDS, "coverage")
        for profile, capture, digest in (("default", args.default_capture, args.default_capture_sha256), ("clustering", args.clustering_capture, args.clustering_capture_sha256)):
            validate_capture_outputs(args.patched_root.resolve(), args.capture_root, args.manifest.resolve(), capture, digest, profile, use_tables[profile], coverage_tables[profile])
        reviews = read_table(args.site_reviews, SITE_FIELDS)
        handlers = read_table(args.handlers, HANDLER_FIELDS)
        handler_reviews = read_table(args.handler_reviews, HANDLER_REVIEW_FIELDS)
        candidates = read_table(args.edge_candidates, EDGE_CANDIDATE_FIELDS)
        edge_reviews = read_table(args.edge_reviews, EDGE_REVIEW_FIELDS)
        validate_site_reviews(root, manifest, uses, coverage, reviews)
        validate_handlers(root, handlers, handler_reviews)
        validate_edges(root, candidates, edge_reviews)
    except (OSError, ValueError, KeyError, csv.Error, subprocess.CalledProcessError) as error:
        raise SystemExit(f"inventory validation failed: {error}") from error
    print(f"validated {len(reviews)} compiler sites, {len(handler_reviews)} handler candidates and {len(edge_reviews)} supplied indirect edges at {SOURCE_COMMIT}")
    print("Handler cfg/test labels describe manual source review, not compiled or runtime reachability. Indirect-edge discovery and classification correctness remain manual review obligations.")


def group_manifest(rows: list[dict[str, str]]) -> dict[str, list[dict[str, str]]]:
    grouped: dict[str, list[dict[str, str]]] = collections.defaultdict(list)
    for row in rows:
        grouped[row["file"]].append(row)
    return grouped


if __name__ == "__main__":
    main()
