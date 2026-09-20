import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from plan_nextest_binary_shards import binary_filter, plan


class BinaryShardPlannerTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.inventory = self.root / "inventory.json"

    def write_inventory(self, sizes):
        suites = {}
        for index, (binary_id, size) in enumerate(sizes):
            path = self.root / f"binary-{index}"
            path.write_bytes(b"\x7fELF" + bytes(size - 4))
            suites[binary_id] = {
                "binary-id": binary_id,
                "binary-path": str(path),
                "status": "listed",
                "testcases": {},
            }
        document = {"test-count": 0, "rust-suites": suites}
        self.inventory.write_text(json.dumps(document))
        return document

    def test_balances_by_bytes_and_assigns_every_binary_once(self):
        sizes = [(f"crate::{index}", size * 100) for index, size in enumerate(range(8, 0, -1))]
        self.write_inventory(sizes)
        result = plan(self.inventory)
        self.assertEqual([shard["bytes"] for shard in result["shards"]], [900] * 4)
        assigned = [binary["binary_id"] for shard in result["shards"] for binary in shard["binaries"]]
        self.assertCountEqual(assigned, [binary_id for binary_id, _ in sizes])
        self.assertEqual(result["bytes"], 3600)
        self.assertEqual(result["binary_count"], 8)

    def test_ties_and_json_order_are_deterministic(self):
        document = self.write_inventory([(name, 100) for name in ["d", "c", "b", "a", "e"]])
        first = plan(self.inventory)
        document["rust-suites"] = dict(reversed(list(document["rust-suites"].items())))
        self.inventory.write_text(json.dumps(document))
        self.assertEqual(first, plan(self.inventory))
        self.assertEqual(
            [[binary["binary_id"] for binary in shard["binaries"]] for shard in first["shards"]],
            [["a", "e"], ["b"], ["c"], ["d"]],
        )

    def test_ignored_only_and_empty_binaries_remain_assigned(self):
        document = self.write_inventory([("empty", 50), ("ignored", 60), ("first", 70), ("second", 80)])
        document["rust-suites"]["ignored"]["testcases"] = {
            "ignored_case": {"kind": "test", "ignored": True, "filter-match": {"status": "mismatch", "reason": "ignored"}}
        }
        document["test-count"] = 1
        self.inventory.write_text(json.dumps(document))
        result = plan(self.inventory)
        self.assertTrue(all(len(shard["binaries"]) == 1 for shard in result["shards"]))
        self.assertCountEqual(
            [shard["binaries"][0]["binary_id"] for shard in result["shards"]],
            ["empty", "ignored", "first", "second"],
        )

    def test_safe_exact_filter_encoding_for_target_symbols(self):
        self.assertEqual(binary_filter(["foo-bar::bin/foo_bar.1"]), r"binary_id(=foo-bar::bin\u{2f}foo_bar.1)")
        self.assertEqual(
            binary_filter(["a) | all(),\\*?/#=~\n\t\ré😀"]),
            r"binary_id(=a\u{29}\u{20}\u{7c}\u{20}all\u{28}\u{29}\u{2c}\u{5c}\u{2a}\u{3f}\u{2f}\u{23}\u{3d}\u{7e}\u{a}\u{9}\u{d}\u{e9}\u{1f600})",
        )
        self.assertEqual(binary_filter(["b", "a"]), "binary_id(=a) | binary_id(=b)")

    def test_invalid_filter_ids_fail(self):
        for ids in ([], [""], ["bad\ud800"]):
            with self.subTest(ids=repr(ids)), self.assertRaises(ValueError):
                binary_filter(ids)

    def test_insufficient_binaries_and_invalid_count_fail(self):
        self.write_inventory([("one", 100)])
        with self.assertRaisesRegex(ValueError, "cannot fill 4 whole-binary shards with only 1 nonshared binaries"):
            plan(self.inventory)
        for count in (0, -1):
            with self.assertRaisesRegex(ValueError, "must be positive"):
                plan(self.inventory, count)

    def test_incomplete_inventory_or_mismatched_binary_id_fails(self):
        for field, value, message in (("status", "skipped", "list every binary"), ("binary-id", "other", "inventory key")):
            document = self.write_inventory([("one", 100)])
            document["rust-suites"]["one"][field] = value
            self.inventory.write_text(json.dumps(document))
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, message):
                plan(self.inventory, 1)

    def test_missing_nonregular_and_nonelf_binaries_fail(self):
        document = self.write_inventory([("one", 100)])
        path = Path(document["rust-suites"]["one"]["binary-path"])
        path.unlink()
        with self.assertRaises(FileNotFoundError):
            plan(self.inventory, 1)
        path.mkdir()
        with self.assertRaisesRegex(ValueError, "regular file"):
            plan(self.inventory, 1)
        path.rmdir()
        path.write_bytes(b"not ELF")
        with self.assertRaisesRegex(ValueError, "ELF file"):
            plan(self.inventory, 1)

    def test_cli_writes_plan_and_four_filters(self):
        self.write_inventory([(name, 100) for name in ["a", "b", "c", "d"]])
        output = self.root / "output"
        process = subprocess.run(
            [sys.executable, str(Path(__file__).with_name("plan_nextest_binary_shards.py")), str(self.inventory), str(output)],
            capture_output=True, text=True,
        )
        self.assertEqual(process.returncode, 0, process.stderr)
        result = json.loads((output / "plan.json").read_text())
        self.assertEqual(result, plan(self.inventory))
        self.assertEqual(len(list(output.iterdir())), 10)
        self.assertEqual((output / "shared.filter").read_text(), "none()\n")
        for shard in result["shards"]:
            self.assertEqual((output / f"partition-{shard['index']}.filter").read_text(), shard["filter"] + "\n")
            self.assertEqual((output / f"partition-{shard['index']}.whole.filter").read_text(), shard["whole_filter"] + "\n")

    def test_shared_binaries_replicate_without_changing_owned_balance(self):
        sizes = [(name, size) for name, size in zip("abcdefgh", range(800, 0, -100))]
        sizes += [("server", 2000), ("cluster", 1500)]
        self.write_inventory(sizes)
        result = plan(self.inventory, shared_binary_ids=["server", "cluster"])
        self.assertEqual(result["shared_bytes"], 3500)
        self.assertEqual(result["shared_filter"], "binary_id(=cluster) | binary_id(=server)")
        self.assertEqual([shard["whole_bytes"] for shard in result["shards"]], [900] * 4)
        self.assertEqual([shard["bytes"] for shard in result["shards"]], [4400] * 4)
        owned = []
        for shard in result["shards"]:
            whole_ids = [binary["binary_id"] for binary in shard["whole_binaries"]]
            owned.extend(whole_ids)
            self.assertCountEqual([binary["binary_id"] for binary in shard["binaries"]], whole_ids + ["server", "cluster"])
            self.assertNotIn("server", shard["whole_filter"])
            self.assertIn("binary_id(=server)", shard["filter"])
        self.assertCountEqual(owned, list("abcdefgh"))
        self.assertEqual(result, plan(self.inventory, shared_binary_ids=["cluster", "server"]))

    def test_shared_ids_must_exist_and_leave_enough_owned_binaries(self):
        self.write_inventory([(name, 100) for name in "abcd"])
        with self.assertRaisesRegex(ValueError, "unknown shared binary IDs"):
            plan(self.inventory, shared_binary_ids=["unknown"])
        with self.assertRaisesRegex(ValueError, "only 3 nonshared binaries"):
            plan(self.inventory, shared_binary_ids=["a"])

    def test_cli_accepts_repeated_shared_binary_options(self):
        self.write_inventory([(name, 100) for name in "abcdef"])
        output = self.root / "output"
        process = subprocess.run(
            [sys.executable, str(Path(__file__).with_name("plan_nextest_binary_shards.py")), str(self.inventory), str(output),
             "--shared-binary", "e", "--shared-binary", "f"],
            capture_output=True, text=True,
        )
        self.assertEqual(process.returncode, 0, process.stderr)
        self.assertEqual((output / "shared.filter").read_text(), "binary_id(=e) | binary_id(=f)\n")
        self.assertEqual(json.loads((output / "plan.json").read_text()), plan(self.inventory, shared_binary_ids=["e", "f"]))


if __name__ == "__main__":
    unittest.main()
