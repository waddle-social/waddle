"""Timing-contract regressions: python3 tests/ci-timings-test.py."""

from datetime import datetime, timedelta, timezone
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest


SPEC = importlib.util.spec_from_file_location("ci_timings", Path(__file__).parents[1] / "scripts/ci-timings.py")
CI = importlib.util.module_from_spec(SPEC)
sys.dont_write_bytecode = True
SPEC.loader.exec_module(CI)
SHA = "a" * 40


def at(seconds):
    return (datetime(2026, 9, 20, tzinfo=timezone.utc) + timedelta(seconds=seconds)).isoformat()


def run(identifier=1, name="CI", start=0, attempt=1, status="completed", conclusion="success"):
    return {"id": identifier, "head_sha": SHA, "name": name, "path": f".github/workflows/{name}.yml", "created_at": at(0), "run_started_at": at(start), "run_attempt": attempt, "status": status, "conclusion": conclusion}


def job(identifier=10, run_id=1, name="Build", start=0, end=600, attempt=1, status="completed", conclusion="success"):
    return {"id": identifier, "run_id": run_id, "run_attempt": attempt, "name": name, "started_at": at(start), "completed_at": at(end) if end is not None else None, "status": status, "conclusion": conclusion}


class TimingContractTests(unittest.TestCase):
    def test_last_finisher_includes_codeql_and_workflow_start_delay(self):
        codeql = {**run(2, "CodeQL"), "created_at": at(60)}
        report = CI.summarize([run(), codeql], [job(end=850), job(20, 2, "Rust", start=600, end=950)], SHA)
        self.assertTrue(report["successful"])
        self.assertEqual(report["event_to_last_job_seconds"], 950)
        self.assertFalse(report["under_budget"])
        self.assertEqual(report["critical_finish"]["workflow"], "CodeQL")

    def test_retry_does_not_reset_original_event_latency(self):
        report = CI.summarize(
            [run(conclusion="failure"), run(start=3600, attempt=2)],
            [job(end=600, conclusion="failure"), job(11, start=3610, end=3700, attempt=2)], SHA,
        )
        self.assertTrue(report["successful"])
        self.assertEqual(report["event_to_last_job_seconds"], 3700)
        self.assertEqual(report["workflows"][0]["attempt_seconds"], 100)
        self.assertFalse(report["under_budget"])
        self.assertFalse(report["first_attempt_successful"])
        self.assertEqual(len(report["workflows"][0]["jobs"]), 1)

    def test_retry_without_current_attempt_jobs_is_unproven(self):
        report = CI.summarize([run(attempt=2, start=3600)], [job()], SHA)
        self.assertFalse(report["measurement_complete"])
        self.assertIsNone(report["under_budget"])

    def test_failure_and_cancellation_never_count_as_fast_success(self):
        for conclusion in ("failure", "cancelled", "timed_out", "action_required"):
            with self.subTest(conclusion=conclusion):
                report = CI.summarize([run(conclusion=conclusion)], [job(conclusion=conclusion, end=100)], SHA)
                self.assertFalse(report["successful"])
                self.assertIsNone(report["under_budget"])

    def test_in_progress_and_missing_workflow_jobs_are_incomplete(self):
        cases = [
            ([run(status="in_progress", conclusion=None)], [job(end=None, status="in_progress", conclusion=None)]),
            ([run(), run(2, "CodeQL")], [job()]),
        ]
        for runs, jobs in cases:
            with self.subTest(runs=runs):
                report = CI.summarize(runs, jobs, SHA)
                self.assertFalse(report["measurement_complete"])
                self.assertIsNone(report["event_to_last_job_seconds"])
                self.assertIsNone(report["critical_finish"])

    def test_wait_is_unknown_without_graph_and_excludes_dependency_runtime(self):
        jobs = [job(start=10, end=100), job(11, name="Test", start=130, end=200)]
        report = CI.summarize([run()], jobs, SHA)
        self.assertTrue(all(item["eligible_wait_seconds"] is None for item in report["workflows"][0]["jobs"]))
        graph = {".github/workflows/CI.yml": {"Build": [], "Test": ["Build"]}}
        report = CI.summarize([run()], jobs, SHA, dependencies=graph)
        self.assertEqual([item["eligible_wait_seconds"] for item in report["workflows"][0]["jobs"]], [10, 30])

    def test_ambiguous_or_missing_dependency_cannot_claim_queue_delay(self):
        jobs = [job(), job(11), job(12, name="Test", start=700, end=800)]
        graph = {".github/workflows/CI.yml": {"Test": ["Build"]}}
        report = CI.summarize([run()], jobs, SHA, dependencies=graph)
        self.assertIsNone(report["workflows"][0]["jobs"][2]["eligible_wait_seconds"])

    def test_supplied_event_timestamp_includes_delivery_delay(self):
        report = CI.summarize([run()], [job()], SHA, event_time=at(-60))
        self.assertEqual(report["event_to_last_job_seconds"], 660)
        with self.assertRaises(ValueError):
            CI.summarize([run()], [job()], SHA, event_time=at(1))

    def test_timezone_offsets_do_not_reorder_critical_finish(self):
        earlier = {**job(), "completed_at": "2026-09-20T01:10:00+01:00"}
        report = CI.summarize([run(), run(2, "CodeQL")], [earlier, job(20, 2, end=700)], SHA)
        self.assertEqual(report["critical_finish"]["workflow"], "CodeQL")

    def test_negative_job_duration_is_incomplete(self):
        report = CI.summarize([run()], [job(start=100, end=50)], SHA)
        self.assertFalse(report["measurement_complete"])
        self.assertIsNone(report["under_budget"])

    def test_partial_api_page_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "jobs.json"
            path.write_text(json.dumps({"total_count": 2, "jobs": [job()]}))
            with self.assertRaisesRegex(ValueError, "incomplete jobs"):
                CI.read_files([path], "jobs")

    def test_attempt_wrapper_carries_provenance_to_raw_jobs(self):
        document = {"run_id": 1, "run_attempt": 2, "jobs": [{"id": 11}]}
        self.assertEqual(CI.records(document, "jobs"), [{"id": 11, "run_id": 1, "run_attempt": 2}])


if __name__ == "__main__":
    unittest.main()
