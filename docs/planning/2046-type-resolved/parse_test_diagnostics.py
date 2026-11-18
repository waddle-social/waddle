#!/usr/bin/env python3
"""Parse separately captured test/support marker diagnostics (schema v2).

Each capture is scoped to an explicit mode and, for integration tests, a list
of selected test targets. A Cargo compiler-message has no profile.test field;
when more than one artifact shares its package/kind/name, attribution is
ambiguous and the entire capture is rejected. Source-region hints are lexical
and never determine compiled target mode. A target artifact does not establish
that its cfg-gated body compiled. Test captures do not prove execution, all-test
coverage, the original capture's authenticity, or absence of uses for which
rustc did not emit a warning.

Server-support unit and integration modes expect test-support enabled (and
clustering for that profile); XMPP uses test-utils. XMPP default unit and
integration modes instead expect an empty XMPP feature list. These expectations
are checked against Cargo artifacts; feature unification that differs is rejected.

Integration target selection is limited to targets marked test=true in the
retained metadata, including explicit selections. This excludes test=false
targets even if explicitly requested; the pinned metadata has no such targets.
"""

from __future__ import annotations

import argparse
import bisect
import collections
import csv
import hashlib
import importlib.util
import json
import os
import pathlib
import re
import subprocess
import tomllib
import urllib.parse
from functools import lru_cache

from parse_diagnostics import (
    DEV_PROFILE, MARKER, MARKER_LINE, enclosing_fn, load_manifest, normalize_file,
    parse_cargo_stream, parse_capture_root, read_capture, read_source,
    strip_marker_lines, validate_patched_checkout,
)

MODES = ("support-lib", "server-unit", "server-integration", "server-support-unit", "server-support-integration",
         "xmpp-unit", "xmpp-integration", "xmpp-default-unit", "xmpp-default-integration")
SCHEMA_VERSION = "test-capture-v2"
USE_FIELDS = (
    "schema_version", "capture_mode", "feature_profile", "capture_sha256", "metadata_sha256", "tag", "kind", "source_class_hint",
    "file", "line", "column", "compiled_package_id", "compiled_target_kind", "compiled_target_name",
    "compiled_target_test_mode", "compiled_target_features", "enclosing_fn", "rw_classification", "source",
)
COVERAGE_FIELDS = ("schema_version", "capture_mode", "feature_profile", "capture_sha256", "metadata_sha256", "tag", "target_declarations", "typed_use_sites")
INVENTORY_FIELDS = (
    "schema_version", "capture_mode", "feature_profile", "capture_sha256", "metadata_sha256", "package_id", "package_name", "target_kind",
    "target_name", "target_source", "target_test_mode", "features", "fresh",
)
PACKAGES = {"waddle-server": "waddle_server", "waddle-xmpp": "waddle_xmpp"}


def validate_source_equivalence(root: pathlib.Path) -> None:
    from mark_targets import SOURCE_COMMIT

    scope = ["server/Cargo.toml", "server/Cargo.lock", "server/crates"]
    status = subprocess.check_output(["git", "-C", str(root), "status", "--porcelain", "--untracked-files=all", "--", *scope], text=True)
    diff = subprocess.run(["git", "-C", str(root), "diff", "--quiet", SOURCE_COMMIT, "HEAD", "--", *scope])
    if status or diff.returncode != 0:
        raise ValueError(f"test-target source must be clean and match {SOURCE_COMMIT}")


