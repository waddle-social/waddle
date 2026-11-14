#!/usr/bin/env python3
"""Synthetic validation checks for support-library semantic reviews."""

from __future__ import annotations

import copy
import csv
import gzip
import hashlib
import io
import json
import pathlib
import subprocess
import tempfile
import unittest
from unittest import mock

import validate_support_reviews as validator


class SupportReviewTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = pathlib.Path(self.tmp.name)
        self.rel = "server/crates/example/src/lib.rs"
        path = self.root / self.rel
        path.parent.mkdir(parents=True)
        path.write_text("let common = authority();\nlet extra = marker();\nlet other = marker();\n")
        self.manifest = [{"tag": "T::COMMON@L1C1"}, {"tag": "T::EXTRA@L2C1"}]
        self.tables = {}
        for profile in validator.CAPTURES:
            self.tables[profile] = {
                "production": [self.use(profile, 1, support=False)],
                "support": [self.use(profile, 1, support=True), self.use(profile, 2, support=True)],
            }
        self.candidates = validator.derive_candidates(self.root, self.manifest, self.tables, require_pinned_counts=False)
        self.reviews = []
        for key in sorted(self.candidates):
            self.reviews.append({
                "source_commit": validator.SOURCE_COMMIT, **dict(zip(validator.REVIEW_KEY, key)),
                "operation": "call", "classification": "transport",
                "rationale": "This call transports the selected message to its destination.",
                "evidence": f"{self.rel}:2 shows the selected message call.",
            })

    def use(self, profile: str, line: int, *, support: bool) -> dict[str, str]:
        tag = "T::COMMON@L1C1" if line == 1 else "T::EXTRA@L2C1"
        common = {
            "tag": tag, "kind": "T", "source_class_hint": "test-support", "file": self.rel,
            "line": str(line), "column": "1", "compiled_target_kind": "lib", "compiled_target_name": "waddle_server",
            "compiled_target_test_mode": "false", "enclosing_fn": "fixture", "rw_classification": "manual-review-required",
            "source": (self.root / self.rel).read_text().splitlines()[line - 1],
        }
        if not support:
            return {"profile": profile, **common}
        features = ["clustering", "test-support"] if profile == "clustering" else ["test-support"]
        return {
            "schema_version": validator.SCHEMA_VERSION, "capture_mode": "support-lib", "feature_profile": profile,
            "capture_sha256": validator.CAPTURES[profile]["support_sha256"], "metadata_sha256": validator.METADATA_SHA256,
            "compiled_package_id": "path+file:///archive/server/crates/waddle-server#0.1.0",
            "compiled_target_features": json.dumps(features, separators=(",", ":")), **common,
        }

    def derive(self, tables: dict[str, dict[str, list[dict[str, str]]]]) -> dict[tuple[str, ...], dict[str, str]]:
        return validator.derive_candidates(self.root, self.manifest, tables, require_pinned_counts=False)

    def test_exact_profile_specific_reviews_pass(self) -> None:
        self.assertEqual(len(self.candidates), 2)
        self.assertEqual({key[0] for key in self.candidates}, {"default", "clustering"})
        validator.validate_reviews(self.root, self.candidates, self.reviews)
        validator.validate_reviews(self.root, self.candidates, [
            {**row, "operation": "call;read", "classification": "authority;transport"} for row in self.reviews
        ])
        validator.validate_reviews(self.root, self.candidates, [
            {**row, "evidence": f"{self.rel}:1 provides context; {self.rel}:2 is the reviewed call."} for row in self.reviews
        ])

    def test_missing_duplicate_extra_or_wrong_profile_reviews_fail(self) -> None:
        for rows, message in (
            (self.reviews[:-1], "coverage mismatch"),
            (self.reviews + self.reviews[:1], "duplicate"),
            (self.reviews + [{**self.reviews[0], "line": "3"}], "coverage mismatch"),
            ([{**row, "feature_profile": "default"} for row in self.reviews], "duplicate"),
            ([{**self.reviews[0], "feature_profile": "unknown"}, self.reviews[1]], "coverage mismatch"),
        ):
            with self.subTest(message=message, rows=rows), self.assertRaisesRegex(ValueError, message):
                validator.validate_reviews(self.root, self.candidates, rows)

    def test_invalid_or_unreviewed_semantics_and_source_commit_fail(self) -> None:
        changes = (
            ({"source_commit": "0" * 40}, "different source commit"),
            ({"operation": "manual-review-required"}, "operation"),
            ({"classification": "unclassified"}, "classification"),
            ({"classification": "transport;authority"}, "classification"),
            ({"operation": "type-only", "classification": "transport"}, "sole classification"),
            ({"classification": "obsolete"}, "obsolescence"),
            ({"rationale": "TODO source review required"}, "rationale"),
            ({"evidence": "There is no source citation in this entry."}, "source location"),
            ({"evidence": f"{self.rel}:99 is an invalid source citation."}, "out of bounds"),
            ({"evidence": f"{self.rel}:1 is a valid but unrelated citation."}, "cite the reviewed source location"),
        )
        for change, message in changes:
            altered = copy.deepcopy(self.reviews)
            altered[0].update(change)
            with self.subTest(change=change), self.assertRaisesRegex(ValueError, message):
                validator.validate_reviews(self.root, self.candidates, altered)

    def test_review_schema_and_location_are_exact(self) -> None:
        for change in ({"path": self.rel.replace("example", "other")}, {"line": "3"}, {"column": "2"},
                       {"compiled_target_name": "waddle_xmpp"}, {"compiled_target_test_mode": "true"}):
            altered = copy.deepcopy(self.reviews)
            altered[0].update(change)
            with self.subTest(change=change), self.assertRaisesRegex(ValueError, "coverage mismatch"):
                validator.validate_reviews(self.root, self.candidates, altered)
        for mutation in (lambda row: row.pop("evidence"), lambda row: row.update(extra="value")):
            altered = copy.deepcopy(self.reviews)
            mutation(altered[0])
            with self.assertRaisesRegex(ValueError, "schema"):
                validator.validate_reviews(self.root, self.candidates, altered)

    def test_raw_site_identity_and_source_fail_closed(self) -> None:
        cases = (
            ({"tag": "T::UNKNOWN@L2C1"}, "unknown compiler"),
            ({"kind": "OTHER"}, "tag kind mismatch"),
            ({"feature_profile": "clustering"}, "unknown compiler"),
            ({"capture_mode": "server-unit"}, "capture provenance"),
            ({"capture_sha256": "0" * 64}, "capture provenance"),
            ({"metadata_sha256": "0" * 64}, "capture provenance"),
            ({"compiled_target_features": '["clustering","test-support"]'}, "feature set"),
            ({"compiled_target_test_mode": "true"}, "use-site provenance"),
            ({"source": "stale text"}, "stale compiler"),
            ({"line": "99"}, "out of bounds"),
            ({"column": "999"}, "out of bounds"),
            ({"file": "server/crates/../escape.rs"}, "invalid source path"),
        )
        for change, message in cases:
            tables = copy.deepcopy(self.tables)
            tables["default"]["support"][1].update(change)
            with self.subTest(change=change), self.assertRaisesRegex(ValueError, message):
                self.derive(tables)
        duplicate = copy.deepcopy(self.tables)
        duplicate["default"]["support"].append(copy.deepcopy(duplicate["default"]["support"][1]))
        with self.assertRaisesRegex(ValueError, "duplicate"):
            self.derive(duplicate)
        missing = copy.deepcopy(self.tables)
        missing["default"]["support"].pop(0)
        with self.assertRaisesRegex(ValueError, "omits 1 production"):
            self.derive(missing)
        with self.assertRaisesRegex(ValueError, "counts do not match"):
            validator.derive_candidates(self.root, self.manifest, self.tables)

    def test_review_tsv_rejects_wrong_header_and_truncation(self) -> None:
        path = self.root / "reviews.tsv"
        with path.open("w", newline="") as output:
            writer = csv.DictWriter(output, fieldnames=validator.REVIEW_FIELDS, delimiter="\t", lineterminator="\n")
            writer.writeheader()
            writer.writerows(self.reviews)
        validator.validate_reviews(self.root, self.candidates, validator.read_table(path, validator.REVIEW_FIELDS))
        path.write_bytes(path.read_bytes()[:-1])
        with self.assertRaisesRegex(ValueError, "final newline"):
            validator.read_table(path, validator.REVIEW_FIELDS)
        path.write_text("incorrect\theader\nvalue\tvalue\n")
        with self.assertRaisesRegex(ValueError, "invalid columns"):
            validator.read_table(path, validator.REVIEW_FIELDS)

    def test_reparse_rejects_altered_retained_table(self) -> None:
        """Hash fixture bytes; pin values and parser subprocesses are mocked."""
        capture_dir = self.root / "captures"
        capture_dir.mkdir()
        profiles = copy.deepcopy(validator.CAPTURES)
        metadata = b'{"version":1}\n'
        (capture_dir / "cargo-metadata.json.gz").write_bytes(gzip.compress(metadata))
        metadata_digest = hashlib.sha256(metadata).hexdigest()
        snapshots = {}

        def table_bytes(fields: tuple[str, ...], rows: list[dict[str, str]]) -> bytes:
            text = io.StringIO(newline="")
            writer = csv.DictWriter(text, fieldnames=fields, delimiter="\t", lineterminator="\n")
            writer.writeheader()
            writer.writerows(rows)
            return text.getvalue().encode()

        for profile in profiles:
            base = capture_dir / f"test-support-{profile}"
            base.mkdir()
            for kind, path in (("production", capture_dir / f"{profile}.jsonl.gz"),
                               ("support", base / "diagnostics.jsonl.gz")):
                raw = f"{profile} {kind} fixture\n".encode()
                path.write_bytes(gzip.compress(raw))
                profiles[profile][f"{kind}_sha256"] = hashlib.sha256(raw).hexdigest()
                uses = table_bytes(validator.PRODUCTION_FIELDS if kind == "production" else validator.SUPPORT_FIELDS,
                                   self.tables[profile][kind])
                coverage = f"{profile} {kind} coverage\n".encode()
                inventory = f"{profile} {kind} inventory\n".encode()
                snapshots[(profile, kind)] = {"--output": uses, "--coverage": coverage, "--inventory": inventory}
                if kind == "production":
                    (capture_dir / f"uses-{profile}.tsv").write_bytes(uses)
                    (capture_dir / f"coverage-{profile}.tsv").write_bytes(coverage)
                else:
                    prefix = f"w2046-4219-support-{profile}"
                    (base / f"{prefix}-uses.tsv").write_bytes(uses)
                    (base / f"{prefix}-coverage.tsv").write_bytes(coverage)
                    (base / f"{prefix}-inventory.tsv").write_bytes(inventory)

        commands = []

        def fake_parser(command: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
            commands.append(command)
            support = "--capture-mode" in command
            flag = "--feature-profile" if support else "--profile"
            profile = command[command.index(flag) + 1]
            kind = "support" if support else "production"
            self.assertEqual(command[command.index("--capture-sha256") + 1], profiles[profile][f"{kind}_sha256"])
            if support:
                self.assertEqual(command[command.index("--metadata-sha256") + 1], metadata_digest)
            for option, content in snapshots[(profile, kind)].items():
                if option in command:
                    pathlib.Path(command[command.index(option) + 1]).write_bytes(content)
            return subprocess.CompletedProcess(command, 0, stdout="", stderr="")

        output_dir = self.root / "regenerated"
        output_dir.mkdir()
        with mock.patch.object(validator, "CAPTURES", profiles), mock.patch.object(validator, "METADATA_SHA256", metadata_digest), \
                mock.patch.object(validator.subprocess, "run", side_effect=fake_parser):
            tables = validator.regenerated_tables(capture_dir, self.root, self.root / "manifest.tsv", output_dir)
            self.assertEqual(tables, self.tables)
            self.assertEqual(len(commands), 4)
            retained = capture_dir / "test-support-default/w2046-4219-support-default-coverage.tsv"
            retained.write_bytes(b"altered retained coverage\n")
            with self.assertRaisesRegex(ValueError, "regenerated table differs from retained .*coverage.tsv"):
                validator.regenerated_tables(capture_dir, self.root, self.root / "manifest.tsv", output_dir)


class CaptureHashTests(unittest.TestCase):
    def test_pinned_metadata_and_every_capture_are_hash_checked(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            profiles = {}
            contents = {"cargo-metadata.json.gz": b'{"version":1}\n'}
            for profile in ("default", "clustering"):
                production = f"{profile}.jsonl.gz"
                support = f"test-support-{profile}/diagnostics.jsonl.gz"
                contents[production] = f"{profile} production\n".encode()
                contents[support] = f"{profile} support\n".encode()
                profiles[profile] = {"production_sha256": hashlib.sha256(contents[production]).hexdigest(),
                                     "support_sha256": hashlib.sha256(contents[support]).hexdigest()}
            for name, raw in contents.items():
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(gzip.compress(raw))
            metadata_digest = hashlib.sha256(contents["cargo-metadata.json.gz"]).hexdigest()
            with mock.patch.object(validator, "CAPTURES", profiles), mock.patch.object(validator, "METADATA_SHA256", metadata_digest):
                validator.verify_capture_hashes(root)
                for name, raw in contents.items():
                    path = root / name
                    path.write_bytes(gzip.compress(raw + b"modified\n"))
                    with self.subTest(name=name), self.assertRaisesRegex(ValueError, "SHA-256"):
                        validator.verify_capture_hashes(root)
                    path.write_bytes(gzip.compress(raw))
                path = root / "default.jsonl.gz"
                path.write_bytes(path.read_bytes()[:-3])
                with self.assertRaisesRegex(ValueError, "truncated gzip"):
                    validator.verify_capture_hashes(root)


if __name__ == "__main__":
    unittest.main()
