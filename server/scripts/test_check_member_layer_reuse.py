#!/usr/bin/env python3
"""Contract tests for check_member_layer_reuse.py."""

from __future__ import annotations

import contextlib
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import check_member_layer_reuse as reuse  # noqa: E402

ROOT = "/build/source"


def artifact(manifest: str, fresh: bool) -> str:
    return json.dumps(
        {
            "reason": "compiler-artifact",
            "manifest_path": manifest,
            "fresh": fresh,
            "target": {"name": "x", "kind": ["lib"]},
        }
    )


def run(lines: list[str], *allowed: str, report_only: bool = False) -> tuple[int, str, str]:
    with tempfile.TemporaryDirectory() as directory:
        messages = Path(directory) / "messages.jsonl"
        messages.write_text("\n".join(lines) + "\n", encoding="utf-8")
        argv = [str(messages), "--root", ROOT]
        for member in allowed:
            argv += ["--rebuild-allowed", member]
        if report_only:
            argv.append("--report-only")
        stdout, stderr = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            code = reuse.main(argv)
        return code, stdout.getvalue(), stderr.getvalue()


class MemberLayerReuseTest(unittest.TestCase):
    def test_only_allowed_member_rebuilt_passes(self) -> None:
        code, out, _ = run(
            [
                artifact(f"{ROOT}/crates/waddle-xmpp/Cargo.toml", True),
                artifact(f"{ROOT}/crates/waddle-server/Cargo.toml", False),
                artifact("/build/cargo-vendor-dir/tokio-1/Cargo.toml", True),
            ],
            "crates/waddle-server",
        )
        self.assertEqual(code, 0)
        self.assertIn("member_units_built=1 member_units_fresh=1", out)
        self.assertIn("dependency_units_built=0 dependency_units_fresh=1", out)

    def test_rebuilt_layer_member_fails(self) -> None:
        code, _, err = run(
            [
                artifact(f"{ROOT}/crates/waddle-xmpp/Cargo.toml", False),
                artifact(f"{ROOT}/experiments/native-fixture/Cargo.toml", False),
                artifact(f"{ROOT}/crates/waddle-server/Cargo.toml", False),
            ],
            "crates/waddle-server",
        )
        self.assertEqual(code, 1)
        self.assertIn("crates/waddle-xmpp, experiments/native-fixture", err)

    def test_report_only_never_fails(self) -> None:
        code, out, err = run(
            [artifact(f"{ROOT}/crates/waddle-xmpp/Cargo.toml", False)],
            report_only=True,
        )
        self.assertEqual(code, 0)
        self.assertIn("member_units_built=1", out)
        self.assertEqual(err, "")

    def test_dependencies_and_vendor_patches_never_fail(self) -> None:
        code, out, _ = run(
            [
                artifact("/build/cargo-vendor-dir/uniffi-0.29/Cargo.toml", False),
                artifact(f"{ROOT}/vendor/kameo/Cargo.toml", False),
            ]
        )
        self.assertEqual(code, 0)
        self.assertIn("dependency_units_built=2", out)

    def test_non_json_and_other_messages_are_ignored(self) -> None:
        code, _, _ = run(
            [
                "   Compiling waddle-server",
                json.dumps({"reason": "build-finished", "success": True}),
                artifact(f"{ROOT}/crates/waddle-server/Cargo.toml", False),
            ],
            "crates/waddle-server",
        )
        self.assertEqual(code, 0)

    def test_missing_artifacts_is_an_error(self) -> None:
        with self.assertRaises(SystemExit) as raised:
            run([json.dumps({"reason": "build-finished", "success": True})])
        self.assertIn("no compiler-artifact messages", str(raised.exception))

    def test_member_report_covers_the_workspace_and_counts_a_mixed_member_once(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "Cargo.toml").write_text('[workspace]\nmembers = ["a", "b"]\n')
            messages = root / "messages"
            report = root / "report"
            messages.write_text("\n".join(
                artifact(f"{root}/{member}/Cargo.toml", fresh)
                for member, fresh in [("a", False), ("a", True), ("b", True), ("fixture", False)]
            ))
            args = [str(messages), "--root", directory, "--output", str(report), "--report-only"]
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(reuse.main(args), 0)
            self.assertEqual(json.loads(report.read_text()), {"built": ["a"], "fresh": ["b"]})

            messages.write_text(artifact(f"{root}/a/Cargo.toml", True))
            with self.assertRaisesRegex(SystemExit, "missing workspace member artifacts: b"):
                reuse.main(args)


if __name__ == "__main__":
    unittest.main()
