#!/usr/bin/env python3
"""Proof conditions for the changed-crate Namespace benchmark."""

import json
import os
import tempfile
from argparse import Namespace
from contextlib import redirect_stdout
from io import StringIO
from unittest.mock import patch
import unittest
from importlib import import_module
from pathlib import Path

suite = import_module("ci-nix-test-volume")


def measurements():
    rows = {}
    for phase in suite.PHASES:
        edited = phase != "seed"
        layer_hit = phase == "delayed"
        rows[phase] = {
            "revision": "abc", "runner": phase,
            "source_before": "base", "source_after": "changed" if edited else "base",
            "tests": {"inventory_hash": "full", "skipped": 10}, "members": {"built": ["crates/waddle-server"]},
            "start": {
                name: {"path": name, "present": layer_hit}
                for name in ("dependencies", "base", "xmpp")
            },
            "plan": {"built": ["waddle-server-default-test-nextest-0.1.0.drv"]},
            "compiles_by_derivation": {},
            "unedited_lane": {"path": "base"},
        }
        rows[phase]["start"]["lane"] = {"path": "changed" if edited else "base", "present": False}
    return rows


class SuiteTests(unittest.TestCase):
    def test_late_replication_is_measurable(self):
        self.assertEqual(suite.validate(measurements()), [])

    def test_rejects_incorrect_attribution(self):
        for key, value in {
            "runner": "seed",
            "source_after": "another edit",
            "tests": {"inventory_hash": "different", "skipped": 10},
            "members": {"built": ["crates/waddle-server", "crates/waddle-xmpp"]},
            "plan": {"built": ["waddle-server-default-test-members-base-0.1.0.drv"]},
            "compiles_by_derivation": {"waddle-server-default-test-members-xmpp": ["waddle-xmpp"]},
        }.items():
            with self.subTest(key=key):
                data = measurements()
                data["delayed"][key] = value
                self.assertTrue(suite.validate(data))
        for field, value in [("path", "unrelated output"), ("present", False)]:
            data = measurements()
            data["delayed"]["start"]["base"][field] = value
            self.assertTrue(suite.validate(data))

    def test_artifact_transfer_and_job_wall(self):
        data = measurements()
        data["warm"] = data.pop("delayed")
        del data["immediate"]
        jobs = []
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for phase, row in data.items():
                row["runner"] = phase
                row["build_seconds"] = 200 if phase == "warm" else 1200
                row["members"]["fresh"] = ["other"]
                row["tests"].update(ran=100, failed=[], flaky=[])
                for layer, state in row["start"].items():
                    name = state["path"]
                    if layer == "lane":
                        name = "waddle-server-default-test-nextest-0.1.0"
                    state["path"] = "/nix/store/" + ("b" if phase == "seed" and layer == "lane" else "a") * 32 + "-" + name
                row["unedited_lane"]["path"] = "/nix/store/" + "b" * 32 + "-waddle-server-default-test-nextest-0.1.0"
                folder = root / f"nix-test-artifact-{phase}"
                folder.mkdir()
                (folder / "suite.json").write_text(json.dumps(row))
                steps = [{"name": n, "started_at": "2026-11-26T00:01:00Z", "completed_at": "2026-11-26T00:02:00Z"}
                         for n in ("Export member layer closure", "Upload member layer artifact", "Download member layer artifact",
                                   "Verify and import member layer", "Build and run the production default-feature test suite")]
                jobs.append({"name": f"nix-test-artifact/{phase}", "runner_name": phase, "conclusion": "success", "steps": steps,
                             "started_at": "2026-11-26T00:00:00Z", "completed_at": f"2026-11-26T00:{'08' if phase == 'warm' else '25'}:00Z"})
            args = Namespace(input=tmp, jobs=root / "jobs.json", transport="artifact")
            for success in (True, False):
                if not success:
                    next(j for j in jobs if j["runner_name"] == "warm")["completed_at"] = "2026-11-26T00:30:00Z"
                args.jobs.write_text(json.dumps([{"jobs": jobs}]))
                output = StringIO()
                with patch.dict(os.environ, GITHUB_STEP_SUMMARY=str(root / "summary")), redirect_stdout(output):
                    self.assertEqual(suite.summary(args), 0 if success else 1)
                self.assertIn("Download member layer artifact: 60s", output.getvalue())
                self.assertIn("1020s" if success else "did not beat the cold job", output.getvalue())

    def test_only_additional_executed_test_is_removed_from_comparison(self):
        with tempfile.TemporaryDirectory() as tmp:
            report = Path(tmp) / "junit.xml"
            prefix = '<testsuites><testsuite><testcase classname="waddle-server" name="existing" />'
            suffix = '</testsuite></testsuites>'
            report.write_text(prefix + suffix)
            summary = "Summary [ 20.001s] 1 test run: 1 passed (1 slow), 10 skipped"
            seed = suite.inventory(report, False, summary)
            self.assertEqual(seed["skipped"], 10)
            with self.assertRaisesRegex(ValueError, "counts differ"):
                suite.inventory(report, False, "Summary [ 20.001s] 2 tests run: 2 passed")
            with self.assertRaises(ValueError):
                suite.inventory(report, True, summary)
            edit = f'<testcase classname="waddle-server" name="{suite.PROBE.name}"/>'
            report.write_text(prefix + edit + suffix)
            warm = suite.inventory(report, True, "Summary [ 0.2s] 2 tests run: 2 passed, 10 skipped")
            self.assertEqual(seed["inventory_hash"], warm["inventory_hash"])
            self.assertEqual((seed["ran"], warm["ran"]), (1, 2))


if __name__ == "__main__":
    unittest.main()
