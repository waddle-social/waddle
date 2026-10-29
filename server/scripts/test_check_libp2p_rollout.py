import contextlib
import importlib.util
import io
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).with_name("check-libp2p-rollout.py")
SPEC = importlib.util.spec_from_file_location("check_libp2p_rollout", SCRIPT)
rollout = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(rollout)


class BaselineValidationTests(unittest.TestCase):
    def test_malicious_baselines_are_rejected_before_git_or_build_setup(self):
        for baseline in (
            "--output=/tmp/rollout-injected",
            "--exec=touch /tmp/rollout-injected",
            "-o/tmp/rollout-injected",
            "HEAD\n--output=/tmp/rollout-injected",
            "HEAD\r--output=/tmp/rollout-injected",
            "HEAD\x00--output=/tmp/rollout-injected",
            "HEAD\t--output=/tmp/rollout-injected",
            "HEAD;touch /tmp/rollout-injected",
        ):
            with self.subTest(baseline=repr(baseline)):
                with patch.object(sys, "argv", [str(SCRIPT), "--baseline=" + baseline]):
                    with patch.object(rollout.subprocess, "check_output") as git:
                        with patch.object(rollout.tempfile, "TemporaryDirectory") as workspace:
                            with contextlib.redirect_stderr(io.StringIO()):
                                with self.assertRaises(SystemExit) as error:
                                    rollout.main()
                            self.assertEqual(error.exception.code, 2)
                        workspace.assert_not_called()
                    git.assert_not_called()

    def test_untrusted_git_output_must_be_a_complete_commit_id(self):
        for output in (
            "--output=/tmp/rollout-injected\n",
            "a" * 39,
            "a" * 41,
            "a" * 63,
            "a" * 65,
            "g" * 40,
            "a" * 40 + "\n--output=/tmp/rollout-injected",
        ):
            with self.subTest(output=repr(output)):
                with patch.object(rollout.subprocess, "check_output", return_value=output):
                    with self.assertRaises(ValueError):
                        rollout.resolve_baseline("HEAD")

    def test_sha1_and_sha256_commit_ids_are_supported(self):
        for length in (40, 64):
            commit = "a" * length
            with self.subTest(length=length):
                with patch.object(rollout.subprocess, "check_output", return_value=commit + "\n"):
                    self.assertEqual(rollout.resolve_baseline("HEAD"), commit)

    def test_resolved_commit_is_used_for_actual_file_and_archive_reads(self):
        with tempfile.TemporaryDirectory() as temporary:
            repository = Path(temporary) / "repository"
            repository.mkdir()
            subprocess.run(["git", "init", "-q", str(repository)], check=True)
            lockfile = repository / "server/Cargo.lock"
            lockfile.parent.mkdir()
            lockfile.write_text('[[package]]\nname = "libp2p"\nversion = "0.56.0"\n')
            kameo = repository / "server/vendor/kameo"
            kameo.mkdir(parents=True)
            (kameo / "Cargo.toml").write_text('[package]\nname = "kameo"\nversion = "0.20.0"\n')
            subprocess.run(["git", "add", "."], cwd=repository, check=True)
            subprocess.run([
                "git", "-c", "user.name=Test", "-c", "user.email=test@example.test",
                "commit", "-qm", "test(server): preserve a baseline reference for validation",
            ], cwd=repository, check=True)
            subprocess.run(["git", "tag", "rollout-baseline"], cwd=repository, check=True)
            expected = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repository, text=True).strip()
            blob = subprocess.check_output(["git", "hash-object", "-w", "--stdin"], cwd=repository, input=b"not a commit").decode().strip()
            with patch.object(rollout, "ROOT", repository):
                for reference in ("HEAD", "rollout-baseline", expected[:12]):
                    with self.subTest(reference=reference):
                        self.assertEqual(rollout.resolve_baseline(reference), expected)
                with self.assertRaises(ValueError):
                    rollout.resolve_baseline(blob)
                peer = Path(temporary) / "old-peer"
                self.assertEqual(rollout.prepare_peer(peer, expected, "old"), "0.56.0")
                self.assertEqual((peer / "server/vendor/kameo/Cargo.toml").read_text(), (kameo / "Cargo.toml").read_text())


if __name__ == "__main__":
    unittest.main()
