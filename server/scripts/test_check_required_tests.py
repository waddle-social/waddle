#!/usr/bin/env python3
"""Contract tests for the required-test executed-name check."""
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("check_required_tests.py")
REQUIRED_LIST = Path(__file__).parent.parent / ".config/required-cluster-tests.txt"

# The JUnit shape cargo-nextest writes (one testsuite per binary id).
REPORT = """<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="3" failures="{failures}" errors="0">
    <testsuite name="waddle-server::clustering_cluster_e2e" tests="2">
        <testcase name="lone_survivor_and_isolation_fencing" classname="waddle-server::clustering_cluster_e2e" time="12.5"/>
        <testcase name="muc_join_routes_to_foreign_room_owner" classname="waddle-server::clustering_cluster_e2e" time="9.1">{failure}</testcase>
    </testsuite>
    <testsuite name="waddle-server" tests="1">
        <testcase name="ingress::tests::postgres_case" classname="waddle-server" time="0.4"/>
    </testsuite>
</testsuites>
"""


class RequiredTestTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.required = self.root / "required.txt"
        self.report = self.root / "junit.xml"
        self.report.write_text(REPORT.format(failures=0, failure=""))
        self.executed = self.root / "executed.txt"

    def run_check(self, *reports):
        return subprocess.run(
            [sys.executable, str(SCRIPT), "--required", str(self.required),
             "--executed-out", str(self.executed), *map(str, reports or (self.report,))],
            text=True, capture_output=True, check=False)

    def test_all_required_tests_executed_passes_and_emits_names(self):
        self.required.write_text(
            "# two-process harness\n"
            "waddle-server::clustering_cluster_e2e lone_survivor_and_isolation_fencing\n"
            "\n"
            "waddle-server ingress::tests::postgres_case  # lib unit test\n")
        result = self.run_check()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.executed.read_text().splitlines(), [
            "waddle-server ingress::tests::postgres_case",
            "waddle-server::clustering_cluster_e2e lone_survivor_and_isolation_fencing",
            "waddle-server::clustering_cluster_e2e muc_join_routes_to_foreign_room_owner",
        ])

    def test_required_test_that_did_not_run_fails_with_its_name(self):
        self.required.write_text(
            "waddle-server::clustering_cluster_e2e lone_survivor_and_isolation_fencing\n"
            "waddle-server::xep0198_cross_node_resume pure_two_node_detached_steal_race_exactly_one_winner\n")
        result = self.run_check()
        self.assertEqual(result.returncode, 1)
        self.assertIn("did not run: waddle-server::xep0198_cross_node_resume "
                      "pure_two_node_detached_steal_race_exactly_one_winner", result.stderr)
        self.assertTrue(self.executed.is_file(), "executed names are emitted even on failure")

    def test_same_name_in_another_binary_does_not_count(self):
        self.required.write_text("waddle-server::other_binary lone_survivor_and_isolation_fencing\n")
        self.assertEqual(self.run_check().returncode, 1)

    def test_skipped_testcase_does_not_count_as_executed(self):
        self.report.write_text(REPORT.format(failures=0, failure="<skipped/>"))
        self.required.write_text(
            "waddle-server::clustering_cluster_e2e muc_join_routes_to_foreign_room_owner\n")
        self.assertEqual(self.run_check().returncode, 1)

    def test_failed_required_test_fails(self):
        self.report.write_text(REPORT.format(
            failures=1, failure='<failure type="test failure">panicked</failure>'))
        self.required.write_text(
            "waddle-server::clustering_cluster_e2e muc_join_routes_to_foreign_room_owner\n")
        result = self.run_check()
        self.assertEqual(result.returncode, 1)
        self.assertIn("required test failed", result.stderr)

    def test_names_from_several_shard_reports_are_combined(self):
        other = self.root / "shard-2.xml"
        other.write_text(
            '<testsuites><testsuite name="waddle-server::db_lineage_multiprocess">'
            '<testcase name="split_deployment_rejected" classname="waddle-server::db_lineage_multiprocess"/>'
            "</testsuite></testsuites>")
        self.required.write_text(
            "waddle-server::db_lineage_multiprocess split_deployment_rejected\n"
            "waddle-server::clustering_cluster_e2e lone_survivor_and_isolation_fencing\n")
        self.assertEqual(self.run_check(self.report, other).returncode, 0)

    def test_malformed_inputs_are_rejected(self):
        for content in ("only-one-field\n", "a b c\n", "# nothing required\n",
                        "bin test\nbin test\n"):
            self.required.write_text(content)
            self.assertEqual(self.run_check().returncode, 2, content)
        self.required.write_text("bin test\n")
        self.report.write_text("<testsuites><testsuite>")
        self.assertEqual(self.run_check().returncode, 2)

    def test_committed_list_parses(self):
        result = subprocess.run(
            [sys.executable, str(SCRIPT), "--required", str(REQUIRED_LIST), str(self.report)],
            text=True, capture_output=True, check=False)
        # The fixture report lacks most required tests; only parsing must succeed.
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("required test did not run", result.stderr)


if __name__ == "__main__":
    unittest.main()
