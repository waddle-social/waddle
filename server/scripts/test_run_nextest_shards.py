import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("run-nextest-shards.sh")
REPORTS = ("whole-inventory.json", "shared-inventory.json", "whole-coverage.json", "shared-coverage.json")
JUNIT = ("junit-whole.xml", "junit-shared.xml")
BINARY = "waddle-server::clustering_cluster_e2e"
ANCHORS = (
    "waddle-server::sqlx_cancelled_begin cancelled_ordinary_begin_never_reuses_an_open_postgres_transaction",
    "waddle-server::db_lineage_multiprocess two_replicas_same_database_both_ready",
    "waddle-server::clustering_cluster_e2e cross_node_resume_live_steal_handshake",
    "waddle-server::clustering_cluster_e2e muc_join_routes_to_foreign_room_owner",
    "waddle-server::clustering_cluster_e2e groupchat_fanout_reaches_foreign_node_occupant_and_terminalizes",
)


def junit(test_names):
    cases = []
    for test in test_names:
        binary, name = test.split(" ", 1)
        cases.append(f'<testcase name="{name}" classname="{binary}"/>')
    return "<testsuites><testsuite>" + "".join(cases) + "</testsuite></testsuites>"


class LocalShardTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.server = self.root / "server"
        self.server.mkdir()
        self.archive = self.server / ".ci/nextest-archive/mock-archive"
        (self.archive / "ci-performance").mkdir(parents=True)
        for name in ("ci-performance/cargo-timing.html", "partition-coverage.json", "plan.json"):
            (self.archive / name).write_text("baseline evidence")
        (self.server / "scripts").mkdir()
        (self.server / "scripts/build-nextest-archive.sh").write_text(
            'exit_code="${COMPILE_EXIT:-0}"\n'
            'if [ "$exit_code" != 0 ]; then exit "$exit_code"; fi\n'
            'printf "%s\\n" "$TEST_ARCHIVE"\n'
        )
        (self.server / ".config").mkdir()
        self.required = self.server / ".config/required-cluster-tests.txt"
        self.required.write_text("\n".join([*ANCHORS, f"{BINARY} whole_1", f"{BINARY} shared_4"]) + "\n")
        (self.server / ".ci/nextest-archive/mock-shards").mkdir(parents=True)
        for partition in range(1, 5):
            output = self.server / f".ci/nextest-archive/mock-shards/shard-{partition}"
            output.mkdir()
            for name in REPORTS:
                (output / name).write_text(f"verified shard {partition}")
            for name in JUNIT:
                selection = name[6:-4]
                (output / name).write_text(
                    junit([*ANCHORS, f"{BINARY} {selection}_{partition}"])
                )
        nix = self.server / "scripts/nix"
        nix.write_text(
            f"#!{sys.executable}\n"
            "import json, os, pathlib, sys\n"
            "root = pathlib.Path(os.environ['TEST_ROOT'])\n"
            "args = sys.argv[1:]\n"
            "if args[0] == 'build' and args[-1] == '../#waddle-ci-sandbox-probe':\n"
            "    (root / '.ci/nextest-archive/probe-args.json').write_text(json.dumps(args))\n"
            "    sys.exit(int(os.environ.get('SANDBOX_EXIT', '0')))\n"
            "if args[0] == 'build':\n"
            "    (root / '.ci/nextest-archive/build-args.json').write_text(json.dumps(args))\n"
            "    print('[]')\n"
            "    sys.exit(int(os.environ.get('SHARD_EXIT', '0')))\n"
            "if args[:2] == ['eval', '--raw']:\n"
            "    partition = args[2].split('shard-')[1].split('.')[0]\n"
            "    print(root / '.ci/nextest-archive/mock-shards' / ('shard-' + partition))\n"
            "else:\n"
            "    sys.exit(99)\n"
        )
        nix.chmod(0o755)
        subprocess.run(["git", "-C", str(self.root), "init", "--quiet"], check=True)
        subprocess.run(["git", "-C", str(self.root), "config", "user.name", "Fixture"], check=True)
        subprocess.run(["git", "-C", str(self.root), "config", "user.email", "fixture@example.invalid"], check=True)
        subprocess.run(["git", "-C", str(self.root), "config", "commit.gpgsign", "false"], check=True)
        subprocess.run(["git", "-C", str(self.root), "add", "server/scripts/build-nextest-archive.sh",
                        "server/scripts/nix", "server/.config/required-cluster-tests.txt"], check=True)
        subprocess.run(["git", "-C", str(self.root), "commit", "--quiet", "-m", "fixture"], check=True)
        self.env = dict(os.environ, PATH=f"{self.server / 'scripts'}:{os.environ['PATH']}",
                        TEST_ROOT=str(self.server), TEST_ARCHIVE=str(self.archive))

    def run_shards(self):
        return subprocess.run(["bash", str(SCRIPT)], cwd=self.server, env=self.env,
                              text=True, capture_output=True, check=False)

    def commit_required_list(self):
        subprocess.run(["git", "-C", str(self.root), "add", "server/.config/required-cluster-tests.txt"], check=True)
        subprocess.run(["git", "-C", str(self.root), "commit", "--quiet", "-m", "change fixture inventory"], check=True)

    def test_all_four_isolated_shards_are_required_and_evidence_is_retained(self):
        result = self.run_shards()
        self.assertEqual(result.returncode, 0, result.stderr)
        args = json.loads((self.server / ".ci/nextest-archive/build-args.json").read_text())
        self.assertEqual(args[-4:], [f"../#waddle-server-test-shard-{i}" for i in range(1, 5)])
        probe_args = json.loads((self.server / ".ci/nextest-archive/probe-args.json").read_text())
        for setting in ("builders", "sandbox", "sandbox-fallback"):
            self.assertIn(setting, probe_args)
        self.assertIn("--keep-going", args)
        for option, value in (("--max-jobs", "4"), ("--cores", "8")):
            self.assertEqual(args[args.index(option) + 1], value)
        settings = {args[i + 1]: args[i + 2] for i, arg in enumerate(args) if arg == "--option"}
        self.assertEqual(settings, {"builders": "", "sandbox": "true", "sandbox-fallback": "false"})
        for partition in range(1, 5):
            for name in REPORTS:
                report = self.server / f".ci/nextest-archive/shard-{partition}/{name}"
                self.assertEqual(report.read_text(), f"verified shard {partition}")
        sidecar = self.server / ".ci/nextest-archive/qualification-evidence.json"
        self.assertTrue(sidecar.is_file())
        evidence = json.loads(sidecar.read_text())
        self.assertEqual(evidence["candidate"]["worktree"], "clean")
        self.assertEqual(evidence["qualification"], "not_assessed")

    def test_executed_test_names_from_every_shard_are_emitted(self):
        result = self.run_shards()
        self.assertEqual(result.returncode, 0, result.stderr)
        executed = (self.server / ".ci/nextest-archive/executed-tests.txt").read_text().splitlines()
        self.assertEqual(executed, sorted([*ANCHORS, *(f"{BINARY} {selection}_{partition}"
                                                        for selection in ("whole", "shared")
                                                        for partition in range(1, 5))]))
        for partition in range(1, 5):
            for name in JUNIT:
                self.assertTrue((self.server / f".ci/nextest-archive/shard-{partition}/{name}").is_file())

    def test_required_test_that_did_not_run_fails_the_lane(self):
        self.required.write_text("\n".join([*ANCHORS, f"{BINARY} whole_1",
                                               f"{BINARY} returned_early_or_filtered"]) + "\n")
        self.commit_required_list()
        result = self.run_shards()
        self.assertEqual(result.returncode, 1)
        self.assertIn(f"required test did not run: {BINARY} returned_early_or_filtered", result.stderr)

    def test_missing_shard_junit_report_cannot_report_success(self):
        (self.server / ".ci/nextest-archive/mock-shards/shard-2/junit-shared.xml").unlink()
        self.assertNotEqual(self.run_shards().returncode, 0)

    def test_failed_shard_fails_the_job_and_preserves_compiler_diagnostics(self):
        self.env["SHARD_EXIT"] = "17"
        result = self.run_shards()
        self.assertEqual(result.returncode, 17)
        self.assertTrue((self.server / ".ci/nextest-archive/cargo-timing.html").is_file())
        self.assertFalse((self.server / ".ci/nextest-archive/qualification-evidence.json").exists())

    def test_unavailable_sandbox_fails_before_compilation(self):
        self.env["SANDBOX_EXIT"] = "23"
        self.env["COMPILE_EXIT"] = "19"
        self.assertEqual(self.run_shards().returncode, 23)
        self.assertFalse((self.server / ".ci/nextest-archive/build-args.json").exists())
        self.assertFalse((self.server / ".ci/nextest-archive/qualification-evidence.json").exists())

    def test_failed_compile_cannot_start_shards(self):
        self.env["COMPILE_EXIT"] = "19"
        self.assertEqual(self.run_shards().returncode, 19)
        self.assertFalse((self.server / ".ci/nextest-archive/build-args.json").exists())

    def test_missing_shard_coverage_cannot_report_success(self):
        (self.server / ".ci/nextest-archive/mock-shards/shard-4/shared-coverage.json").unlink()
        self.assertNotEqual(self.run_shards().returncode, 0)


if __name__ == "__main__":
    unittest.main()
