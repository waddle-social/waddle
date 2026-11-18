#!/usr/bin/env python3
"""Synthetic tests for the supervised_io compiler-site review validator."""

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

import validate_supervised_io_reviews as validator


class SupervisedReviewTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = pathlib.Path(self.tmp.name)
        self.rel = "server/crates/waddle-server/tests/supervised_io/fixture.rs"
        path = self.root / self.rel
        path.parent.mkdir(parents=True)
        path.write_text("let one = marker();\nlet two = marker();\nlet other = marker();\n")
        self.manifest = [{"tag": "T::ONE@L1C1"}, {"tag": "T::TWO@L2C1"}]
        self.rows = [self.use(1), self.use(2), self.use(1, library=True)]
        self.candidates = validator.derive_candidates(self.root, self.manifest, self.rows, require_pinned_counts=False)
        self.reviews = []
        for key in sorted(self.candidates):
            row = {"source_commit": validator.SOURCE_COMMIT, **dict(zip(validator.REVIEW_KEY, key)),
                   "operation": "call", "classification": "transport",
                   "rationale": "This call transports the selected message to its destination."}
            row["evidence"] = f"{self.rel}:{row['line']} contains the selected message call."
            self.reviews.append(row)

    def use(self, line: int, *, library: bool = False) -> dict[str, str]:
        return {
            "schema_version": validator.SCHEMA_VERSION, "capture_mode": validator.CAPTURE_MODE,
            "feature_profile": validator.FEATURE_PROFILE, "capture_sha256": validator.CAPTURE_SHA256,
            "metadata_sha256": validator.METADATA_SHA256, "tag": "T::ONE@L1C1" if line == 1 else "T::TWO@L2C1",
            "kind": "T", "source_class_hint": "test", "file": self.rel, "line": str(line), "column": "1",
            "compiled_package_id": "path+file:///archive/server/crates/waddle-server#0.1.0",
            "compiled_target_kind": "lib" if library else "test",
            "compiled_target_name": "waddle_server" if library else "supervised_io",
            "compiled_target_test_mode": "false" if library else "true",
            "compiled_target_features": json.dumps(["test-support"], separators=(",", ":")),
            "enclosing_fn": "fixture", "rw_classification": "manual-review-required",
            "source": (self.root / self.rel).read_text().splitlines()[line - 1],
        }

    def derive(self, rows: list[dict[str, str]]) -> dict[tuple[str, ...], dict[str, str]]:
        return validator.derive_candidates(self.root, self.manifest, rows, require_pinned_counts=False)

    def test_exact_target_reviews_pass_and_library_rows_are_excluded(self) -> None:
        self.assertEqual(len(self.candidates), 2)
        self.assertTrue(all(key[-3:] == validator.TARGET for key in self.candidates))
        validator.validate_reviews(self.root, self.candidates, self.reviews)
        validator.validate_reviews(self.root, self.candidates, [
            {**row, "operation": "call;read", "classification": "authority;transport"} for row in self.reviews
        ])

    def test_missing_extra_duplicate_or_wrong_target_review_fails(self) -> None:
        cases = (
            (self.reviews[:-1], "coverage mismatch"),
            (self.reviews + self.reviews[:1], "duplicate"),
            (self.reviews + [{**self.reviews[0], "line": "3"}], "coverage mismatch"),
            ([{**self.reviews[0], "capture_mode": "support-lib"}, self.reviews[1]], "coverage mismatch"),
            ([{**self.reviews[0], "feature_profile": "clustering"}, self.reviews[1]], "coverage mismatch"),
            ([{**self.reviews[0], "compiled_target_kind": "lib"}, self.reviews[1]], "coverage mismatch"),
            ([{**self.reviews[0], "compiled_target_name": "other_test"}, self.reviews[1]], "coverage mismatch"),
            ([{**self.reviews[0], "compiled_target_test_mode": "false"}, self.reviews[1]], "coverage mismatch"),
        )
        for rows, message in cases:
            with self.subTest(rows=rows), self.assertRaisesRegex(ValueError, message):
                validator.validate_reviews(self.root, self.candidates, rows)

    def test_invalid_semantics_commit_and_unrelated_source_citation_fail(self) -> None:
        first = self.reviews[0]
        other_line = "2" if first["line"] == "1" else "1"
        changes = (
            ({"source_commit": "0" * 40}, "different source commit"),
            ({"operation": "manual-review-required"}, "operation"),
            ({"classification": "unknown"}, "classification"),
            ({"classification": "transport;authority"}, "classification"),
            ({"operation": "type-only", "classification": "transport"}, "sole classification"),
            ({"rationale": "TODO source review required"}, "rationale"),
            ({"evidence": f"{self.rel}:{other_line} is a valid but unrelated citation."}, "cite the reviewed source location"),
            ({"evidence": f"{self.rel}:99 is outside the source file."}, "out of bounds"),
        )
        for change, message in changes:
            rows = copy.deepcopy(self.reviews)
            rows[0].update(change)
            with self.subTest(change=change), self.assertRaisesRegex(ValueError, message):
                validator.validate_reviews(self.root, self.candidates, rows)

    def test_raw_provenance_source_and_test_target_fail_closed(self) -> None:
        changes = (
            ({"capture_mode": "support-lib"}, "capture provenance"),
            ({"feature_profile": "clustering"}, "capture provenance"),
            ({"capture_sha256": "0" * 64}, "capture provenance"),
            ({"metadata_sha256": "0" * 64}, "capture provenance"),
            ({"tag": "T::UNKNOWN@L1C1"}, "target tag"),
            ({"compiled_target_kind": "lib"}, "target identity"),
            ({"compiled_target_name": "another_test"}, "target identity"),
            ({"compiled_target_test_mode": "false"}, "target identity"),
            ({"compiled_target_features": "[]"}, "use-site provenance"),
            ({"rw_classification": "read"}, "use-site provenance"),
            ({"kind": "OTHER"}, "use-site provenance"),
            ({"source": "stale content"}, "stale supervised_io"),
            ({"line": "99"}, "out of bounds"),
            ({"column": "999"}, "out of bounds"),
        )
        for change, message in changes:
            rows = copy.deepcopy(self.rows)
            rows[0].update(change)
            with self.subTest(change=change), self.assertRaisesRegex(ValueError, message):
                self.derive(rows)
        with self.assertRaisesRegex(ValueError, "duplicate"):
            self.derive(self.rows + self.rows[:1])
        with self.assertRaisesRegex(ValueError, "count does not match"):
            validator.derive_candidates(self.root, self.manifest, self.rows)

    def test_review_tsv_schema_and_final_newline_are_required(self) -> None:
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

    def test_reparse_hashes_and_every_retained_table_are_checked(self) -> None:
        """Hash fixture bytes; pin constants and the parser subprocess are mocked."""
        capture_dir = self.root / "capture"
        base = capture_dir / "test-support-supervised"
        base.mkdir(parents=True)
        capture, metadata = b"captured diagnostics\n", b'{"version":1}\n'
        capture_path, metadata_path = base / "diagnostics.jsonl.gz", capture_dir / "cargo-metadata.json.gz"
        capture_path.write_bytes(gzip.compress(capture))
        metadata_path.write_bytes(gzip.compress(metadata))
        capture_digest, metadata_digest = hashlib.sha256(capture).hexdigest(), hashlib.sha256(metadata).hexdigest()
        text = io.StringIO(newline="")
        writer = csv.DictWriter(text, fieldnames=validator.USE_FIELDS, delimiter="\t", lineterminator="\n")
        writer.writeheader()
        writer.writerows(self.rows)
        output = {"--output": text.getvalue().encode(), "--coverage": b"coverage fixture\n", "--inventory": b"inventory fixture\n"}
        retained = {}
        for option, suffix in (("--output", "uses"), ("--coverage", "coverage"), ("--inventory", "inventory")):
            path = base / f"w2046-4219-support-integration-supervised-{suffix}.tsv"
            path.write_bytes(output[option])
            retained[option] = path
        calls = []

        def fake_parser(command: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
            calls.append(command)
            for flag, expected in (("--capture-mode", validator.CAPTURE_MODE), ("--feature-profile", "default"),
                                   ("--test-target", "supervised_io"), ("--capture-sha256", capture_digest),
                                   ("--metadata-sha256", metadata_digest)):
                self.assertEqual(command[command.index(flag) + 1], expected)
            for flag, content in output.items():
                pathlib.Path(command[command.index(flag) + 1]).write_bytes(content)
            return subprocess.CompletedProcess(command, 0, stdout="", stderr="")

        destination = self.root / "regenerated"
        destination.mkdir()
        with mock.patch.object(validator, "CAPTURE_SHA256", capture_digest), mock.patch.object(validator, "METADATA_SHA256", metadata_digest), \
                mock.patch.object(validator.subprocess, "run", side_effect=fake_parser):
            self.assertEqual(validator.regenerated_uses(capture_dir, self.root, self.root / "manifest.tsv", destination), self.rows)
            self.assertEqual(len(calls), 1)
            for flag, path in retained.items():
                path.write_bytes(b"altered table\n")
                with self.subTest(table=flag), self.assertRaisesRegex(ValueError, "regenerated table differs"):
                    validator.regenerated_uses(capture_dir, self.root, self.root / "manifest.tsv", destination)
                path.write_bytes(output[flag])
            for path, original in ((capture_path, capture), (metadata_path, metadata)):
                path.write_bytes(gzip.compress(original + b"altered\n"))
                with self.subTest(path=path), self.assertRaisesRegex(ValueError, "SHA-256"):
                    validator.regenerated_uses(capture_dir, self.root, self.root / "manifest.tsv", destination)
                path.write_bytes(gzip.compress(original))


if __name__ == "__main__":
    unittest.main()
