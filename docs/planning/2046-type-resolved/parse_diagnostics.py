#!/usr/bin/env python3
"""Convert rustc deprecation JSON from the W2046 marker pass into TSV.

The parser accepts Cargo JSON on stdin-derived file and emits primary source
spans only. It maps annotated-tree lines back to the unmodified source using
the W2046 marker manifest and labels test/test-support source regions with the
repository's existing lexical cfg classifier. Those labels classify source
regions; they are not themselves compiler proof that a region was selected.
"""

from __future__ import annotations

import argparse
import bisect
import collections
import csv
import gzip
import hashlib
import json
import os
import pathlib
import re
import subprocess
import sys
import zlib
from functools import lru_cache

from mark_targets import MANIFEST_SHA256, SOURCE_COMMIT, manifest_fingerprint


MARKER = "W2046|"
MARKER_LINE = re.compile(r'^[ \t]*#\[deprecated\(note = "((?:W2046\|[^;"\s]+)(?:;W2046\|[^;"\s]+)*)"\)\]\r?\n$')
USE_FIELDS = ("profile", "tag", "kind", "source_class_hint", "file", "line", "column", "compiled_target_kind", "compiled_target_name", "compiled_target_test_mode", "enclosing_fn", "rw_classification", "source")
COVERAGE_FIELDS = ("profile", "tag", "target_declarations", "typed_use_sites")
DEV_PROFILE = {"opt_level": "0", "debuginfo": "line-tables-only", "debug_assertions": True, "overflow_checks": True, "test": False}


def read_capture(path: pathlib.Path, expected_sha256: str | None = None) -> bytes:
    """Hash the uncompressed capture bytes, regardless of gzip container metadata."""
    encoded = path.read_bytes()
    try:
        raw = gzip.decompress(encoded) if path.suffix == ".gz" else encoded
    except (OSError, EOFError, zlib.error) as error:
        raise ValueError("invalid or truncated gzip Cargo capture") from error
    digest = hashlib.sha256(raw).hexdigest()
    if expected_sha256 is not None:
        if not re.fullmatch(r"[0-9a-f]{64}", expected_sha256) or digest != expected_sha256:
            raise ValueError("uncompressed Cargo capture SHA-256 does not match retained digest")
    if not raw.endswith(b"\n"):
        raise ValueError("Cargo JSON stream lacks final newline")
    return raw


def strip_marker_lines(text: str) -> str:
    """Remove inserted marker-only lines while preserving original newlines."""
    return "".join(line for line in text.splitlines(keepends=True) if not MARKER_LINE.fullmatch(line))


def read_source(path: pathlib.Path) -> str:
    with path.open(encoding="utf-8", newline="") as source:
        return source.read()


def validate_unique_tags(tags: list[str]) -> None:
    if len(set(tags)) != len(tags):
        raise ValueError("manifest tags are not one-to-one with declarations")


def validate_known_tags(tags: set[str], known: set[str]) -> None:
    unknown = tags - known
    if unknown:
        raise ValueError(f"unknown marker tags: {sorted(unknown)[:8]}")


def validate_source_hash(path: str, expected: str, actual: str) -> None:
    if actual != expected:
        raise ValueError(f"pre-annotation hash mismatch for {path}")


def load_manifest(path: pathlib.Path, *, expected_digest: str = MANIFEST_SHA256) -> list[dict[str, str]]:
    expected = ["file", "line", "column", "tag", "kind", "sha256_before"]
    with path.open(encoding="utf-8", newline="") as source:
        reader = csv.DictReader(source, delimiter="\t", strict=True)
        if reader.fieldnames != expected:
            raise ValueError(f"unexpected manifest columns: {reader.fieldnames}")
        rows = list(reader)
    if not rows:
        raise ValueError("target manifest is empty")
    identities = set()
    for row in rows:
        if None in row or any(not row.get(key) for key in expected):
            raise ValueError("manifest contains missing or extra fields")
        rel = pathlib.PurePosixPath(row["file"])
        if rel.is_absolute() or ".." in rel.parts or not row["file"].startswith("server/crates/"):
            raise ValueError(f"invalid target path: {row['file']}")
        if not row["line"].isdigit() or not row["column"].isdigit() or int(row["line"]) < 1 or int(row["column"]) < 1:
            raise ValueError(f"invalid target coordinates for {row['tag']}")
        if not row["tag"].endswith(f"@L{row['line']}C{row['column']}"):
            raise ValueError(f"tag coordinates do not match manifest: {row['tag']}")
        if row["kind"] not in {"type", "field", "variant", "method", "function"} or not re.fullmatch(r"[0-9a-f]{64}", row["sha256_before"]):
            raise ValueError(f"invalid target kind or hash for {row['tag']}")
        identity = (row["file"], row["line"], row["column"])
        if identity in identities:
            raise ValueError(f"duplicate target declaration: {identity}")
        identities.add(identity)
    validate_unique_tags([row["tag"] for row in rows])
    if manifest_fingerprint(rows) != expected_digest:
        raise ValueError("target manifest does not match the pinned discovery fingerprint")
    return rows


