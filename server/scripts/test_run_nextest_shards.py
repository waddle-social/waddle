import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("run-nextest-shards.sh")
REPORTS = ("whole-inventory.json", "shared-inventory.json", "whole-coverage.json", "shared-coverage.json")


class LocalShardTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.archive = self.root / "archive"
        (self.archive / "ci-performance").mkdir(parents=True)
        for name in ("ci-performance/cargo-timing.html", "partition-coverage.json", "plan.json"):
            (self.archive / name).write_text("baseline evidence")
        (self.root / "scripts").mkdir()
        (self.root / "scripts/build-nextest-archive.sh").write_text(
            'exit_code="${COMPILE_EXIT:-0}"\n'
            'if [ "$exit_code" != 0 ]; then exit "$exit_code"; fi\n'
            'printf "%s\\n" "$TEST_ARCHIVE"\n'
        )
        for partition in range(1, 5):
            output = self.root / f"shard-{partition}"
            output.mkdir()
            for name in REPORTS:
                (output / name).write_text(f"verified shard {partition}")
        nix = self.root / "nix"
        nix.write_text(
            f"#!{sys.executable}\n"
            "import json, os, pathlib, sys\n"
            "root = pathlib.Path(os.environ['TEST_ROOT'])\n"
            "args = sys.argv[1:]\n"
            "if args[0] == 'build' and args[-1] == '../#waddle-ci-sandbox-probe':\n"
            "    (root / 'probe-args.json').write_text(json.dumps(args))\n"
            "    sys.exit(int(os.environ.get('SANDBOX_EXIT', '0')))\n"
            "if args[0] == 'build':\n"
            "    (root / 'build-args.json').write_text(json.dumps(args))\n"
            "    print('[]')\n"
            "    sys.exit(int(os.environ.get('SHARD_EXIT', '0')))\n"
            "if args[:2] == ['eval', '--raw']:\n"
            "    partition = args[2].split('shard-')[1].split('.')[0]\n"
            "    print(root / ('shard-' + partition))\n"
            "else:\n"
            "    sys.exit(99)\n"
        )
        nix.chmod(0o755)
        self.env = dict(os.environ, PATH=f"{self.root}:{os.environ['PATH']}",
                        TEST_ROOT=str(self.root), TEST_ARCHIVE=str(self.archive))

    def run_shards(self):
        return subprocess.run(["bash", str(SCRIPT)], cwd=self.root, env=self.env,
                              text=True, capture_output=True, check=False)

    def test_all_four_isolated_shards_are_required_and_evidence_is_retained(self):
        result = self.run_shards()
        self.assertEqual(result.returncode, 0, result.stderr)
        args = json.loads((self.root / "build-args.json").read_text())
        self.assertEqual(args[-4:], [f"../#waddle-server-test-shard-{i}" for i in range(1, 5)])
        probe_args = json.loads((self.root / "probe-args.json").read_text())
        for setting in ("builders", "sandbox", "sandbox-fallback"):
            self.assertIn(setting, probe_args)
        self.assertIn("--keep-going", args)
        for option, value in (("--max-jobs", "4"), ("--cores", "8")):
            self.assertEqual(args[args.index(option) + 1], value)
        settings = {args[i + 1]: args[i + 2] for i, arg in enumerate(args) if arg == "--option"}
        self.assertEqual(settings, {"builders": "", "sandbox": "true", "sandbox-fallback": "false"})
        for partition in range(1, 5):
            for name in REPORTS:
                report = self.root / f".ci/nextest-archive/shard-{partition}/{name}"
                self.assertEqual(report.read_text(), f"verified shard {partition}")

    def test_failed_shard_fails_the_job_and_preserves_compiler_diagnostics(self):
        self.env["SHARD_EXIT"] = "17"
        result = self.run_shards()
        self.assertEqual(result.returncode, 17)
        self.assertTrue((self.root / ".ci/nextest-archive/cargo-timing.html").is_file())

    def test_unavailable_sandbox_fails_before_compilation(self):
        self.env["SANDBOX_EXIT"] = "23"
        self.env["COMPILE_EXIT"] = "19"
        self.assertEqual(self.run_shards().returncode, 23)
        self.assertFalse((self.root / "build-args.json").exists())
        self.assertFalse((self.root / ".ci/nextest-archive").exists())

    def test_failed_compile_cannot_start_shards(self):
        self.env["COMPILE_EXIT"] = "19"
        self.assertEqual(self.run_shards().returncode, 19)
        self.assertFalse((self.root / "build-args.json").exists())

    def test_missing_shard_coverage_cannot_report_success(self):
        (self.root / "shard-4/shared-coverage.json").unlink()
        self.assertNotEqual(self.run_shards().returncode, 0)


if __name__ == "__main__":
    unittest.main()