def load_metadata(path: pathlib.Path, digest: str, root: pathlib.Path, capture_root: pathlib.PurePosixPath) -> dict[str, dict[str, object]]:
    """Validate retained Cargo metadata without claiming its invocation is attested."""
    data = json.loads(read_capture(path, digest).decode("utf-8"))
    if not isinstance(data, dict) or data.get("version") != 1 or data.get("workspace_root") != str(capture_root / "server") or not isinstance(data.get("packages"), list) or not isinstance(data.get("workspace_members"), list):
        raise ValueError("Cargo metadata has wrong schema or capture workspace root")
    with (root / "server/Cargo.toml").open("rb") as source:
        version = tomllib.load(source)["workspace"]["package"]["version"]
    result: dict[str, dict[str, object]] = {}
    for package in data["packages"]:
        if not isinstance(package, dict) or package.get("name") not in PACKAGES:
            continue
        name = package["name"]
        package_id = package.get("id")
        if name in result or not isinstance(package_id, str) or package_id not in data["workspace_members"] or package.get("version") != version:
            raise ValueError(f"duplicate or invalid metadata package identity for {name}")
        if "source" not in package or package["source"] is not None:
            raise ValueError(f"metadata package source must be present and null for path package {name}")
        if not package_id.startswith("path+file://"):
            raise ValueError(f"non-path metadata package identity for {name}")
        parts = urllib.parse.urlsplit(package_id[len("path+"):])
        if (parts.scheme != "file" or parts.netloc or parts.query
                or urllib.parse.unquote(parts.path) != str(capture_root / "server/crates" / name)
                or parts.fragment not in {version, f"{name}@{version}"}):
            raise ValueError(f"metadata package ID does not match source for {name}")
        manifest_path = package.get("manifest_path")
        if not isinstance(manifest_path, str) or not pathlib.PurePosixPath(manifest_path).is_absolute() or normalize_file(manifest_path, capture_root) != f"server/crates/{name}/Cargo.toml":
            raise ValueError(f"invalid metadata manifest path for {name}")
        if not isinstance(package.get("targets"), list):
            raise ValueError(f"missing metadata target inventory for {name}")
        targets: dict[tuple[str, str], dict[str, object]] = {}
        for target in package["targets"]:
            if not isinstance(target, dict) or not isinstance(target.get("name"), str) or not isinstance(target.get("kind"), list) or len(target["kind"]) != 1 or not isinstance(target["kind"][0], str):
                raise ValueError(f"malformed metadata target in {name}")
            target_name, kind = target["name"], target["kind"][0]
            source = target.get("src_path")
            relative = normalize_file(source, capture_root) if isinstance(source, str) and pathlib.PurePosixPath(source).is_absolute() else None
            if relative is None or not relative.startswith(f"server/crates/{name}/"):
                raise ValueError(f"invalid metadata source for {name}:{target_name}")
            source_path = (root / relative).resolve()
            if not source_path.is_relative_to((root / "server/crates" / name).resolve()) or not source_path.is_file():
                raise ValueError(f"missing or escaping metadata source for {name}:{target_name}")
            required = target.get("required-features", [])
            if not isinstance(required, list) or any(not isinstance(feature, str) for feature in required) or len(set(required)) != len(required) or not isinstance(target.get("test"), bool):
                raise ValueError(f"invalid metadata feature/test flags for {name}:{target_name}")
            key = (kind, target_name)
            if key in targets:
                raise ValueError(f"duplicate metadata target for {name}:{key}")
            targets[key] = {"source": relative, "required_features": required, "test": target["test"]}
        if ("lib", PACKAGES[name]) not in targets:
            raise ValueError(f"missing metadata library target for {name}")
        result[name] = {"id": package_id, "targets": targets}
    if set(result) != set(PACKAGES):
        raise ValueError("Cargo metadata is missing expected workspace packages")
    if result["waddle-server"]["id"] == result["waddle-xmpp"]["id"]:
        raise ValueError("Cargo metadata package IDs are not unique")
    return result


def expected_features(mode: str, profile: str, package: str) -> list[str]:
    if package == "waddle-xmpp":
        return [] if mode in {"xmpp-default-unit", "xmpp-default-integration"} else ["test-utils"]
    result = ["clustering"] if profile == "clustering" else []
    if mode in {"support-lib", "server-support-unit", "server-support-integration"}:
        result.append("test-support")
    return sorted(result)


