#!/usr/bin/env python3
"""Synthetic checks for shared production/support semantic review coverage."""

from __future__ import annotations

import contextlib
import copy
import csv
import io
import json
import pathlib
import tempfile
import unittest
from unittest import mock

import validate_shared_support_reviews as shared
import validate_support_reviews as support


class SharedSupportReviewTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = pathlib.Path(self.tmp.name)
        self.rel = "server/crates/example/src/lib.rs"
        path = self.root / self.rel
        path.parent.mkdir(parents=True)
        path.write_text("let common = marker();\nlet cluster = marker();\nlet extra = marker();\n")
        self.manifest = [{"tag": "T::COMMON@L1C1"}, {"tag": "T::CLUSTER@L2C1"}, {"tag": "T::EXTRA@L3C1"}]
        self.tables = {}
        for profile in support.CAPTURES:
            lines = [1, 2] if profile == "clustering" else [1]
            self.tables[profile] = {
                "production": [self.use(profile, line, support_mode=False) for line in lines],
                "support": [self.use(profile, line, support_mode=True) for line in [*lines, 3]],
            }
        self.candidates = self.derive(self.tables)
        self.reviews = []
        for key in sorted(self.candidates):
            row = {"source_commit": shared.SOURCE_COMMIT, **dict(zip(support.REVIEW_KEY, key)),
                   "operation": "call", "classification": "transport",
                   "rationale": "The selected call transports the message to its destination."}
            row["evidence"] = f"{self.rel}:{row['line']} shows the selected message call."
            self.reviews.append(row)

    def use(self, profile: str, line: int, *, support_mode: bool) -> dict[str, str]:
        tags = {1: "T::COMMON@L1C1", 2: "T::CLUSTER@L2C1", 3: "T::EXTRA@L3C1"}
        common = {
            "tag": tags[line], "kind": "T", "source_class_hint": "prod", "file": self.rel,
            "line": str(line), "column": "1", "compiled_target_kind": "lib", "compiled_target_name": "waddle_server",
            "compiled_target_test_mode": "false", "enclosing_fn": "fixture", "rw_classification": "manual-review-required",
            "source": (self.root / self.rel).read_text().splitlines()[line - 1],
        }
        if not support_mode:
            return {"profile": profile, **common}
        features = ["clustering", "test-support"] if profile == "clustering" else ["test-support"]
        return {"schema_version": support.SCHEMA_VERSION, "capture_mode": "support-lib", "feature_profile": profile,
                "capture_sha256": support.CAPTURES[profile]["support_sha256"], "metadata_sha256": support.METADATA_SHA256,
                "compiled_package_id": "path+file:///archive/server/crates/waddle-server#0.1.0",
                "compiled_target_features": json.dumps(features, separators=(",", ":")), **common}

    def derive(self, tables: dict[str, dict[str, list[dict[str, str]]]]) -> dict[tuple[str, ...], dict[str, str]]:
        return shared.derive_candidates(self.root, self.manifest, tables, require_pinned_counts=False)

    def test_intersection_and_explicit_per_profile_reviews(self) -> None:
        self.assertEqual({(key[0], key[1]) for key in self.candidates},
                         {("default", "T::COMMON@L1C1"), ("clustering", "T::COMMON@L1C1"), ("clustering", "T::CLUSTER@L2C1")})
        self.assertTrue(all(row["rw_classification"] == "manual-review-required" for row in self.candidates.values()))
        support.validate_reviews(self.root, self.candidates, self.reviews)
        updated = [{**row, "operation": "call;read", "classification": "authority;transport"} for row in self.reviews]
        support.validate_reviews(self.root, self.candidates, updated)
        with self.assertRaisesRegex(ValueError, "coverage mismatch"):
            support.validate_reviews(self.root, self.candidates, self.reviews[:-1])

    def test_missing_duplicate_extra_or_wrong_profile_reviews_fail(self) -> None:
        cases = (
            (self.reviews + self.reviews[:1], "duplicate"),
            (self.reviews + [{**self.reviews[0], "tag": "T::EXTRA@L3C1", "line": "3"}], "coverage mismatch"),
            ([{**self.reviews[0], "feature_profile": "unknown"}, *self.reviews[1:]], "coverage mismatch"),
            ([{**self.reviews[0], "compiled_target_name": "waddle_xmpp"}, *self.reviews[1:]], "coverage mismatch"),
            ([{**self.reviews[0], "line": "3"}, *self.reviews[1:]], "coverage mismatch"),
        )
        for rows, message in cases:
            with self.subTest(rows=rows), self.assertRaisesRegex(ValueError, message):
                support.validate_reviews(self.root, self.candidates, rows)

    def test_semantic_rules_and_exact_source_citation_are_required(self) -> None:
        row = self.reviews[0]
        unrelated = "3" if row["line"] != "3" else "1"
        changes = (
            ({"source_commit": "0" * 40}, "different source commit"),
            ({"operation": "manual-review-required"}, "operation"),
            ({"classification": "unclassified"}, "classification"),
            ({"classification": "transport;authority"}, "classification"),
            ({"operation": "type-only", "classification": "transport"}, "sole classification"),
            ({"rationale": "TODO source review required"}, "rationale"),
            ({"evidence": f"{self.rel}:{unrelated} is a valid but unrelated citation."}, "cite the reviewed source location"),
            ({"evidence": f"{self.rel}:99 is outside the source file."}, "out of bounds"),
        )
        for change, error in changes:
            rows = copy.deepcopy(self.reviews)
            rows[0].update(change)
            with self.subTest(change=change), self.assertRaisesRegex(ValueError, error):
                support.validate_reviews(self.root, self.candidates, rows)

    def test_source_and_capture_inputs_fail_closed(self) -> None:
        mutations = (
            ({"feature_profile": "clustering"}, "unknown compiler"),
            ({"capture_mode": "server-unit"}, "capture provenance"),
            ({"capture_sha256": "0" * 64}, "capture provenance"),
            ({"metadata_sha256": "0" * 64}, "capture provenance"),
            ({"tag": "T::UNKNOWN@L1C1"}, "unknown compiler"),
            ({"compiled_target_features": "[]"}, "feature set"),
            ({"source": "stale text"}, "stale compiler"),
            ({"line": "99"}, "out of bounds"),
        )
        for change, error in mutations:
            tables = copy.deepcopy(self.tables)
            tables["default"]["support"][0].update(change)
            with self.subTest(change=change), self.assertRaisesRegex(ValueError, error):
                self.derive(tables)
        missing = copy.deepcopy(self.tables)
        missing["default"]["support"].pop(0)
        with self.assertRaisesRegex(ValueError, "omits 1 production"):
            self.derive(missing)
        duplicate = copy.deepcopy(self.tables)
        duplicate["default"]["production"].append(copy.deepcopy(duplicate["default"]["production"][0]))
        with self.assertRaisesRegex(ValueError, "duplicate"):
            self.derive(duplicate)
        with self.assertRaisesRegex(ValueError, "both shared-site"):
            self.derive({"default": self.tables["default"]})
        with self.assertRaisesRegex(ValueError, "counts do not match"):
            shared.derive_candidates(self.root, self.manifest, self.tables)

    def test_cli_combines_review_files_and_rejects_incomplete_or_duplicate_rows(self) -> None:
        """Exercise CLI coverage with synthetic tables; source checks and reparsing are mocked."""
        profiles = copy.deepcopy(support.CAPTURES)
        profiles["default"].update(production_count=1, support_count=2, additional_count=1)
        profiles["clustering"].update(production_count=2, support_count=3, additional_count=1)
        paths = {profile: self.root / f"{profile}.tsv" for profile in profiles}

        def write_reviews(profile: str, rows: list[dict[str, str]]) -> None:
            with paths[profile].open("w", newline="") as output:
                writer = csv.DictWriter(output, fieldnames=support.REVIEW_FIELDS, delimiter="\t", lineterminator="\n")
                writer.writeheader()
                writer.writerows(rows)

        for profile in profiles:
            write_reviews(profile, [row for row in self.reviews if row["feature_profile"] == profile])
        args = ["validate_shared_support_reviews.py", "--source-root", str(self.root), "--patched-root", str(self.root),
                "--capture-dir", str(self.root), "--reviews", str(paths["default"]), "--reviews", str(paths["clustering"])]
        with mock.patch.object(support, "CAPTURES", profiles), mock.patch.object(shared, "load_manifest", return_value=self.manifest), \
                mock.patch.object(support, "validate_source_tree"), mock.patch.object(support, "regenerated_tables", return_value=self.tables), \
                mock.patch("sys.argv", args), contextlib.redirect_stdout(io.StringIO()) as stdout:
            shared.main()
            self.assertIn("validated 3 profile-specific shared compiler-site reviews", stdout.getvalue())
            write_reviews("clustering", [row for row in self.reviews if row["feature_profile"] == "clustering"][:1])
            with self.assertRaisesRegex(SystemExit, "coverage mismatch"):
                shared.main()
            write_reviews("clustering", [row for row in self.reviews if row["feature_profile"] == "clustering"] + [self.reviews[0]])
            with self.assertRaisesRegex(SystemExit, "duplicate"):
                shared.main()


if __name__ == "__main__":
    unittest.main()
