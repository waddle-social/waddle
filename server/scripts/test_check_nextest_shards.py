import copy
import gzip
import json
from pathlib import Path
import subprocess
import sys
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
                "empty": {"status": "listed", "testcases": {}},
                "ignored_only": {"status": "listed", "testcases": {"ignored": self.case(ignored=True)}},
            },
        }
        self.expected["test-count"] += 1
        for binary_id, suite in self.expected["rust-suites"].items():
            suite.update({
                "binary-id": binary_id, "binary-name": binary_id,
                "package-name": binary_id, "kind": "lib", "build-platform": "target",
            })
        self.first = copy.deepcopy(self.expected)
        self.second = copy.deepcopy(self.expected)
        for binary_id in ("xmpp", "ignored_only"):
            self.skip(self.first, binary_id)
        for binary_id in ("server", "empty"):
            self.skip(self.second, binary_id)

    @staticmethod
    def skip(document, binary_id):
        suite = document["rust-suites"][binary_id]
        document["test-count"] -= len(suite["testcases"])
        suite["status"] = "skipped"
        suite["testcases"] = {}

    @staticmethod
    def archived(document):
        result = copy.deepcopy(document)
        result["rust-suites"] = {
            key: suite for key, suite in result["rust-suites"].items() if suite["status"] == "listed"
        }
        return result

    def write(self, name, document):
        path = self.root / f"{name}.json"
        path.write_text(json.dumps(document))
        return path

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

    def test_compressed_partition_retains_all_coverage_checks(self):
        expected = self.root / "expected.json.gz"
        expected.write_bytes(gzip.compress(json.dumps(self.first).encode(), mtime=0))
        actual = self.write("actual", self.archived(self.first))
        plain = self.write("expected", self.first)
        self.assertEqual(check_partition(expected, actual), check_partition(plain, actual))
        changed = self.archived(self.first)
        changed["rust-suites"]["server"]["testcases"]["same_name"]["ignored"] = True
        with self.assertRaisesRegex(ValueError, "partition identities"):
            check_partition(expected, self.write("changed", changed))

    def test_complete_union_preserves_ignored_and_duplicate_names_across_binaries(self):
        result = self.run_check()
        self.assertEqual(result["selected_tests"], 2)
        self.assertEqual(result["ignored_tests"], 2)
        self.assertEqual(result["shard_counts"], [1, 1])
        self.first = self.archived(self.first)
        self.second = self.archived(self.second)
        self.assertEqual(self.run_check(), result)

    def test_duplicate_assignment_fails(self):
        for binary_id in ("server", "empty", "ignored_only"):
            with self.subTest(binary_id=binary_id):
                destination = self.second if binary_id != "ignored_only" else self.first
                saved = copy.deepcopy(destination)
                destination["rust-suites"][binary_id] = copy.deepcopy(self.expected["rust-suites"][binary_id])
                destination["test-count"] += len(destination["rust-suites"][binary_id]["testcases"])
                with self.assertRaisesRegex(ValueError, "multiple shards"):
                    self.run_check()
                destination.clear()
                destination.update(saved)

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
        for binary_id in ("server", "empty", "ignored_only"):
            with self.subTest(binary_id=binary_id):
                source = self.first if binary_id != "ignored_only" else self.second
                saved = copy.deepcopy(source)
                self.skip(source, binary_id)
                with self.assertRaisesRegex(ValueError, "missing from the shard union|selects no tests"):
                    self.run_check()
                source.clear()
                source.update(saved)

    def test_missing_shard_fails(self):
        with self.assertRaisesRegex(ValueError, "expected 4 shards"):
            check(self.root / "unused", [], count=4)

    def test_a_test_excluded_by_every_shard_fails(self):
        for document in (self.expected, self.first):
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

    def test_consumer_omission_of_producer_skipped_binaries_is_valid(self):
        result = check_partition(self.write("producer", self.first), self.write("archive", self.archived(self.first)))
        self.assertEqual(result, {"binaries": 2, "selected_tests": 1})

    def test_unknown_binary_fails_even_when_skipped(self):
        suite = copy.deepcopy(self.first["rust-suites"]["empty"])
        suite["binary-id"] = "unknown"
        self.first["rust-suites"]["unknown"] = suite
        for status in ("listed", "skipped"):
            suite["status"] = status
            with self.subTest(status=status), self.assertRaisesRegex(ValueError, "unknown binary"):
                self.run_check()

    def test_changed_binary_kind_or_platform_fails(self):
        for field, value in (("kind", "bench"), ("build-platform", "host")):
            original = self.first["rust-suites"]["server"][field]
            self.first["rust-suites"]["server"][field] = value
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, "kind or metadata differ"):
                self.run_check()
            self.first["rust-suites"]["server"][field] = original

    def test_invalid_suite_status_and_skipped_cases_fail(self):
        suite = self.first["rust-suites"]["server"]
        suite["status"] = "unknown"
        with self.assertRaisesRegex(ValueError, "unknown status"):
            self.run_check()
        suite["status"] = "skipped"
        with self.assertRaisesRegex(ValueError, "skipped binary.*contains testcases"):
            self.run_check()

    def test_original_inventory_must_be_complete(self):
        self.expected["rust-suites"]["empty"]["status"] = "skipped"
        with self.assertRaisesRegex(ValueError, "original inventory must list every binary"):
            self.run_check()

    def test_bad_count_and_unknown_filter_status_fail(self):
        self.first["test-count"] += 1
        with self.assertRaisesRegex(ValueError, "test-count disagrees"):
            self.run_check()
        self.first["test-count"] -= 1
        self.first["rust-suites"]["server"]["testcases"]["same_name"]["filter-match"]["status"] = "unknown"
        with self.assertRaisesRegex(ValueError, "unknown filter status"):
            self.run_check()

    def test_consumer_missing_empty_binary_or_ignored_case_fails(self):
        expected = self.write("producer", self.first)
        actual = self.archived(self.first)
        del actual["rust-suites"]["empty"]
        with self.assertRaisesRegex(ValueError, "identities, ignored status or selection"):
            check_partition(expected, self.write("actual", actual))
        actual = self.archived(self.first)
        del actual["rust-suites"]["server"]["testcases"]["ignored"]
        actual["test-count"] -= 1
        with self.assertRaisesRegex(ValueError, "identities, ignored status or selection"):
            check_partition(expected, self.write("actual", actual))

    def test_consumer_unknown_binary_and_changed_kind_fail(self):
        expected = self.write("producer", self.first)
        actual = self.archived(self.first)
        suite = copy.deepcopy(actual["rust-suites"]["empty"])
        suite["binary-id"] = "unknown"
        actual["rust-suites"]["unknown"] = suite
        with self.assertRaisesRegex(ValueError, "unknown binary"):
            check_partition(expected, self.write("actual", actual))
        del actual["rust-suites"]["unknown"]
        actual["rust-suites"]["empty"]["kind"] = "test"
        with self.assertRaisesRegex(ValueError, "kind or metadata differ"):
            check_partition(expected, self.write("actual", actual))


class SharedBinaryInventoryTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.shared_ids = ["server", "cluster"]
        suites = {}
        for binary_id in [f"whole-{index}" for index in range(4)] + self.shared_ids:
            cases = {"test": ShardInventoryTests.case()}
            if binary_id in self.shared_ids:
                cases = {f"test-{index}": ShardInventoryTests.case() for index in range(4)}
                cases["ignored"] = ShardInventoryTests.case(ignored=True)
            suites[binary_id] = {
                "binary-id": binary_id, "binary-name": binary_id, "package-name": binary_id,
                "kind": "lib", "build-platform": "target", "status": "listed", "testcases": cases,
            }
        self.expected = {"test-count": 14, "rust-suites": suites}
        self.phases = []
        for index in range(4):
            for included in ({f"whole-{index}"}, set(self.shared_ids)):
                phase = copy.deepcopy(self.expected)
                for binary_id in suites:
                    if binary_id not in included:
                        ShardInventoryTests.skip(phase, binary_id)
                    elif binary_id in self.shared_ids:
                        for other in range(4):
                            phase["rust-suites"][binary_id]["testcases"][f"test-{other}"] = ShardInventoryTests.case(selected=other == index)
                self.phases.append(phase)

    def paths(self):
        result = []
        for index, document in enumerate([self.expected] + self.phases):
            path = self.root / f"inventory-{index}.json"
            path.write_text(json.dumps(document))
            result.append(path)
        return result

    def run_check(self, shared=None):
        paths = self.paths()
        return check(paths[0], paths[1:], count=8, shared_binary_ids=self.shared_ids if shared is None else shared)

    def test_eight_phases_preserve_all_tests_and_shared_ignored_cases(self):
        result = self.run_check()
        self.assertEqual(result, {
            "binaries": 6, "test_count": 14, "selected_tests": 12,
            "ignored_tests": 2, "shard_counts": [1, 2, 1, 2, 1, 2, 1, 2],
        })
        self.phases = [ShardInventoryTests.archived(phase) for phase in self.phases]
        self.assertEqual(self.run_check(), result)

    def test_shared_repetition_requires_explicit_known_ids(self):
        with self.assertRaisesRegex(ValueError, "binaries assigned to multiple shards"):
            self.run_check(shared=[])
        with self.assertRaisesRegex(ValueError, "unknown shared binary IDs"):
            self.run_check(shared=["unknown"])

    def test_shared_selected_test_still_cannot_repeat(self):
        self.phases[3]["rust-suites"]["server"]["testcases"]["test-0"] = ShardInventoryTests.case()
        with self.assertRaisesRegex(ValueError, "tests selected by multiple shards"):
            self.run_check()

    def test_shared_selected_test_cannot_disappear(self):
        self.phases[7]["rust-suites"]["server"]["testcases"]["test-3"] = ShardInventoryTests.case(selected=False)
        with self.assertRaisesRegex(ValueError, "tests missing from the shard union"):
            self.run_check()

    def test_every_shared_copy_must_preserve_ignored_case_classification(self):
        self.phases[3]["rust-suites"]["server"]["testcases"]["ignored"]["ignored"] = False
        with self.assertRaisesRegex(ValueError, "identities or ignored status"):
            self.run_check()

    def test_nonshared_binary_cannot_repeat_when_shared_ids_are_enabled(self):
        self.phases[2]["rust-suites"]["whole-0"] = copy.deepcopy(self.expected["rust-suites"]["whole-0"])
        self.phases[2]["test-count"] += 1
        with self.assertRaisesRegex(ValueError, "binaries assigned to multiple shards"):
            self.run_check()

    def test_worker_must_match_its_specific_shared_count_partition(self):
        paths = self.paths()
        with self.assertRaisesRegex(ValueError, "selection differ"):
            check_partition(paths[2], paths[4])

    def test_cli_accepts_eight_phases_and_repeated_shared_ids(self):
        paths = self.paths()
        process = subprocess.run(
            [sys.executable, str(Path(__file__).with_name("check_nextest_shards.py")),
             *map(str, paths), "--count", "8", "--shared-binary", "server", "--shared-binary", "cluster"],
            capture_output=True, text=True,
        )
        self.assertEqual(process.returncode, 0, process.stderr)
        self.assertEqual(json.loads(process.stdout)["selected_tests"], 12)


if __name__ == "__main__":
    unittest.main()
