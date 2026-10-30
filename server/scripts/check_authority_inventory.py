#!/usr/bin/env python3
"""Manual source-drift check; never a semantic authorization-closure proof."""
from __future__ import annotations

import hashlib
import json
import os
import re
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / "server/docs/planning/authority-inventory.json"
SCOPES = (
    "server/crates/waddle-server/src",
    "server/crates/waddle-xmpp/src",
)


def source_path(root: Path, relative: str) -> Path:
    """Only inspect regular, in-scope source files; never follow symlinks."""
    parts = Path(relative).parts
    if Path(relative).is_absolute() or ".." in parts:
        raise ValueError("invalid source path")
    if not any(relative.startswith(scope + "/") for scope in SCOPES):
        raise ValueError("source path outside reviewed scan scopes")
    path = root
    for part in parts:
        path /= part
        if path.is_symlink():
            raise ValueError("symlink in source path")
    if path.suffix != ".rs" or not path.is_file():
        raise ValueError("missing Rust source file")
    return path


def lexical_pattern(tokens: list[str]) -> re.Pattern:
    """Require distinct ASCII Rust-style identifiers, then match raw word boundaries."""
    if not tokens or any(
        not isinstance(token, str) or not re.fullmatch(r"[A-Za-z_]\w*", token, re.ASCII)
        for token in tokens
    ):
        raise ValueError("invalid lexical token list")
    if len(set(tokens)) != len(tokens):
        raise ValueError("invalid lexical token list")
    return re.compile(r"\b(?:" + "|".join(map(re.escape, tokens)) + r")\b")


def scan_error(error: OSError) -> None:
    raise error


def rust_sources(root: Path):
    """Visit each scope in order and fail on unreadable or symlinked directories."""
    for scope in SCOPES:
        directory = root / scope
        if directory.is_symlink() or not directory.is_dir():
            raise ValueError("missing or symlinked scan scope")
        candidates = []
        for current, directories, files in os.walk(directory, onerror=scan_error):
            if any((Path(current) / name).is_symlink() for name in directories):
                raise ValueError("symlinked directory in scan scope")
            candidates.extend(Path(current) / name for name in files if name.endswith(".rs"))
        for candidate in sorted(candidates):
            relative = candidate.relative_to(root).as_posix()
            yield relative, source_path(root, relative)


def lexical_records(root: Path, pattern: re.Pattern):
    for relative, path in rust_sources(root):
        for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            for match in pattern.finditer(line):
                yield [relative, number, match.group(), line]


def lexical_snapshot(root: Path, tokens: list[str]) -> dict:
    """Hash raw matched lines, including comments, tests and all cfg branches.

    This deliberately does not parse Rust, classify calls or infer reachability.
    Unknown vocabulary and changes to unmatched files can escape this scan.
    """
    pattern = lexical_pattern(tokens)
    digest = hashlib.sha256()
    counts = dict.fromkeys(tokens, 0)
    for record in lexical_records(root, pattern):
        counts[record[2]] += 1
        digest.update(json.dumps(record, ensure_ascii=True).encode() + b"\n")
    return {"sha256": digest.hexdigest(), "occurrences": counts}


def source_fingerprints(records: list[dict]) -> dict[str, str]:
    """Decode explicit source metadata without silently collapsing duplicate paths."""
    if not isinstance(records, list) or not records:
        raise ValueError("empty source inventory")
    sources = {}
    for record in records:
        if not isinstance(record, dict):
            raise ValueError("source fingerprint must be an object")
        relative, expected = record["path"], record["sha256"]
        if not isinstance(relative, str) or not isinstance(expected, str) or not re.fullmatch(
            r"[0-9a-f]{64}", expected
        ):
            raise ValueError("invalid source fingerprint")
        if relative in sources:
            raise ValueError("duplicate source fingerprint")
        sources[relative] = expected
    return sources


def check_sources(root: Path, sources: dict[str, str]) -> list[str]:
    failures = []
    for relative, expected in sources.items():
        path = source_path(root, relative)
        if hashlib.sha256(path.read_bytes()).hexdigest() != expected:
            failures.append(f"reviewed file changed: {relative}")
    return failures


def anchor_matches(root: Path, sources: dict[str, str], anchor: dict) -> bool:
    if not isinstance(anchor, dict):
        raise ValueError("anchor must be an object")
    relative = anchor["path"]
    if relative not in sources:
        raise ValueError("anchor source is not fingerprinted")
    lines = source_path(root, relative).read_text(encoding="utf-8").splitlines()
    number = anchor["line"]
    if type(number) is not int or number < 1:
        raise ValueError("invalid anchor line")
    text = anchor["text"]
    if not isinstance(text, str) or not text:
        raise ValueError("empty anchor text")
    return number <= len(lines) and text in lines[number - 1]


def check(root: Path, manifest: dict) -> list[str]:
    if not isinstance(manifest, dict):
        raise ValueError("inventory must be an object")
    if manifest["format"] != 2 or not re.fullmatch(
        r"[0-9a-f]{40}", manifest["baseline_commit"]
    ):
        raise ValueError("unsupported inventory format or baseline")
    sources = source_fingerprints(manifest["sources"])
    anchors = manifest["anchors"]
    if not isinstance(anchors, dict) or not anchors:
        raise ValueError("empty anchor inventory")
    failures = check_sources(root, sources)
    for name, anchor in anchors.items():
        if not anchor_matches(root, sources, anchor):
            failures.append(f"source anchor changed: {name}")
    if lexical_snapshot(root, manifest["lexical_tokens"]) != manifest["lexical_snapshot"]:
        failures.append("raw lexical match inventory changed (not a call graph)")
    return failures


def main() -> int:
    try:
        manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
        failures = check(ROOT, manifest)
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"Authority inventory STALE/INVALID: {type(error).__name__}")
        return 1
    if failures:
        print("Authority inventory STALE: review findings before updating evidence.")
        for failure in failures:
            print(f"- {failure}")
        return 1
    print(
        f"Authority inventory source evidence matches {manifest['baseline_commit']}: "
        f"{len(manifest['sources'])} files, {len(manifest['anchors'])} anchors."
    )
    print("Raw lexical matches agree; semantic completeness/reachability/closure NOT established.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
