#!/usr/bin/env python3
"""Contract tests for the required-test executed-name check."""
from pathlib import Path
import hashlib
import json
import subprocess
import sys
import tempfile
import unittest

from check_required_tests import LANE_ID, PREREQUISITE_ANCHORS

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


def junit_cases(test_names, *, failures=frozenset(), skipped=frozenset()):
    cases = []
    for test in test_names:
        binary, name = test.split(" ", 1)
        if test in failures:
            result = '<failure type="test failure">fixture failure</failure>'
        elif test in skipped:
            result = "<skipped/>"
        else:
            result = ""
        cases.append(f'<testcase name="{name}" classname="{binary}">{result}</testcase>')
    return "<testsuites><testsuite>" + "".join(cases) + "</testsuite></testsuites>"


class RequiredTestTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.required = self.root / "required.txt"
        self.report = self.root / "junit.xml"
        self.report.write_text(REPORT.format(failures=0, failure=""))
        self.executed = self.root / "executed.txt"
        self.evidence_fixture_number = 0

    def run_check(self, *reports):
        return subprocess.run(
            [sys.executable, str(SCRIPT), "--required", str(self.required),
             "--executed-out", str(self.executed), *map(str, reports or (self.report,))],
            text=True, capture_output=True, check=False)

    def make_evidence_checkout(self):
        self.evidence_fixture_number += 1
        repo = self.root / f"candidate-{self.evidence_fixture_number}"
        server = repo / "server"
        required = server / ".config/required-cluster-tests.txt"
        diagnostics = server / ".ci/nextest-archive"
        reports = [
            diagnostics / f"shard-{partition}/junit-{selection}.xml"
            for partition in range(1, 5)
            for selection in ("whole", "shared")
        ]
        evidence = diagnostics / "qualification-evidence.json"
        required_tests = sorted({
            anchor
            for anchors in PREREQUISITE_ANCHORS.values()
            for anchor in anchors
        } | {"waddle-server::clustering_cluster_e2e required_non_anchor"})
        required.parent.mkdir(parents=True)
        required.write_text("\n".join(required_tests) + "\n")
        (server / "README.md").write_text("candidate fixture\n")

        subprocess.run(["git", "-C", str(repo), "init", "--quiet"], check=True)
        subprocess.run(["git", "-C", str(repo), "config", "user.name", "Fixture"], check=True)
        subprocess.run(["git", "-C", str(repo), "config", "user.email", "fixture@example.invalid"], check=True)
        subprocess.run(["git", "-C", str(repo), "config", "commit.gpgsign", "false"], check=True)
        subprocess.run(["git", "-C", str(repo), "add", "server/.config/required-cluster-tests.txt",
                        "server/README.md"], check=True)
        subprocess.run(["git", "-C", str(repo), "commit", "--quiet", "-m", "fixture"], check=True)
        candidate_sha = subprocess.run(
            ["git", "-C", str(repo), "rev-parse", "HEAD"],
            text=True, capture_output=True, check=True,
        ).stdout.strip()

        for report_index, report in enumerate(reports):
            report.parent.mkdir(parents=True, exist_ok=True)
            report_cases = (
                required_tests if report_index == 0 else
                ["waddle-server::clustering_cluster_e2e optional_observation"] if report_index == 1 else
                []
            )
            report.write_text(junit_cases(report_cases))
        return {
            "repo": repo,
            "server": server,
            "required": required,
            "reports": reports,
            "evidence": evidence,
            "candidate_sha": candidate_sha,
            "required_tests": required_tests,
        }

    def run_evidence_check(self, fixture, *, candidate_sha=None, reports=None, include_sha=True):
        report_paths = reports if reports is not None else fixture["reports"]
        arguments = [
            sys.executable, str(SCRIPT),
            "--required", ".config/required-cluster-tests.txt",
            "--executed-out", ".ci/nextest-archive/executed-tests.txt",
            "--evidence-out", ".ci/nextest-archive/qualification-evidence.json",
        ]
        if include_sha:
            arguments.extend(["--candidate-sha", candidate_sha or fixture["candidate_sha"]])
        arguments.extend(str(path.relative_to(fixture["server"])) for path in report_paths)
        return subprocess.run(
            arguments,
            cwd=fixture["server"],
            text=True,
            capture_output=True,
            check=False,
        )

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

    def test_failure_log_lists_executed_names_of_the_affected_binary(self):
        self.required.write_text(
            "waddle-server::clustering_cluster_e2e lone_survivor_renamed\n")
        result = self.run_check()
        self.assertEqual(result.returncode, 1)
        self.assertIn("executed in waddle-server::clustering_cluster_e2e: 2 tests", result.stderr)
        self.assertIn("  waddle-server::clustering_cluster_e2e lone_survivor_and_isolation_fencing",
                      result.stderr)
        self.assertNotIn("ingress::tests::postgres_case", result.stderr,
                         "binaries without a missing test are not dumped")

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
        self.report.write_text("<not-junit/>")
        self.assertEqual(self.run_check().returncode, 2)

    def test_committed_list_parses(self):
        result = subprocess.run(
            [sys.executable, str(SCRIPT), "--required", str(REQUIRED_LIST), str(self.report)],
            text=True, capture_output=True, check=False)
        # The fixture report lacks most required tests; only parsing must succeed.
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("required test did not run", result.stderr)

    def test_evidence_sidecar_is_deterministic_and_binds_required_and_executed_inventory(self):
        fixture = self.make_evidence_checkout()
        result = self.run_evidence_check(fixture)
        self.assertEqual(result.returncode, 0, result.stderr)
        first = fixture["evidence"].read_bytes()
        record = json.loads(first)
        self.assertEqual(record["lane_id"], LANE_ID)
        self.assertEqual(record["candidate"], {
            "commit_sha": fixture["candidate_sha"], "worktree": "clean"})
        self.assertEqual(record["qualification"], "not_assessed")
        self.assertEqual(record["required_test_gate"]["outcome"], "passed")
        self.assertEqual(record["required_test_gate"]["required_count"], len(fixture["required_tests"]))
        self.assertEqual(record["required_test_gate"]["executed_count"], len(fixture["required_tests"]) + 1)
        self.assertEqual(record["required_test_gate"]["required_tests"], [
            {"test": test, "outcome": "passed"} for test in fixture["required_tests"]
        ])
        self.assertIn("waddle-server::clustering_cluster_e2e optional_observation",
                      record["required_test_gate"]["executed_inventory"])
        self.assertEqual({name: value["outcome"] for name, value in record["prerequisite_anchors"].items()}, {
            "postgres": "passed",
            "multiprocess": "passed",
            "cross_node_resume_websocket": "passed",
            "relay_websocket": "passed",
        })
        self.assertEqual(record["unverified"], {
            "image": "unverified",
            "topology": "unverified",
            "resource_enforcement": "unverified",
            "workload": "unverified",
            "faults": "unverified",
            "deployment": "unverified",
        })
        self.assertEqual(
            record["inputs"]["required_tests"]["sha256"],
            hashlib.sha256(fixture["required"].read_bytes()).hexdigest(),
        )
        self.assertEqual(
            {entry["path"]: entry["sha256"] for entry in record["inputs"]["junit_reports"]},
            {
                str(path.relative_to(fixture["repo"])): hashlib.sha256(path.read_bytes()).hexdigest()
                for path in fixture["reports"]
            },
        )

        fixture["evidence"].unlink()
        repeated = self.run_evidence_check(fixture)
        self.assertEqual(repeated.returncode, 0, repeated.stderr)
        self.assertEqual(fixture["evidence"].read_bytes(), first)

    def test_evidence_sidecar_rejects_missing_or_invalid_provenance_without_output(self):
        fixture = self.make_evidence_checkout()
        mismatch = self.run_evidence_check(fixture, candidate_sha="0" * 40)
        self.assertEqual(mismatch.returncode, 2)
        self.assertIn("candidate SHA mismatch", mismatch.stderr)
        self.assertFalse(fixture["evidence"].exists())

        dirty = self.make_evidence_checkout()
        (dirty["server"] / "README.md").write_text("changed after candidate commit\n")
        result = self.run_evidence_check(dirty)
        self.assertEqual(result.returncode, 2)
        self.assertIn("candidate worktree is dirty", result.stderr)
        self.assertFalse(dirty["evidence"].exists())

        untracked = self.make_evidence_checkout()
        (untracked["server"] / "untracked-source.rs").write_text("candidate source\n")
        result = self.run_evidence_check(untracked)
        self.assertEqual(result.returncode, 2)
        self.assertIn("untracked files outside", result.stderr)
        self.assertFalse(untracked["evidence"].exists())

        staged = self.make_evidence_checkout()
        readme = staged["server"] / "README.md"
        readme.write_text("staged candidate source\n")
        subprocess.run(["git", "-C", str(staged["repo"]), "add", "server/README.md"], check=True)
        readme.write_text("candidate fixture\n")
        result = self.run_evidence_check(staged)
        self.assertEqual(result.returncode, 2)
        self.assertIn("candidate index is dirty", result.stderr)
        self.assertFalse(staged["evidence"].exists())

        missing_sha = self.make_evidence_checkout()
        result = self.run_evidence_check(missing_sha, include_sha=False)
        self.assertEqual(result.returncode, 2)
        self.assertIn("--candidate-sha is required", result.stderr)
        self.assertFalse(missing_sha["evidence"].exists())

    def test_evidence_sidecar_fails_closed_for_missing_malformed_failed_and_skipped_tests(self):
        missing_report = self.make_evidence_checkout()
        missing_report["reports"][-1].unlink()
        result = self.run_evidence_check(missing_report)
        self.assertEqual(result.returncode, 2)
        self.assertFalse(missing_report["evidence"].exists())

        malformed = self.make_evidence_checkout()
        malformed["reports"][0].write_text("<testsuites><")
        result = self.run_evidence_check(malformed)
        self.assertEqual(result.returncode, 2)
        self.assertFalse(malformed["evidence"].exists())

        missing_required = self.make_evidence_checkout()
        report = missing_required["reports"][0]
        report.write_text(junit_cases([
            test for test in missing_required["required_tests"]
            if test != "waddle-server::clustering_cluster_e2e required_non_anchor"
        ]))
        result = self.run_evidence_check(missing_required)
        self.assertEqual(result.returncode, 1)
        self.assertIn("required test did not run", result.stderr)
        self.assertFalse(missing_required["evidence"].exists())

        failed = self.make_evidence_checkout()
        failed["reports"][0].write_text(junit_cases(
            failed["required_tests"],
            failures={"waddle-server::clustering_cluster_e2e required_non_anchor"},
        ))
        result = self.run_evidence_check(failed)
        self.assertEqual(result.returncode, 1)
        self.assertIn("required test failed", result.stderr)
        self.assertFalse(failed["evidence"].exists())

        skipped = self.make_evidence_checkout()
        skipped["reports"][0].write_text(junit_cases(
            skipped["required_tests"],
            skipped={"waddle-server::clustering_cluster_e2e required_non_anchor"},
        ))
        result = self.run_evidence_check(skipped)
        self.assertEqual(result.returncode, 1)
        self.assertIn("required test did not run", result.stderr)
        self.assertFalse(skipped["evidence"].exists())

        contradictory = self.make_evidence_checkout()
        required_test = "waddle-server::clustering_cluster_e2e required_non_anchor"
        contradictory["reports"][1].write_text(junit_cases([required_test], skipped={required_test}))
        result = self.run_evidence_check(contradictory)
        self.assertEqual(result.returncode, 1)
        self.assertIn("required test did not run", result.stderr)
        self.assertFalse(contradictory["evidence"].exists())

    def test_evidence_sidecar_requires_each_explicit_prerequisite_inventory_anchor(self):
        for anchor_ids in PREREQUISITE_ANCHORS.values():
            for anchor in anchor_ids:
                fixture = self.make_evidence_checkout()
                lines = fixture["required"].read_text().splitlines()
                fixture["required"].write_text("\n".join(line for line in lines if line != anchor) + "\n")
                subprocess.run(["git", "-C", str(fixture["repo"]), "add",
                                "server/.config/required-cluster-tests.txt"], check=True)
                subprocess.run(["git", "-C", str(fixture["repo"]), "commit", "--quiet", "-m",
                                "remove prerequisite anchor"], check=True)
                fixture["candidate_sha"] = subprocess.run(
                    ["git", "-C", str(fixture["repo"]), "rev-parse", "HEAD"],
                    text=True, capture_output=True, check=True,
                ).stdout.strip()
                result = self.run_evidence_check(fixture)
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertIn("prerequisite anchor", result.stderr)
                self.assertFalse(fixture["evidence"].exists())


if __name__ == "__main__":
    unittest.main()
