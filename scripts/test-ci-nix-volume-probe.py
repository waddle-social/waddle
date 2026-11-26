#!/usr/bin/env python3
"""Parsing and reporting contracts for ci-nix-volume-probe.py."""

import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("probe", Path(__file__).with_name("ci-nix-volume-probe.py"))
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)


class ProbeTests(unittest.TestCase):
    def test_dry_run_separates_builds_from_fetches(self):
        text = (
            "evaluating...\n"
            "these 2 derivations will be built:\n"
            "  /nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-waddle-server-clippy-0.1.0.drv\n"
            "  /nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-waddle-server-clippy-members-xmpp-0.1.0.drv\n"
            "these 1 paths will be fetched (1.0 MiB download, 2.0 MiB unpacked):\n"
            "  /nix/store/cccccccccccccccccccccccccccccccc-rust-1.98.1\n"
        )
        plan = probe.parse_dry_run(text)
        self.assertEqual(plan["built"], ["waddle-server-clippy-0.1.0.drv", "waddle-server-clippy-members-xmpp-0.1.0.drv"])
        self.assertEqual(plan["fetched"], ["rust-1.98.1"])

    def test_empty_dry_run_builds_nothing(self):
        self.assertEqual(probe.parse_dry_run(""), {"built": [], "fetched": []})

    def test_members_are_grouped_by_derivation_and_exclude_dependencies(self):
        log = (
            "waddle-server-clippy-members-base>     Checking waddle-xmpp-core v0.1.0 (/build/source/crates/waddle-xmpp-core)\n"
            "waddle-server-clippy-members-base>    Compiling serde v1.0.228\n"
            "waddle-server-clippy-members-base>     Checking link-board v0.1.0 (/build/source/extensions/link-board)\n"
            "waddle-server-clippy>     Checking waddle-server v0.1.0 (/build/source/crates/waddle-server)\n"
            "unrelated line\n"
        )
        self.assertEqual(probe.checked_members(log), {
            "waddle-server-clippy": ["waddle-server"],
            "waddle-server-clippy-members-base": ["link-board", "waddle-xmpp-core"],
        })

    def test_render_marks_hits_misses_and_failures(self):
        rows = [{
            "volume": {"phase": "probe-1", "store_paths": 812},
            "probe": {
                "phase": "probe-1",
                "targets": {"deps": {"present": True}, "layer-base": {"present": True}, "layer-xmpp": {"present": False}},
                "plan": {"built": ["a.drv", "b.drv"]},
                "members": {"waddle-server-clippy": ["waddle-server"]},
                "build_seconds": 321.5,
                "build_success": False,
                "sandboxed": True,
            },
        }]
        line = probe.render(rows).splitlines()[-1]
        self.assertEqual(line, "| probe-1 | yes | 812 | hit | hit | miss | 2 | waddle-server | 321.5 | FAILED |")


if __name__ == "__main__":
    unittest.main()