def test_inventory(
    messages: list[dict[str, object]], root: pathlib.Path, capture_root: pathlib.PurePosixPath,
    mode: str, profile: str, selected: set[str], metadata: dict[str, dict[str, object]],
) -> dict[tuple[str, str, str], dict[str, object]]:
    if mode not in MODES:
        raise ValueError("unknown test capture mode")
    if mode in {"xmpp-default-unit", "xmpp-default-integration"}:
        allowed_profiles = {"default"}
    elif mode.startswith("xmpp-"):
        allowed_profiles = {"test-utils"}
    else:
        allowed_profiles = {"default", "clustering"}
    if profile not in allowed_profiles:
        raise ValueError("feature profile does not match capture mode")
    package = "waddle-xmpp" if mode.startswith("xmpp-") else "waddle-server"
    integration = mode.endswith("-integration")
    if integration != bool(selected):
        raise ValueError("integration mode requires explicit selected test targets; other modes forbid them")
    package_targets = metadata[package]["targets"]
    available = {name: target for (kind, name), target in package_targets.items() if kind == "test" and target["test"] and set(target["required_features"]).issubset(expected_features(mode, profile, package))} if integration else {}
    if selected - set(available):
        raise ValueError(f"unknown or feature-ineligible selected test targets: {sorted(selected - set(available))}")
    package_ids = {entry["id"]: name for name, entry in metadata.items()}
    expected_packages = {"waddle-xmpp"} if mode.startswith("xmpp-") else set(PACKAGES)
    artifacts: dict[tuple[str, str, str], dict[str, object]] = {}
    identities: set[tuple[str, str, str]] = set()
    found: set[tuple[str, str, str]] = set()
    # Cargo's compiler-message does not identify the artifact profile. Scan
    # for ambiguous identities before checking which profile was expected, so
    # stream order cannot make a dual-mode build look unambiguous.
    for message in messages:
        if message.get("reason") != "compiler-artifact":
            continue
        name = package_ids.get(message.get("package_id"))
        if name is None:
            continue
        target = message.get("target")
        if not isinstance(target, dict) or not isinstance(target.get("name"), str) or not isinstance(target.get("kind"), list) or len(target["kind"]) != 1 or not isinstance(target["kind"][0], str):
            raise ValueError("malformed Cargo artifact target")
        identity = (name, target["kind"][0], target["name"])
        if identity in identities:
            raise ValueError(f"ambiguous or duplicate artifact target identity: {identity}")
        identities.add(identity)
    identities.clear()
    for message in messages:
        if message.get("reason") != "compiler-artifact":
            continue
        package_id = message.get("package_id")
        package_name = package_ids.get(package_id)
        if package_name is None:
            continue
        if package_name not in expected_packages:
            raise ValueError(f"unexpected package in {mode}: {package_name}")
        target = message.get("target")
        if not isinstance(target, dict) or not isinstance(target.get("name"), str) or not isinstance(target.get("kind"), list) or len(target["kind"]) != 1 or not isinstance(target["kind"][0], str):
            raise ValueError("malformed Cargo artifact target")
        name, kind = target["name"], target["kind"][0]
        logical_key = (package_name, kind, name)
        if logical_key in identities:
            raise ValueError(f"ambiguous or duplicate artifact target identity: {logical_key}")
        identities.add(logical_key)
        metadata_target = metadata[package_name]["targets"].get((kind, name))
        if metadata_target is None or not set(metadata_target["required_features"]).issubset(expected_features(mode, profile, package_name)):
            raise ValueError(f"unknown or feature-ineligible metadata target: {logical_key}")
        if kind == "lib" and name == PACKAGES[package_name]:
            target_source = metadata_target["source"]
            test_mode = (mode in {"server-unit", "server-support-unit"} and package_name == "waddle-server") or (mode in {"xmpp-unit", "xmpp-default-unit"} and package_name == "waddle-xmpp")
        elif integration and package_name == package and kind == "test" and name in selected:
            target_source, test_mode = metadata_target["source"], True
        elif mode in {"server-integration", "server-support-integration"} and package_name == "waddle-server" and kind == "bin":
            target_source, test_mode = metadata_target["source"], False
        else:
            raise ValueError(f"unknown or unselected target in {mode}: {logical_key}")
        source = target.get("src_path")
        if not isinstance(source, str) or not pathlib.PurePosixPath(source).is_absolute() or normalize_file(source, capture_root) != target_source:
            raise ValueError(f"unexpected target source for {logical_key}")
        profile_fields = message.get("profile")
        expected_profile = {**DEV_PROFILE, "test": test_mode}
        if not isinstance(profile_fields, dict) or profile_fields != expected_profile or any(
            profile_fields.get(field) is not expected for field, expected in (("debug_assertions", True), ("overflow_checks", True), ("test", test_mode))
        ):
            raise ValueError(f"unexpected dev/test profile for {logical_key}")
        if message.get("fresh") is not False:
            raise ValueError(f"fresh or missing rebuild evidence for {logical_key}")
        features = message.get("features")
        if features != expected_features(mode, profile, package_name):
            raise ValueError(f"unexpected feature set for {logical_key}: {features}")
        key = (str(package_id), kind, name)
        artifacts[key] = {"package_name": package_name, "source": target_source, "test": test_mode, "features": features}
        found.add(logical_key)
    required = {(name, "lib", PACKAGES[name]) for name in expected_packages}
    required.update((package, "test", name) for name in selected)
    missing = required - found
    if missing:
        raise ValueError(f"missing required Cargo targets: {sorted(missing)}")
    return artifacts


