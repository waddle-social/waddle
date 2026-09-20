import copy
import json
from pathlib import Path
import tempfile
import unittest

from check_nextest_shards import check, check_partition


class ShardInventoryTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.expected = {
            "test-count": 3,
            "rust-suites": {
                "server": {
                    "status": "listed",
                    "testcases": {
                        "same_name": self.case(),
                        "ignored": self.case(ignored=True),
                    },
                },
                "xmpp": {
                    "status": "listed",
                    "testcases": {"same_name": self.case()},
                },
            },
        }
        self.first = copy.deepcopy(self.expected)
        self.second = copy.deepcopy(self.expected)
        self.first["rust-suites"]["xmpp"]["testcases"]["same_name"] = self.case(selected=False)
        self.second["rust-suites"]["server"]["testcases"]["same_name"] = self.case(selected=False)

    @staticmethod
    def case(selected=True, ignored=False):
        match = {"status": "matches"}
        if ignored or not selected:
            match = {"status": "mismatch", "reason": "ignored" if ignored else "partition"}
        return {"kind": "test", "ignored": ignored, "filter-match": match}

    def run_check(self):
        paths = []
        for name, document in [("expected", self.expected), ("first", self.first), ("second", self.second)]:
            path = self.root / f"{name}.json"
            path.write_text(json.dumps(document))
            paths.append(path)
        return check(paths[0], paths[1:], count=2)

    def test_complete_union_preserves_ignored_and_duplicate_names_across_binaries(self):
        result = self.run_check()
        self.assertEqual(result["selected_tests"], 2)
        self.assertEqual(result["ignored_tests"], 1)
        self.assertEqual(result["shard_counts"], [1, 1])

    def test_duplicate_assignment_fails(self):
        self.second["rust-suites"]["server"]["testcases"]["same_name"] = self.case()
        with self.assertRaisesRegex(ValueError, "multiple shards"):
            self.run_check()

    def test_equal_count_with_a_different_test_fails(self):
        tests = self.first["rust-suites"]["server"]["testcases"]
        tests["renamed"] = tests.pop("same_name")
        with self.assertRaisesRegex(ValueError, "identities or ignored status"):
            self.run_check()

    def test_ignored_test_cannot_be_enabled(self):
        self.first["rust-suites"]["server"]["testcases"]["ignored"]["filter-match"] = {"status": "matches"}
        with self.assertRaisesRegex(ValueError, "unexpected selected"):
            self.run_check()

    def test_omitted_binary_fails(self):
        del self.first["rust-suites"]["xmpp"]
        self.first["test-count"] -= 1
        with self.assertRaisesRegex(ValueError, "identities or ignored status"):
            self.run_check()

    def test_missing_shard_fails(self):
        with self.assertRaisesRegex(ValueError, "expected 4 shards"):
            check(self.root / "unused", [], count=4)

    def test_a_test_excluded_by_every_shard_fails(self):
        for document in (self.expected, self.first, self.second):
            document["test-count"] += 1
            document["rust-suites"]["server"]["testcases"]["missing"] = self.case(
                selected=document is self.expected
            )
        with self.assertRaisesRegex(ValueError, "missing from the shard union"):
            self.run_check()

    def test_consumer_selection_must_match_its_producer_partition(self):
        self.run_check()
        with self.assertRaisesRegex(ValueError, "selection differ"):
            check_partition(self.root / "first.json", self.root / "second.json")
        result = check_partition(self.root / "first.json", self.root / "first.json")
        self.assertEqual(result["selected_tests"], 1)


if __name__ == "__main__":
    unittest.main()