def validate_patched_sources(root: pathlib.Path, rows: list[dict[str, str]]) -> None:
    by_file: dict[str, list[dict[str, str]]] = collections.defaultdict(list)
    for row in rows:
        by_file[row["file"]].append(row)
    for rel, entries in by_file.items():
        path = (root / rel).resolve()
        if not path.is_relative_to(root.resolve()):
            raise ValueError(f"target escapes source root: {rel}")
        raw = read_source(path)
        expected: dict[int, set[str]] = collections.defaultdict(set)
        hashes = {entry["sha256_before"] for entry in entries}
        if len(hashes) != 1:
            raise ValueError(f"inconsistent pre-annotation hashes for {rel}")
        for entry in entries:
            expected[int(entry["line"])].add(entry["tag"])
        found: dict[int, set[str]] = {}
        original_line = 1
        for line in raw.splitlines(keepends=True):
            match = MARKER_LINE.fullmatch(line)
            if match:
                tags = [part[len(MARKER):] for part in match.group(1).split(";")]
                if len(tags) != len(set(tags)) or original_line in found:
                    raise ValueError(f"duplicate marker line or tag in {rel}:{original_line}")
                found[original_line] = set(tags)
            else:
                if MARKER in line:
                    raise ValueError(f"unrecognized marker line in {rel}:{original_line}")
                original_line += 1
        if found != dict(expected):
            raise ValueError(f"marker lines do not exactly match manifest for {rel}")
        original = strip_marker_lines(raw)
        validate_source_hash(rel, next(iter(hashes)), hashlib.sha256(original.encode()).hexdigest())
        source_lines = original.splitlines()
        for entry in entries:
            line, col = int(entry["line"]), int(entry["column"])
            if line > len(source_lines) or col > len(source_lines[line - 1]) + 1:
                raise ValueError(f"target coordinates outside source for {entry['tag']}")


def validate_patched_checkout(root: pathlib.Path, rows: list[dict[str, str]]) -> None:
    head = subprocess.check_output(["git", "-C", str(root), "rev-parse", "HEAD"], text=True).strip()
    if head != SOURCE_COMMIT:
        raise ValueError(f"patched source must be at {SOURCE_COMMIT}, found {head}")
    changed = set(subprocess.check_output(["git", "-C", str(root), "diff", "--name-only", "-z", "HEAD", "--"], text=True).strip("\0").split("\0"))
    untracked = subprocess.check_output(["git", "-C", str(root), "ls-files", "--others", "--exclude-standard", "-z"], text=True)
    if changed != {row["file"] for row in rows} or untracked:
        raise ValueError("patched checkout has missing, additional, or untracked changes")
    validate_patched_sources(root, rows)


def cargo_artifacts(messages: list[dict[str, object]], capture_root: pathlib.PurePosixPath, profile: str) -> dict[tuple[str, str], dict[str, object]]:
    expected = {"waddle_server": "waddle-server", "waddle_xmpp": "waddle-xmpp"}
    artifacts: dict[tuple[str, str], dict[str, object]] = {}
    for message in messages:
        if message.get("reason") != "compiler-artifact":
            continue
        target = message.get("target")
        if not isinstance(target, dict) or target.get("name") not in expected:
            continue
        name = target["name"]
        profile_fields = message.get("profile")
        if target.get("kind") != ["lib"] or not isinstance(profile_fields, dict) or profile_fields != DEV_PROFILE or any(
            profile_fields.get(field) is not expected_value
            for field, expected_value in (("debug_assertions", True), ("overflow_checks", True), ("test", False))
        ):
            raise ValueError(f"artifact does not have the exact captured dev profile for {name}")
        if message.get("fresh") is not False:
            raise ValueError(f"fresh or missing rebuild evidence for {name}")
        source = target.get("src_path")
        expected_source = f"server/crates/{expected[name]}/src/lib.rs"
        if not isinstance(source, str) or not pathlib.PurePosixPath(source).is_absolute() or normalize_file(source, capture_root) != expected_source:
            raise ValueError(f"artifact source root does not match capture root for {name}")
        features = message.get("features")
        expected_features = ["clustering"] if name == "waddle_server" and profile == "clustering" else []
        if features != expected_features:
            raise ValueError(f"artifact features do not match {profile} profile for {name}")
        key = (name, "lib")
        if key in artifacts or not isinstance(message.get("package_id"), str):
            raise ValueError(f"duplicate or missing artifact identity for {name}")
        artifacts[key] = message
    missing = {(name, "lib") for name in expected} - set(artifacts)
    if missing:
        raise ValueError(f"missing required Cargo artifacts: {sorted(missing)}")
    return artifacts