def diagnostic_rows(
    messages: list[dict[str, object]], root: pathlib.Path, capture_root: pathlib.PurePosixPath,
    manifest: list[dict[str, str]], artifacts: dict[tuple[str, str, str], dict[str, object]], mode: str, profile: str,
    capture_digest: str, metadata_digest: str,
) -> list[dict[str, str]]:
    root = root.resolve()
    marker_lines = {rel: tuple(sorted({int(row["line"]) for row in manifest if row["file"] == rel})) for rel in {row["file"] for row in manifest}}
    tags = {row["tag"] for row in manifest}
    classifier_path = root / "docs/planning/connection_caller_regions.py"
    spec = importlib.util.spec_from_file_location("w2046_capture_region_classifier", classifier_path)
    if spec is None or spec.loader is None:
        raise ValueError("cannot load source-region classifier")
    classifier = importlib.util.module_from_spec(spec)
    old_cwd = pathlib.Path.cwd()
    try:
        os.chdir(root)
        spec.loader.exec_module(classifier)
    finally:
        os.chdir(old_cwd)

    @lru_cache(maxsize=None)
    def texts(rel: str) -> tuple[tuple[str, ...], tuple[int, ...]]:
        path = (root / rel).resolve()
        if not path.is_relative_to(root):
            raise ValueError(f"source path escapes patched tree: {rel}")
        raw = read_source(path)
        return tuple(strip_marker_lines(raw).splitlines()), tuple(i for i, line in enumerate(raw.splitlines(keepends=True), 1) if MARKER_LINE.fullmatch(line))

    rows: dict[tuple[str, ...], dict[str, str]] = {}
    for message in messages:
        if message.get("reason") != "compiler-message":
            continue
        diagnostic = message.get("message")
        if not isinstance(diagnostic, dict):
            raise ValueError("malformed compiler diagnostic")
        code = diagnostic.get("code") or {}
        if not isinstance(code, dict) or code.get("code") != "deprecated":
            continue
        text = diagnostic.get("message")
        if not isinstance(text, str):
            raise ValueError("compiler diagnostic has no message")
        found = re.findall(r"W2046\|([^;\s`\"]+)", text)
        if MARKER in text and not found:
            raise ValueError("malformed marker diagnostic")
        if not found:
            continue
        if set(found) - tags:
            raise ValueError(f"unknown marker tags: {sorted(set(found) - tags)[:8]}")
        target = message.get("target")
        if not isinstance(target, dict) or not isinstance(target.get("kind"), list) or len(target["kind"]) != 1 or not isinstance(target.get("name"), str):
            raise ValueError("marker diagnostic has no target identity")
        key = (message.get("package_id"), target["kind"][0], target["name"])
        artifact = artifacts.get(key)
        if artifact is None:
            raise ValueError(f"marker diagnostic has unknown or unselected target: {key}")
        target_source = target.get("src_path")
        if (not isinstance(target_source, str) or not pathlib.PurePosixPath(target_source).is_absolute()
                or normalize_file(target_source, capture_root) != artifact["source"]):
            raise ValueError(f"marker diagnostic target source does not match its artifact: {key}")
        spans = diagnostic.get("spans")
        if not isinstance(spans, list):
            raise ValueError("marker diagnostic has invalid spans")
        primary = [span for span in spans if isinstance(span, dict) and span.get("is_primary") is True]
        if not primary:
            raise ValueError("marker diagnostic has no primary span")
        for span in primary:
            seen: set[int] = set()
            while span.get("expansion"):
                if id(span) in seen or not isinstance(span["expansion"], dict) or not isinstance(span["expansion"].get("span"), dict):
                    raise ValueError("invalid diagnostic expansion chain")
                seen.add(id(span))
                span = span["expansion"]["span"]
            rel = normalize_file(span.get("file_name", ""), capture_root)
            if rel is None:
                raise ValueError("marker diagnostic source is outside the capture root")
            lines, marked = texts(rel)
            if type(span.get("line_start")) is not int or type(span.get("column_start")) is not int:
                raise ValueError("invalid marker diagnostic coordinates")
            patched_line, col = span["line_start"], span["column_start"]
            if patched_line in marked:
                raise ValueError("diagnostic points to injected marker")
            line = patched_line - bisect.bisect_right(marked, patched_line)
            if not 0 < line <= len(lines) or not 0 < col <= len(lines[line - 1]) + 1:
                raise ValueError("diagnostic coordinates outside source")
            source_class = classifier.classify(rel, line + bisect.bisect_right(marker_lines.get(rel, ()), line))
            if source_class not in {"prod", "test", "test-support"}:
                raise ValueError("unclassified lexical source region")
            for tag in found:
                row = {
                    "schema_version": SCHEMA_VERSION, "capture_mode": mode, "feature_profile": profile,
                    "capture_sha256": capture_digest, "metadata_sha256": metadata_digest,
                    "tag": tag, "kind": tag.split(".", 1)[0].split("::", 1)[0], "source_class_hint": source_class,
                    "file": rel, "line": str(line), "column": str(col), "compiled_package_id": key[0],
                    "compiled_target_kind": key[1], "compiled_target_name": key[2],
                    "compiled_target_test_mode": str(artifact["test"]).lower(),
                    "compiled_target_features": json.dumps(artifact["features"], separators=(",", ":")),
                    "enclosing_fn": enclosing_fn(list(lines), line), "rw_classification": "manual-review-required",
                    "source": lines[line - 1].strip(),
                }
                identity = tuple(row[field] for field in ("tag", "file", "line", "column", "compiled_package_id", "compiled_target_kind", "compiled_target_name", "compiled_target_test_mode"))
                rows[identity] = row
    return [rows[key] for key in sorted(rows)]


