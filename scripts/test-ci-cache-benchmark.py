#!/usr/bin/env python3
"""Cache miss and provider-isolation contracts for the CI experiment."""

import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import types
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location("benchmark", Path(__file__).with_name("ci-cache-benchmark.py"))
benchmark = importlib.util.module_from_spec(spec)
spec.loader.exec_module(benchmark)


class CacheBenchmarkTests(unittest.TestCase):
    def test_excludes_unselected_providers(self):
        configured = ["https://cache.nixos.org", "https://cache.flakehub.com",
                      "http://127.0.0.1:37516?trusted=true", "http://127.0.0.1:37515?trusted=true",
                      "https://unrelated.example/cache"]
        self.assertEqual(benchmark.provider_urls("h3", configured),
                         ["https://cache.nixos.org", "http://127.0.0.1:37516?trusted=true"])
        self.assertEqual(benchmark.provider_urls("fh", configured),
                         ["https://cache.nixos.org", "https://cache.flakehub.com"])
        self.assertEqual(benchmark.provider_urls("namespace", configured), ["https://cache.nixos.org"])

    def test_missing_provider_fails_instead_of_falling_back(self):
        with self.assertRaisesRegex(ValueError, "required fh"):
            benchmark.provider_urls("fh", ["https://cache.nixos.org"])

    def test_flakehub_edge_is_allowed_without_allowing_arbitrary_hosts(self):
        edge = "https://edge.cache.flakehub.com?priority=10"
        unrelated = ["https://other.cache.flakehub.com", "https://edge.cache.flakehub.com.example.org",
                     "http://edge.cache.flakehub.com", "https://unrelated.example/cache"]
        self.assertEqual(benchmark.provider_urls("fh", [edge, *unrelated]),
                         ["https://cache.nixos.org", edge])
        self.assertEqual(benchmark.provider_urls("namespace", [edge]), ["https://cache.nixos.org"])
        with self.assertRaisesRegex(ValueError, "required fh"):
            benchmark.provider_urls("fh", unrelated)

    def test_credential_urls_never_enter_commands_or_reports(self):
        with self.assertRaisesRegex(ValueError, "credential-bearing"):
            benchmark.provider_urls("fh", ["https://secret:password@cache.flakehub.com"])

    def test_cache_miss_cannot_be_success_or_trigger_build(self):
        path = "/nix/store/" + "a" * 32 + "-dependency"
        build_commands = []

        def fake_command(args, **_kwargs):
            if args[:3] == ["git", "rev-parse", "HEAD"]:
                return subprocess.CompletedProcess(args, 0, "abc\n", "")
            if "config" in args:
                return subprocess.CompletedProcess(args, 0, "https://cache.nixos.org https://cache.flakehub.com\n", "")
            if "eval" in args:
                return subprocess.CompletedProcess(args, 0, path, "")
            if "--check-validity" in args:
                return subprocess.CompletedProcess(args, 1, "", "missing")
            if "--realise" in args:
                build_commands.append(args)
                self.assertEqual(args[args.index("--max-jobs") + 1], "0")
                self.assertEqual(args[args.index("--builders") + 1], "")
                return subprocess.CompletedProcess(args, 1, "", "cannot build with max-jobs=0")
            raise AssertionError(args)

        with tempfile.TemporaryDirectory() as output:
            args = types.SimpleNamespace(output=output, provider="fh", variant="fh", phase="existing")
            with patch.object(benchmark, "command", side_effect=fake_command), patch("builtins.print"):
                self.assertEqual(benchmark.restore(args), 2)
            report = json.loads((Path(output) / "result.json").read_text())
            self.assertFalse(report["complete"])
            self.assertEqual(len(build_commands), 3)
            self.assertTrue(all(p["outcome"] == "miss-or-error" for p in report["paths"]))

    def test_summary_rejects_successful_probe_with_failed_cache_post(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            benchmark.write_json(root / "input" / "result.json", {
                "phase": "existing", "variant": "fh", "complete": True})
            jobs = {"total_count": 2, "jobs": [
                {"name": "cache-benchmark/existing/fh", "id": 1, "conclusion": "failure", "steps": [],
                 "started_at": "2026-09-20T12:00:00Z", "completed_at": "2026-09-20T12:02:00Z"},
                {"name": "cache-benchmark/existing/h2", "id": 2, "conclusion": "success", "steps": [],
                 "started_at": "2026-09-20T12:02:00Z", "completed_at": "2026-09-20T12:02:05Z"}]}
            args = types.SimpleNamespace(input=str(root / "input"), output=str(root / "output"))
            with patch.object(benchmark, "api_json", return_value=jobs), patch.dict(benchmark.os.environ, {"GITHUB_RUN_ID": "1"}), patch("builtins.print"):
                benchmark.summarize(args)
            rows = json.loads((root / "output" / "summary.json").read_text())["rows"]
            self.assertFalse(rows[0]["complete"])
            self.assertFalse(rows[1]["complete"])
            self.assertFalse(rows[0]["eligible_for_cache_selection"])
            self.assertIn("unverified", rows[0]["cache_post_status"])
            self.assertEqual(rows[0]["job_seconds_including_cache_post"], 120)

    def test_hestia_seed_requires_acknowledgement_and_committed_manifest(self):
        scenarios = [
            ("hestia hook: daemon did not accept the paths\n", 0, "", 1),
            ("hestia hook: registered 3 path(s), 3 buffered for upload\n", 1, "timed out\n", 1),
            ("hestia hook: registered 3 path(s), 3 buffered for upload\n", 0, "no stats\n", 1),
            ("hestia hook: registered 3 path(s), 3 buffered for upload\n", 0,
             "hestia drain: pushed 3 paths; manifest m3#7\n", 0),
        ]
        for hook_log, drain_code, drain_log, expected_code in scenarios:
            with self.subTest(hook=hook_log, drain=drain_log), tempfile.TemporaryDirectory() as output:
                paths = [{"name": name, "path": "/nix/store/" + chr(97 + i) * 32 + "-fixture", "success": True}
                         for i, name in enumerate([*benchmark.TARGETS, "trial-shard1-archive"])]
                benchmark.write_json(Path(output) / "result.json", {"complete": True, "paths": paths})
                calls = []

                def fake_command(args, **kwargs):
                    calls.append(args)
                    if args[1] == "hook":
                        self.assertEqual(kwargs["timeout"], 15)
                        return subprocess.CompletedProcess(args, 0, "", hook_log)
                    self.assertEqual(args[1], "drain")
                    self.assertEqual(args[-2:], ["--timeout", "300"])
                    self.assertEqual(kwargs["timeout"], 310)
                    return subprocess.CompletedProcess(args, drain_code, "", drain_log)

                with patch.object(benchmark, "command", side_effect=fake_command), \
                        patch.dict(benchmark.os.environ, {"HESTIA_BIN": "hestia", "HESTIA_SOCKET": "/tmp/hook.sock",
                                                        "GITHUB_OUTPUT": str(Path(output) / "outputs")}), \
                        patch("builtins.print"):
                    self.assertEqual(benchmark.seed_hestia(types.SimpleNamespace(output=output)), expected_code)
                result = json.loads((Path(output) / "hestia-seed.json").read_text())
                self.assertEqual(result["success"], expected_code == 0)
                if expected_code == 0:
                    self.assertEqual(result["manifest_version"], 7)
                    self.assertEqual((Path(output) / "outputs").read_text(), "manifest-version=7\n")
                    self.assertIn("unverified", result["cache_save"])
                elif "did not accept" in hook_log:
                    self.assertEqual(len(calls), 1)

    def test_hestia_seed_refuses_incomplete_restore(self):
        with tempfile.TemporaryDirectory() as output:
            benchmark.write_json(Path(output) / "result.json", {"complete": False, "paths": []})
            with patch.object(benchmark, "command") as run, patch("builtins.print"):
                self.assertEqual(benchmark.seed_hestia(types.SimpleNamespace(output=output)), 1)
                run.assert_not_called()

    def test_gate_waits_for_active_ci_and_a_quiet_interval(self):
        clock = [0]
        calls = []

        def collection(_endpoint, _key):
            calls.append(clock[0])
            return [{"id": 2, "path": ".github/workflows/waddle-server-rusttests.yml",
                     "name": "normal CI", "status": "in_progress" if len(calls) == 1 else "completed"}]

        with patch.object(benchmark, "api_collection", side_effect=collection), \
                patch.object(benchmark.time, "monotonic", side_effect=lambda: clock[0]), \
                patch.object(benchmark.time, "sleep", side_effect=lambda seconds: clock.__setitem__(0, clock[0] + seconds)), \
                patch.dict(benchmark.os.environ, {"GITHUB_RUN_ID": "1"}), patch("builtins.print"):
            self.assertEqual(benchmark.wait_for_ci(types.SimpleNamespace(sha="a" * 40, timeout=100)), 0)
        self.assertEqual(calls, [0, 20, 40, 60, 80])


if __name__ == "__main__":
    unittest.main()