def parse_cargo_stream(lines: list[str]) -> list[dict[str, object]]:
    messages: list[dict[str, object]] = []
    for number, raw in enumerate(lines, start=1):
        try:
            message = json.loads(raw)
        except json.JSONDecodeError as error:
            raise ValueError(f"malformed Cargo JSON at line {number}") from error
        if not isinstance(message, dict):
            raise ValueError(f"Cargo JSON line {number} is not an object")
        if not isinstance(message.get("reason"), str) or not message["reason"]:
            raise ValueError(f"Cargo JSON line {number} has no reason")
        messages.append(message)
    finished = [i for i, message in enumerate(messages) if message.get("reason") == "build-finished"]
    if len(finished) != 1:
        raise ValueError(f"expected exactly one Cargo build-finished message, found {len(finished)}")
    if finished[0] != len(messages) - 1:
        raise ValueError("Cargo build-finished message is not the final stream record")
    if messages[finished[0]].get("success") is not True:
        raise ValueError("Cargo build-finished message does not report success=true")
    return messages


def self_check() -> None:
    no_final_newline = "fn no_final_newline() {}"
    annotated = '#[deprecated(note = "W2046|probe")]\n' + no_final_newline
    if strip_marker_lines(annotated) != no_final_newline:
        raise ValueError("marker reconstruction lost a missing final newline")
    if parse_cargo_stream(['{"reason":"build-finished","success":true}'])[0].get("success") is not True:
        raise ValueError("valid successful Cargo stream was rejected")
    for operation, args in (
        (validate_unique_tags, (["one", "one"],)),
        (validate_known_tags, ({"unknown"}, {"known"})),
        (validate_source_hash, ("probe.rs", "original", "stale")),
        (parse_cargo_stream, (["{\"reason\":\"compiler-message\"}"],)),
        (parse_cargo_stream, (["{\"reason\":\"build-finished\",\"success\":false}"],)),
        (parse_cargo_stream, (["{\"reason\":\"build-finished\",\"success\":true}", "{\"reason\":\"compiler-message\"}"],)),
        (parse_cargo_stream, (["not-json", "{\"reason\":\"build-finished\",\"success\":true}"],)),
    ):
        try:
            operation(*args)
        except ValueError:
            continue
        raise ValueError(f"fail-closed check did not reject {operation.__name__}")


def parse_capture_root(value: str) -> pathlib.PurePosixPath:
    """Validate the recorded path lexically; the old checkout need not exist."""
    path = pathlib.PurePosixPath(value)
    if (not path.is_absolute() or path == pathlib.PurePosixPath("/") or str(path) != value
            or any(part in {".", ".."} for part in value.split("/")) or "\\" in value or "\0" in value):
        raise ValueError("capture root must be a canonical absolute POSIX checkout path")
    return path


def normalize_file(file_name: str, capture_root: pathlib.PurePosixPath) -> str | None:
    if not isinstance(file_name, str) or not file_name or "\\" in file_name or "\0" in file_name:
        return None
    path = pathlib.PurePosixPath(file_name)
    if str(path) != file_name or any(part in {".", ".."} for part in file_name.split("/")):
        return None
    if path.is_absolute():
        try:
            rel = path.relative_to(capture_root).as_posix()
        except ValueError:
            return None
    else:
        rel = path.as_posix()
        if rel.startswith("crates/"):
            rel = "server/" + rel
    return rel if rel.startswith("server/crates/") else None


