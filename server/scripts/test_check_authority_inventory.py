"""Negative controls for the manual, deliberately lexical source-drift check."""
import copy
import hashlib
import tempfile
import unittest
from pathlib import Path

import check_authority_inventory as inventory


class InventoryTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        for scope in inventory.SCOPES:
            (self.root / scope).mkdir(parents=True)
        self.relative = inventory.SCOPES[0] + "/writer.rs"
        self.source = self.root / self.relative
        self.source.write_text("fn write_tuple() {}\n// WriteTuple candidate\n")
        self.manifest = {
            "format": 2,
            "baseline_commit": "a" * 40,
            "sources": [
                {
                    "path": self.relative,
                    "sha256": hashlib.sha256(self.source.read_bytes()).hexdigest(),
                },
            ],
            "anchors": {
                "writer": {"path": self.relative, "line": 1, "text": "fn write_tuple()"},
            },
            "lexical_tokens": ["write_tuple", "WriteTuple"],
            "lexical_snapshot": inventory.lexical_snapshot(
                self.root, ["write_tuple", "WriteTuple"]
            ),
        }

    def test_unchanged_source_matches_without_runtime_or_git(self):
        self.assertEqual(inventory.check(self.root, self.manifest), [])
        self.assertEqual(
            self.manifest["lexical_snapshot"]["occurrences"]["WriteTuple"], 1
        )  # A comment is counted, never promoted to a concrete call site.

    def test_changed_unmatched_content_in_reviewed_file_is_stale(self):
        self.source.write_text(self.source.read_text() + "// unrelated change\n")
        self.assertIn(
            f"reviewed file changed: {self.relative}",
            inventory.check(self.root, self.manifest),
        )

    def test_changed_or_shifted_anchor_is_stale_even_with_new_file_hash(self):
        for prefix in ["// shifted\n", "fn changed() {}\n"]:
            with self.subTest(prefix=prefix):
                self.source.write_text(prefix + "// WriteTuple candidate\n")
                self.manifest["sources"][0]["sha256"] = hashlib.sha256(
                    self.source.read_bytes()
                ).hexdigest()
                self.assertIn(
                    "source anchor changed: writer",
                    inventory.check(self.root, self.manifest),
                )

    def test_new_matching_site_in_either_scope_is_stale(self):
        for scope in inventory.SCOPES:
            with self.subTest(scope=scope):
                added = self.root / scope / "added.rs"
                added.write_text("fn new_path() { WriteTuple {} }\n")
                self.assertIn(
                    "raw lexical match inventory changed (not a call graph)",
                    inventory.check(self.root, self.manifest),
                )
                added.unlink()

    def test_unknown_vocabulary_in_unreviewed_file_is_explicitly_not_detected(self):
        (self.root / inventory.SCOPES[0] / "unknown.rs").write_text(
            "fn opaque_new_authority() {}\n"
        )
        self.assertEqual(inventory.check(self.root, self.manifest), [])

    def test_missing_reviewed_source_or_scope_fails_closed(self):
        self.source.unlink()
        with self.assertRaises(ValueError):
            inventory.check(self.root, self.manifest)
        (self.root / inventory.SCOPES[1]).rmdir()
        with self.assertRaises(ValueError):
            inventory.lexical_snapshot(self.root, self.manifest["lexical_tokens"])

    def test_invalid_manifest_and_unfingerprinted_anchor_fail_closed(self):
        for field, value in [
            ("format", 1),
            ("sources", {}),
            ("sources", []),
            ("sources", ["wrong shape"]),
            ("anchors", {}),
        ]:
            candidate = copy.deepcopy(self.manifest)
            candidate[field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                inventory.check(self.root, candidate)
        self.manifest["anchors"]["writer"]["path"] = inventory.SCOPES[1] + "/unknown.rs"
        with self.assertRaisesRegex(ValueError, "not fingerprinted"):
            inventory.check(self.root, self.manifest)

    def test_duplicate_source_paths_fail_closed(self):
        self.manifest["sources"].append(copy.deepcopy(self.manifest["sources"][0]))
        with self.assertRaisesRegex(ValueError, "duplicate source fingerprint"):
            inventory.check(self.root, self.manifest)

    def test_tokens_remain_ascii_and_distinct(self):
        for tokens in [[], ["1writer"], ["write-é"], ["writé"], ["writer", "writer"], [1]]:
            with self.subTest(tokens=tokens), self.assertRaises(ValueError):
                inventory.lexical_snapshot(self.root, tokens)
        self.assertEqual(
            inventory.lexical_snapshot(self.root, ["_writer_2"])["occurrences"],
            {"_writer_2": 0},
        )

    def test_source_escape_and_symlink_are_rejected(self):
        for relative in ["/etc/passwd", "../outside.rs", "server/other.rs"]:
            with self.subTest(relative=relative), self.assertRaises(ValueError):
                inventory.source_path(self.root, relative)
        self.source.unlink()
        self.source.symlink_to(self.root / "outside.rs")
        with self.assertRaises(ValueError):
            inventory.source_path(self.root, self.relative)

    def test_scan_does_not_silently_omit_symlinked_directories(self):
        (self.root / inventory.SCOPES[1] / "linked").symlink_to(
            self.root / inventory.SCOPES[0], target_is_directory=True
        )
        with self.assertRaisesRegex(ValueError, "symlinked directory"):
            inventory.lexical_snapshot(self.root, self.manifest["lexical_tokens"])


if __name__ == "__main__":
    unittest.main()