def write_table(path: pathlib.Path, fields: tuple[str, ...], rows: list[dict[str, str]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8", newline="") as target:
        writer = csv.DictWriter(target, fieldnames=fields, delimiter="\t", lineterminator="\n")
        writer.writeheader()
        writer.writerows(rows)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--list-test-targets", choices=["waddle-server", "waddle-xmpp"], help="list integration targets from retained Cargo metadata")
    parser.add_argument("--source-root", type=pathlib.Path, help="clean source-equivalent tree for --list-test-targets")
    parser.add_argument("--capture-mode", choices=MODES)
    parser.add_argument("--feature-profile", choices=["default", "clustering", "test-utils"])
    parser.add_argument("--test-target", action="append", default=[], help="explicit integration target; repeat for each selected target")
    parser.add_argument("--all-test-targets", action="store_true", help="require every eligible metadata integration target")
    parser.add_argument("--metadata", type=pathlib.Path, help="retained Cargo metadata JSON or .gz snapshot")
    parser.add_argument("--metadata-sha256", help="independently retained SHA-256 of uncompressed Cargo metadata")
    parser.add_argument("--diagnostics", type=pathlib.Path)
    parser.add_argument("--capture-sha256", help="independently retained SHA-256 of uncompressed Cargo stdout")
    parser.add_argument("--capture-root")
    parser.add_argument("--patched-root", type=pathlib.Path)
    parser.add_argument("--manifest", type=pathlib.Path)
    parser.add_argument("--output", type=pathlib.Path)
    parser.add_argument("--coverage", type=pathlib.Path)
    parser.add_argument("--inventory", type=pathlib.Path)
    args = parser.parse_args()
    try:
        if args.list_test_targets:
            if args.source_root is None or args.metadata is None or args.metadata_sha256 is None or args.capture_root is None:
                raise ValueError("--source-root, --metadata, --metadata-sha256 and --capture-root are required for --list-test-targets")
            source_root, recorded_root = args.source_root.resolve(), parse_capture_root(args.capture_root)
            validate_source_equivalence(source_root)
            metadata = load_metadata(args.metadata, args.metadata_sha256, source_root, recorded_root)
            candidates = {name: target for (kind, name), target in metadata[args.list_test_targets]["targets"].items() if kind == "test"}
            print(json.dumps({"package": args.list_test_targets, "kind": "retained-cargo-metadata-test-targets", "metadata_sha256_uncompressed": args.metadata_sha256, "count": len(candidates), "targets": candidates}, sort_keys=True))
            return
        for name in ("capture_mode", "feature_profile", "diagnostics", "capture_sha256", "capture_root", "patched_root", "manifest", "metadata", "metadata_sha256", "output", "coverage", "inventory"):
            if getattr(args, name) is None:
                raise ValueError(f"--{name.replace('_', '-')} is required")
        if len(set(args.test_target)) != len(args.test_target):
            raise ValueError("duplicate selected test target")
        if args.all_test_targets and (args.test_target or not args.capture_mode.endswith("-integration")):
            raise ValueError("--all-test-targets is exclusive with --test-target and requires integration mode")
        root, capture_root = args.patched_root.resolve(), parse_capture_root(args.capture_root)
        paths = [args.diagnostics.resolve(), args.manifest.resolve(), args.output.resolve(), args.coverage.resolve(), args.inventory.resolve(), args.metadata.resolve()]
        if len(set(paths)) != len(paths):
            raise ValueError("inputs and outputs must be distinct files")
        manifest = load_manifest(paths[1])
        validate_patched_checkout(root, manifest)
        metadata = load_metadata(paths[5], args.metadata_sha256, root, capture_root)
        selected = set(args.test_target)
        if args.all_test_targets:
            package = "waddle-xmpp" if args.capture_mode.startswith("xmpp-") else "waddle-server"
            selected = {name for (kind, name), target in metadata[package]["targets"].items() if kind == "test" and target["test"] and set(target["required_features"]).issubset(expected_features(args.capture_mode, args.feature_profile, package))}
        capture = read_capture(paths[0], args.capture_sha256)
        messages = parse_cargo_stream(capture.decode("utf-8").splitlines())
        artifacts = test_inventory(messages, root, capture_root, args.capture_mode, args.feature_profile, selected, metadata)
        rows = diagnostic_rows(messages, root, capture_root, manifest, artifacts, args.capture_mode, args.feature_profile, args.capture_sha256, args.metadata_sha256)
        counts = collections.Counter(row["tag"] for row in rows)
        coverage = [{"schema_version": SCHEMA_VERSION, "capture_mode": args.capture_mode, "feature_profile": args.feature_profile, "capture_sha256": args.capture_sha256, "metadata_sha256": args.metadata_sha256, "tag": tag, "target_declarations": "1", "typed_use_sites": str(counts[tag])} for tag in sorted(row["tag"] for row in manifest)]
        inventory = [{"schema_version": SCHEMA_VERSION, "capture_mode": args.capture_mode, "feature_profile": args.feature_profile, "capture_sha256": args.capture_sha256, "metadata_sha256": args.metadata_sha256, "package_id": key[0], "package_name": item["package_name"], "target_kind": key[1], "target_name": key[2], "target_source": item["source"], "target_test_mode": str(item["test"]).lower(), "features": json.dumps(item["features"], separators=(",", ":")), "fresh": "false"} for key, item in sorted(artifacts.items())]
        write_table(paths[2], USE_FIELDS, rows)
        write_table(paths[3], COVERAGE_FIELDS, coverage)
        write_table(paths[4], INVENTORY_FIELDS, inventory)
    except (OSError, EOFError, ValueError, KeyError, TypeError, csv.Error, subprocess.CalledProcessError) as error:
        raise SystemExit(f"invalid test/support capture: {error}") from error
    print(json.dumps({"schema_version": SCHEMA_VERSION, "capture_mode": args.capture_mode, "feature_profile": args.feature_profile, "capture_root": str(capture_root), "capture_sha256_uncompressed": hashlib.sha256(capture).hexdigest(), "metadata_sha256_uncompressed": args.metadata_sha256, "target_declarations": len(manifest), "typed_use_sites": len(rows), "in_scope_artifact_targets": len(artifacts), "selected_integration_targets": sorted(selected), "evidence_limit": "compiled marker warnings only; cfg-gated target bodies, Cargo metadata invocation, dependency inventory, test execution and runtime reachability are not established by artifact presence"}, sort_keys=True))


if __name__ == "__main__":
    main()
