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
import json
import os
import pathlib
import re
import sys
from functools import lru_cache


MARKER = "W2046|"


def strip_marker_lines(text: str) -> str:
    """Remove inserted marker-only lines while preserving original newlines."""
    return "".join(line for line in text.splitlines(keepends=True) if MARKER not in line)


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


def parse_cargo_stream(lines: list[str]) -> list[dict[str, object]]:
    messages: list[dict[str, object]] = []
    for number, raw in enumerate(lines, start=1):
        try:
            message = json.loads(raw)
        except json.JSONDecodeError as error:
            raise ValueError(f"malformed Cargo JSON at line {number}") from error
        if not isinstance(message, dict):
            raise ValueError(f"Cargo JSON line {number} is not an object")
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


def normalize_file(file_name: str, source_root: pathlib.Path) -> str | None:
    path = pathlib.Path(file_name)
    if path.is_absolute():
        try:
            rel = path.resolve().relative_to(source_root.resolve()).as_posix()
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
    parser.add_argument("--profile", choices=["default", "clustering"])
    parser.add_argument("--patched-root", type=pathlib.Path)
    parser.add_argument("--manifest", type=pathlib.Path)
    parser.add_argument("--output", type=pathlib.Path)
    parser.add_argument("--coverage", type=pathlib.Path)
    parser.add_argument("--self-check", action="store_true", help="run parser validation self-checks and exit")
    args = parser.parse_args()
    if args.self_check:
        self_check()
        print("parser self-check: pass")
        return
    for name in ("diagnostics", "profile", "patched_root", "manifest", "output", "coverage"):
        if getattr(args, name) is None:
            raise SystemExit(f"--{name.replace('_', '-')} is required unless --self-check is used")
    patched_root = args.patched_root.resolve()
    manifest_path = args.manifest.resolve()
    diagnostics_path = args.diagnostics.resolve()
    output_path = args.output.resolve()
    coverage_path = args.coverage.resolve()
    source_root = patched_root

    try:
        manifest_rows = list(csv.DictReader(manifest_path.open(encoding="utf-8"), delimiter="\t"))
    except OSError as error:
        raise SystemExit(f"cannot read target manifest: {error}") from error
    target_tags = sorted({row["tag"] for row in manifest_rows})
    try:
        validate_unique_tags([row["tag"] for row in manifest_rows])
    except ValueError as error:
        raise SystemExit(str(error)) from error
    marker_lines_by_file: dict[str, tuple[int, ...]] = {}
    for rel in sorted({row["file"] for row in manifest_rows}):
        marker_lines_by_file[rel] = tuple(
            sorted({int(row["line"]) for row in manifest_rows if row["file"] == rel})
        )

    # Exercise the reconstruction path for a source file with no final newline.
    no_final_newline = "fn no_final_newline() {}"
    annotated = '#[deprecated(note = "W2046|probe") ]\n' + no_final_newline
    if strip_marker_lines(annotated) != no_final_newline:
        raise SystemExit("marker reconstruction does not preserve a missing final newline")

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
        try:
            raw = read_source(patched_root / rel)
        except OSError:
            return ""
        return strip_marker_lines(raw)

    @lru_cache(maxsize=None)
    def source_lines(rel: str) -> tuple[str, ...]:
        return tuple(original_text(rel).splitlines())

    @lru_cache(maxsize=None)
    def patched_marker_lines(rel: str) -> tuple[int, ...]:
        try:
            lines = read_source(patched_root / rel).splitlines()
        except OSError:
            return ()
        return tuple(i for i, line in enumerate(lines, start=1) if MARKER in line)

    # Refuse stale manifests or a patched tree that was regenerated from
    # another source state. Hashes are of the pre-annotation file contents.
    expected_hashes: dict[str, str] = {}
    for row in manifest_rows:
        old = expected_hashes.setdefault(row["file"], row["sha256_before"])
        if old != row["sha256_before"]:
            raise SystemExit(f"inconsistent pre-annotation hashes for {row['file']}")
    import hashlib

    for rel, expected in expected_hashes.items():
        actual = hashlib.sha256(original_text(rel).encode()).hexdigest()
        try:
            validate_source_hash(rel, expected, actual)
        except ValueError as error:
            raise SystemExit(str(error)) from error

    def to_original_line(rel: str, patched_line: int) -> int:
        marked = patched_marker_lines(rel)
        preceding = sum(1 for line in marked if line <= patched_line)
        return max(1, patched_line - preceding)

    def to_patched_line(rel: str, original_line: int) -> int:
        return original_line + bisect.bisect_right(marker_lines_by_file.get(rel, ()), original_line)

    rows: dict[tuple[str, ...], dict[str, str]] = {}
    unknown_tags: set[str] = set()
    unlocated_diagnostics = 0
    artifact_test_modes: dict[tuple[str, str], set[bool]] = collections.defaultdict(set)
    try:
        cargo_messages = parse_cargo_stream(diagnostics_path.read_text(encoding="utf-8").splitlines())
    except (OSError, ValueError) as error:
        raise SystemExit(f"invalid compiler JSON stream: {error}") from error
    for message in cargo_messages:
        if message.get("reason") != "compiler-artifact":
            continue
        target = message.get("target") or {}
        kinds = target.get("kind")
        profile = message.get("profile") or {}
        test_mode = profile.get("test")
        if isinstance(kinds, list) and isinstance(test_mode, bool) and target.get("name"):
            artifact_test_modes[(target["name"], ",".join(kinds))].add(test_mode)

    for message in cargo_messages:
        if message.get("reason") != "compiler-message":
            continue
        diagnostic = message.get("message", {})
        code = diagnostic.get("code") or {}
        if code.get("code") != "deprecated":
            continue
        tags = re.findall(r"W2046\|([^;\s`\"]+)", diagnostic.get("message", ""))
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
        test_modes = artifact_test_modes.get((target_name, target_kind))
        if not test_modes:
            raise SystemExit(f"marker diagnostic has no compiler-artifact target provenance: {target_name}:{target_kind}")
        for span in primary_spans:
            while span.get("expansion"):
                span = span["expansion"].get("span", span)
            rel = normalize_file(span.get("file_name", ""), source_root)
            if rel is None:
                unlocated_diagnostics += 1
                continue
            line = to_original_line(rel, int(span["line_start"]))
            col = int(span.get("column_start", 1))
            lines = source_lines(rel)
            source = lines[line - 1].strip() if 0 < line <= len(lines) else ""
            for tag in tags:
                for test_mode in test_modes:
                    test_mode_label = str(test_mode).lower()
                    key = (args.profile, tag, rel, line, col, target_kind, target_name, test_mode_label)
                    rows[key] = {
                        "profile": args.profile,
                        "tag": tag,
                        "kind": tag.split(".", 1)[0].split("::", 1)[0],
                        "source_class_hint": classify(rel, to_patched_line(rel, line)),
                        "file": rel,
                        "line": str(line),
                        "column": str(col),
                        "compiled_target_kind": target_kind,
                        "compiled_target_name": target_name,
                        "compiled_target_test_mode": test_mode_label,
                        "enclosing_fn": enclosing_fn(list(lines), line),
                        "source": source[:240],
                        "rw_classification": "manual-review-required",
                    }

    try:
        validate_known_tags(unknown_tags, set())
    except ValueError as error:
        raise SystemExit(f"compiler emitted {error}") from error
    if unlocated_diagnostics:
        raise SystemExit(f"{unlocated_diagnostics} marker diagnostics had no in-scope primary source location")

    fields = ["profile", "tag", "kind", "source_class_hint", "file", "line", "column", "compiled_target_kind", "compiled_target_name", "compiled_target_test_mode", "enclosing_fn", "rw_classification", "source"]
    output_path.parent.mkdir(parents=True, exist_ok=True)
    with output_path.open("w", encoding="utf-8", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=fields, delimiter="\t", lineterminator="\n")
        writer.writeheader()
        for key in sorted(rows):
            writer.writerow(rows[key])

    counts = collections.Counter(row["tag"] for row in rows.values())
    coverage_path.parent.mkdir(parents=True, exist_ok=True)
    with coverage_path.open("w", encoding="utf-8", newline="") as f:
        writer = csv.writer(f, delimiter="\t", lineterminator="\n")
        writer.writerow(["profile", "tag", "target_declarations", "typed_use_sites"])
        target_count = collections.Counter(row["tag"] for row in manifest_rows)
        for tag in target_tags:
            writer.writerow([args.profile, tag, target_count[tag], counts[tag]])
    compiled_targets = sorted({f"{row['compiled_target_name']}:{row['compiled_target_kind']}:test={row['compiled_target_test_mode']}" for row in rows.values()})
    print(json.dumps({"profile": args.profile, "target_declarations": len(manifest_rows), "typed_use_sites": len(rows), "tags_with_uses": len(counts), "compiled_targets": compiled_targets, "source_class_hints": dict(collections.Counter(row["source_class_hint"] for row in rows.values())), "output": str(output_path), "coverage": str(coverage_path)}, sort_keys=True))


if __name__ == "__main__":
    main()
