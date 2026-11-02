import json
import os
from pathlib import Path
import shutil
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
            'printf "%s\\n%s\\n" "$PWD" "$1" > "$TEST_ROOT/.ci/nextest-archive/helper-source"\n'
            'exit_code="${COMPILE_EXIT:-0}"\n'
            'if [ "$exit_code" != 0 ]; then exit "$exit_code"; fi\n'
            'printf "%s\\n" "$TEST_ARCHIVE"\n'
        )
        shutil.copyfile(SCRIPT.with_name("check_required_tests.py"),
                        self.server / "scripts/check_required_tests.py")
        (self.root / "flake.nix").write_text('committed source $Format:%H$\n')
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
            "import json, os, pathlib, shutil, sys\n"
            "root = pathlib.Path(os.environ['TEST_ROOT'])\n"
            "args = sys.argv[1:]\n"
            "if args[:2] == ['store', 'add-path']:\n"
            "    source = root / '.ci/nextest-archive/mock-store-source'\n"
            "    if source.exists(): shutil.rmtree(source)\n"
            "    shutil.copytree(args[-1], source, symlinks=True)\n"
            "    if os.environ.get('TAMPER_SOURCE'):\n"
            "        (source / 'flake.nix').write_text('tampered store source\\n')\n"
            "    print(source)\n"
            "    sys.exit(0)\n"
            "ref = next((arg for arg in args if arg.startswith('path:') and '#' in arg), None)\n"
            "if ref is not None:\n"
            "    source = pathlib.Path(ref[5:].split('#', 1)[0])\n"
            "    if os.environ.get('MUTATE_CHECKOUT'):\n"
            "        original = root.parent / 'flake.nix'\n"
            "        content = original.read_text()\n"
            "        try:\n"
            "            original.write_text('dirty source during Nix evaluation\\n')\n"
            "            observed = (source / 'flake.nix').read_text()\n"
            "            with (root / '.ci/nextest-archive/source-observations').open('a') as log:\n"
            "                log.write(json.dumps({'ref': ref, 'content': observed}) + '\\n')\n"
            "        finally:\n"
            "            original.write_text(content)\n"
            "if args[0] == 'build' and args[-1].endswith('#waddle-ci-sandbox-probe'):\n"
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
        subprocess.run(["git", "-C", str(self.root), "add", "flake.nix",
                        "server/scripts/build-nextest-archive.sh", "server/scripts/check_required_tests.py",
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
        source = self.server / ".ci/nextest-archive/mock-store-source"
        flake = f"path:{source}"
        self.assertEqual(args[-4:], [f"{flake}#waddle-server-test-shard-{i}" for i in range(1, 5)])
        self.assertEqual((self.server / ".ci/nextest-archive/helper-source").read_text().splitlines(),
                         [str(source / "server"), f"{flake}#waddle-server-test-archive"])
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

    def test_all_nix_evaluations_use_committed_source_during_checkout_mutations(self):
        self.env["MUTATE_CHECKOUT"] = "1"
        result = self.run_shards()
        self.assertEqual(result.returncode, 0, result.stderr)
        observations = [json.loads(line) for line in
                        (self.server / ".ci/nextest-archive/source-observations").read_text().splitlines()]
        self.assertEqual(len(observations), 6)  # probe, build, and four shard evals
        source = self.server / ".ci/nextest-archive/mock-store-source"
        self.assertTrue(all(item["ref"].startswith(f"path:{source}#") for item in observations))
        self.assertTrue(all(item["content"] == "committed source $Format:%H$\n" for item in observations))
        self.assertTrue((self.server / ".ci/nextest-archive/qualification-evidence.json").exists())

    def test_archive_export_transformations_fail_before_nix_builds(self):
        for attribute in ("export-ignore", "export-subst"):
            with self.subTest(attribute=attribute):
                (self.root / ".gitattributes").write_text(f"flake.nix {attribute}\n")
                subprocess.run(["git", "-C", str(self.root), "add", ".gitattributes"], check=True)
                subprocess.run(["git", "-C", str(self.root), "commit", "--quiet", "-m", attribute], check=True)
                result = self.run_shards()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("candidate source snapshot:", result.stderr)
                self.assertFalse((self.server / ".ci/nextest-archive/probe-args.json").exists())
                self.assertFalse((self.server / ".ci/nextest-archive/qualification-evidence.json").exists())

    def test_gitlinks_and_modified_materialized_source_fail_closed(self):
        self.env["TAMPER_SOURCE"] = "1"
        result = self.run_shards()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("content differs from Git tree: flake.nix", result.stderr)
        self.assertFalse((self.server / ".ci/nextest-archive/probe-args.json").exists())

        del self.env["TAMPER_SOURCE"]
        sha = subprocess.check_output(["git", "-C", str(self.root), "rev-parse", "HEAD"], text=True).strip()
        subprocess.run(["git", "-C", str(self.root), "update-index", "--add", "--cacheinfo",
                        f"160000,{sha},module"], check=True)
        subprocess.run(["git", "-C", str(self.root), "commit", "--quiet", "-m", "gitlink fixture"], check=True)
        (self.root / "module").mkdir()
        result = self.run_shards()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unsupported Git tree entry: module", result.stderr)
        self.assertFalse((self.server / ".ci/nextest-archive/probe-args.json").exists())

    def test_source_symlinks_cannot_escape_immutable_store_tree(self):
        link = self.root / "source-link"
        link.symlink_to("flake.nix")
        subprocess.run(["git", "-C", str(self.root), "add", "source-link"], check=True)
        subprocess.run(["git", "-C", str(self.root), "commit", "--quiet", "-m", "internal link"], check=True)
        result = self.run_shards()
        self.assertEqual(result.returncode, 0, result.stderr)
        link.unlink()
        link.symlink_to("../outside-source")
        subprocess.run(["git", "-C", str(self.root), "add", "source-link"], check=True)
        subprocess.run(["git", "-C", str(self.root), "commit", "--quiet", "-m", "external link"], check=True)
        result = self.run_shards()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("symlink escapes source: source-link", result.stderr)
        self.assertFalse((self.server / ".ci/nextest-archive/qualification-evidence.json").exists())

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
        sidecar = self.server / ".ci/nextest-archive/qualification-evidence.json"
        sidecar.write_text('{"old": true}')
        self.env["SHARD_EXIT"] = "17"
        result = self.run_shards()
        self.assertEqual(result.returncode, 17)
        self.assertTrue((self.server / ".ci/nextest-archive/cargo-timing.html").is_file())
        self.assertFalse(sidecar.exists())

    def test_unavailable_sandbox_fails_before_compilation(self):
        sidecar = self.server / ".ci/nextest-archive/qualification-evidence.json"
        sidecar.write_text('{"old": true}')
        self.env["SANDBOX_EXIT"] = "23"
        self.env["COMPILE_EXIT"] = "19"
        self.assertEqual(self.run_shards().returncode, 23)
        self.assertFalse((self.server / ".ci/nextest-archive/build-args.json").exists())
        self.assertFalse(sidecar.exists())

    def test_failed_compile_cannot_start_shards(self):
        sidecar = self.server / ".ci/nextest-archive/qualification-evidence.json"
        sidecar.write_text('{"old": true}')
        self.env["COMPILE_EXIT"] = "19"
        self.assertEqual(self.run_shards().returncode, 19)
        self.assertFalse((self.server / ".ci/nextest-archive/build-args.json").exists())
        self.assertFalse(sidecar.exists())

    def test_dirty_candidate_fails_before_nix_and_removes_old_sidecar(self):
        sidecar = self.server / ".ci/nextest-archive/qualification-evidence.json"
        sidecar.write_text('{"old": true}')
        self.required.write_text(self.required.read_text() + "# dirty\n")
        result = self.run_shards()
        self.assertEqual(result.returncode, 1)
        self.assertIn("candidate worktree is dirty before nextest build", result.stderr)
        self.assertFalse((self.server / ".ci/nextest-archive/probe-args.json").exists())
        self.assertFalse(sidecar.exists())

    def test_missing_shard_coverage_cannot_report_success(self):
        (self.server / ".ci/nextest-archive/mock-shards/shard-4/shared-coverage.json").unlink()
        self.assertNotEqual(self.run_shards().returncode, 0)


if __name__ == "__main__":
    unittest.main()