def enclosing_fn(lines: list[str], line: int) -> str:
    pattern = re.compile(r"^\s*(?:(?:pub)(?:\([^)]*\))?\s+)?(?:(?:async|unsafe|const)\s+)*fn\s+([A-Za-z_][A-Za-z0-9_]*)")
    for index in range(min(line, len(lines)) - 1, -1, -1):
        match = pattern.match(lines[index])
        if match:
            return match.group(1)
    return "?"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--diagnostics", type=pathlib.Path, help="stdout from cargo check --message-format=json")
    parser.add_argument("--capture-sha256", help="independently retained SHA-256 of uncompressed Cargo stdout")
    parser.add_argument("--profile", choices=["default", "clustering"])
    parser.add_argument("--patched-root", type=pathlib.Path)
    parser.add_argument("--capture-root", help="absolute checkout path recorded in Cargo JSON; need not exist now")
    parser.add_argument("--manifest", type=pathlib.Path)
    parser.add_argument("--output", type=pathlib.Path)
    parser.add_argument("--coverage", type=pathlib.Path)
    parser.add_argument("--self-check", action="store_true", help="run parser validation self-checks and exit")
    args = parser.parse_args()
    if args.self_check:
        self_check()
        print("parser self-check: pass")
        return
    for name in ("diagnostics", "profile", "patched_root", "capture_root", "manifest", "output", "coverage"):
        if getattr(args, name) is None:
            raise SystemExit(f"--{name.replace('_', '-')} is required unless --self-check is used")
    patched_root = args.patched_root.resolve()
    manifest_path = args.manifest.resolve()
    diagnostics_path = args.diagnostics.resolve()
    output_path = args.output.resolve()
    coverage_path = args.coverage.resolve()
    source_root = patched_root
    try:
        capture_root = parse_capture_root(args.capture_root)
    except ValueError as error:
        raise SystemExit(str(error)) from error
    if len({manifest_path, diagnostics_path, output_path, coverage_path}) != 4:
        raise SystemExit("inputs and outputs must be distinct files")

    try:
        manifest_rows = load_manifest(manifest_path)
        validate_patched_checkout(patched_root, manifest_rows)
    except (OSError, ValueError, csv.Error, subprocess.CalledProcessError) as error:
        raise SystemExit(f"invalid target manifest or patched tree: {error}") from error
    target_tags = sorted({row["tag"] for row in manifest_rows})
    marker_lines_by_file: dict[str, tuple[int, ...]] = {}
    for rel in sorted({row["file"] for row in manifest_rows}):
        marker_lines_by_file[rel] = tuple(
            sorted({int(row["line"]) for row in manifest_rows if row["file"] == rel})
        )

    # The source classifier is reused only for region labels. Rustc JSON is
    # authoritative for the reference spans and profile-specific compilation.
    sys.path.insert(0, str(source_root / "docs/planning"))
    os.chdir(source_root)
    try:
        from connection_caller_regions import classify
    except ImportError as error:
        raise SystemExit(f"cannot load the source-region classifier: {error}") from error

    @lru_cache(maxsize=None)
    def original_text(rel: str) -> str:
        path = (patched_root / rel).resolve()
        if not path.is_relative_to(patched_root):
            raise ValueError(f"source path escapes patched tree: {rel}")
        return strip_marker_lines(read_source(path))

    @lru_cache(maxsize=None)
    def source_lines(rel: str) -> tuple[str, ...]:
        return tuple(original_text(rel).splitlines())

    @lru_cache(maxsize=None)
    def patched_marker_lines(rel: str) -> tuple[int, ...]:
        path = (patched_root / rel).resolve()
        if not path.is_relative_to(patched_root):
            raise ValueError(f"source path escapes patched tree: {rel}")
        lines = read_source(path).splitlines(keepends=True)
        return tuple(i for i, line in enumerate(lines, start=1) if MARKER_LINE.fullmatch(line))

    def to_original_line(rel: str, patched_line: int) -> int:
        marked = patched_marker_lines(rel)
        preceding = sum(1 for line in marked if line <= patched_line)
        return max(1, patched_line - preceding)

    def to_patched_line(rel: str, original_line: int) -> int:
        return original_line + bisect.bisect_right(marker_lines_by_file.get(rel, ()), original_line)

    rows: dict[tuple[str, ...], dict[str, str]] = {}
    unknown_tags: set[str] = set()
    unlocated_diagnostics = 0
    try:
        capture = read_capture(diagnostics_path, args.capture_sha256)
        diagnostics = capture.decode("utf-8")
        cargo_messages = parse_cargo_stream(diagnostics.splitlines())
        artifacts = cargo_artifacts(cargo_messages, capture_root, args.profile)
    except (OSError, ValueError) as error:
        raise SystemExit(f"invalid compiler JSON stream: {error}") from error

    for message in cargo_messages:
        if message.get("reason") != "compiler-message":
            continue
        diagnostic = message.get("message", {})
        code = diagnostic.get("code") or {}
        if code.get("code") != "deprecated":
            continue
        tags = re.findall(r"W2046\|([^;\s`\"]+)", diagnostic.get("message", ""))
        if not tags and MARKER in diagnostic.get("message", ""):
            raise SystemExit("malformed marker diagnostic")
        if not tags:
            continue
        unknown_tags.update(set(tags) - set(target_tags))
        primary_spans = [span for span in diagnostic.get("spans", []) if span.get("is_primary")]
        if not primary_spans:
            unlocated_diagnostics += 1
            continue
        target = message.get("target") or {}
        target_kinds = target.get("kind")
        if not isinstance(target_kinds, list):
            raise SystemExit("marker diagnostic is missing Cargo target kind provenance")
        target_kind = ",".join(target_kinds)
        target_name = target.get("name", "?")
        if not target_kind or target_name == "?":
            raise SystemExit("marker diagnostic is missing Cargo target provenance")
        artifact = artifacts.get((target_name, target_kind))
        if artifact is None or message.get("package_id") != artifact["package_id"]:
            raise SystemExit(f"marker diagnostic has no compiler-artifact target provenance: {target_name}:{target_kind}")
        for span in primary_spans:
            while span.get("expansion"):
                span = span["expansion"].get("span", span)
            rel = normalize_file(span.get("file_name", ""), capture_root)
            if rel is None:
                unlocated_diagnostics += 1
                continue
            try:
                patched_line = int(span["line_start"])
                col = int(span["column_start"])
                if patched_line in patched_marker_lines(rel):
                    raise ValueError("diagnostic points to an injected marker")
                line = to_original_line(rel, patched_line)
                lines = source_lines(rel)
                if not 0 < line <= len(lines) or not 0 < col <= len(lines[line - 1]) + 1:
                    raise ValueError("diagnostic coordinates are outside source")
                source = lines[line - 1].strip()
                source_class = classify(rel, to_patched_line(rel, line))
                if source_class not in {"prod", "test", "test-support"}:
                    raise ValueError(f"unclassified source region: {source_class!r}")
            except (OSError, ValueError, KeyError, TypeError) as error:
                raise SystemExit(f"invalid marker diagnostic source {rel}: {error}") from error
            for tag in tags:
                test_mode_label = "false"
                key = (args.profile, tag, rel, line, col, target_kind, target_name, test_mode_label)
                rows[key] = {
                        "profile": args.profile,
                        "tag": tag,
                        "kind": tag.split(".", 1)[0].split("::", 1)[0],
                        "source_class_hint": source_class,
                        "file": rel,
                        "line": str(line),
                        "column": str(col),
                        "compiled_target_kind": target_kind,
                        "compiled_target_name": target_name,
                        "compiled_target_test_mode": test_mode_label,
                        "enclosing_fn": enclosing_fn(list(lines), line),
                        "source": source,
                        "rw_classification": "manual-review-required",
                    }

    try:
        validate_known_tags(unknown_tags, set())
    except ValueError as error:
        raise SystemExit(f"compiler emitted {error}") from error
    if unlocated_diagnostics:
        raise SystemExit(f"{unlocated_diagnostics} marker diagnostics had no in-scope primary source location")
    if not rows:
        raise SystemExit("compiler stream contains no located marker diagnostics")

    output_path.parent.mkdir(parents=True, exist_ok=True)
    with output_path.open("w", encoding="utf-8", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=USE_FIELDS, delimiter="\t", lineterminator="\n")
        writer.writeheader()
        for key in sorted(rows):
            writer.writerow(rows[key])

    counts = collections.Counter(row["tag"] for row in rows.values())
    coverage_path.parent.mkdir(parents=True, exist_ok=True)
    with coverage_path.open("w", encoding="utf-8", newline="") as f:
        writer = csv.writer(f, delimiter="\t", lineterminator="\n")
        writer.writerow(COVERAGE_FIELDS)
        target_count = collections.Counter(row["tag"] for row in manifest_rows)
        for tag in target_tags:
            writer.writerow([args.profile, tag, target_count[tag], counts[tag]])
    compiled_targets = sorted({f"{row['compiled_target_name']}:{row['compiled_target_kind']}:test={row['compiled_target_test_mode']}" for row in rows.values()})
    print(json.dumps({"profile": args.profile, "capture_root": str(capture_root), "patched_root": str(patched_root), "capture_sha256_uncompressed": hashlib.sha256(capture).hexdigest(), "target_declarations": len(manifest_rows), "typed_use_sites": len(rows), "tags_with_uses": len(counts), "compiled_targets": compiled_targets, "source_class_hints": dict(collections.Counter(row["source_class_hint"] for row in rows.values())), "output": str(output_path), "coverage": str(coverage_path)}, sort_keys=True))


if __name__ == "__main__":
    main()
