#!/usr/bin/env python3
"""Proof conditions for the changed-crate Namespace benchmark."""

import tempfile
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
            "tests": {"inventory_hash": "full"}, "members": {"built": ["crates/waddle-server"]},
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
            "tests": {"inventory_hash": "different"},
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

    def test_only_additional_executed_test_is_removed_from_comparison(self):
        with tempfile.TemporaryDirectory() as tmp:
            report = Path(tmp) / "junit.xml"
            prefix = '<testsuites><testsuite><testcase classname="waddle-server" name="existing" />'
            suffix = '</testsuite></testsuites>'
            report.write_text(prefix + suffix)
            seed = suite.inventory(report, False)
            with self.assertRaises(ValueError):
                suite.inventory(report, True)
            edit = f'<testcase classname="waddle-server" name="{suite.PROBE.name}"/>'
            report.write_text(prefix + edit + suffix)
            warm = suite.inventory(report, True)
            self.assertEqual(seed["inventory_hash"], warm["inventory_hash"])
            self.assertEqual((seed["ran"], warm["ran"]), (1, 2))


if __name__ == "__main__":
    unittest.main()
