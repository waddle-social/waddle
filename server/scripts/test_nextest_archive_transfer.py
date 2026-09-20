import hashlib
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("nextest-archive-transfer.sh")
EXPECTED = "/nix/store/00000000000000000000000000000000-test-archive"
REFERENCE = "/nix/store/11111111111111111111111111111111-runtime"
PAYLOAD_FILES = ("archive-path", "archive-references", "archive-1.nar")


class ArchiveTransferTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.log = self.root / "nix-calls"
        binary = self.root / "nix-store"
        # Stop at the first Nix operation so these tests need no Nix daemon or
        # writable store. Reaching exit 42 means transfer validation passed.
        binary.write_text('#!/bin/bash\nprintf "%s\\n" "$*" >> "$NIX_CALL_LOG"\nexit 42\n')
        binary.chmod(0o755)
        self.env = dict(os.environ, PATH=f"{self.root}:{os.environ['PATH']}", NIX_CALL_LOG=str(self.log))
        (self.root / "archive-path").write_text(EXPECTED + "\n")
        (self.root / "archive-references").write_text("")
        (self.root / "archive-1.nar").write_bytes(b"complete producer NAR payload")
        self.write_checksums()

    def write_checksums(self, partition="1"):
        payload_files = ("archive-path", "archive-references", f"archive-{partition}.nar")
        manifest = "".join(
            f"{hashlib.sha256((self.root / name).read_bytes()).hexdigest()}  {name}\n"
            for name in payload_files
        )
        (self.root / "archive-checksums").write_text(manifest)

    def run_import(self, partition="1"):
        return subprocess.run(
            ["bash", str(SCRIPT), "import", EXPECTED, str(self.root), partition],
            env=self.env, capture_output=True, text=True,
        )

    def assert_rejected_before_nix(self):
        result = self.run_import()
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.log.exists(), result.stderr)

    def test_empty_reference_file_is_valid(self):
        self.assertEqual(self.run_import().returncode, 42)
        self.assertEqual(self.log.read_text(), "--import\n")

    def test_selected_shard_requires_its_own_payload_and_manifest(self):
        self.assertNotEqual(self.run_import("2").returncode, 0)
        self.assertFalse(self.log.exists())
        (self.root / "archive-1.nar").rename(self.root / "archive-2.nar")
        self.assertNotEqual(self.run_import("2").returncode, 0)
        self.assertFalse(self.log.exists())
        self.write_checksums("2")
        self.assertEqual(self.run_import("2").returncode, 42)

    def test_invalid_partition_is_rejected_before_nix(self):
        for partition in ("0", "5", "../archive", "1\n2"):
            with self.subTest(partition=partition):
                self.assertNotEqual(self.run_import(partition).returncode, 0)
                self.assertFalse(self.log.exists())

    def test_runtime_references_are_substituted_without_building(self):
        (self.root / "archive-references").write_text(REFERENCE + "\n")
        self.write_checksums()
        self.assertEqual(self.run_import().returncode, 42)
        self.assertEqual(self.log.read_text(), f"--realise --option max-jobs 0 {REFERENCE}\n")

    def test_every_transfer_file_is_required(self):
        for name in (*PAYLOAD_FILES, "archive-checksums"):
            with self.subTest(file=name):
                file = self.root / name
                content = file.read_bytes()
                file.unlink()
                self.assert_rejected_before_nix()
                file.write_bytes(content)

    def test_tampered_or_truncated_transfer_files_are_rejected(self):
        for name in (*PAYLOAD_FILES, "archive-checksums"):
            with self.subTest(file=name):
                file = self.root / name
                content = file.read_bytes()
                file.write_bytes(content[:-1] if content else b"unexpected reference")
                self.assert_rejected_before_nix()
                file.write_bytes(content)

    def test_manifest_cannot_skip_payload_validation(self):
        manifest = self.root / "archive-checksums"
        manifest.write_text("".join(manifest.read_text().splitlines(keepends=True)[:2]))
        self.assert_rejected_before_nix()

    def test_wrong_expected_path_is_rejected_even_with_valid_checksums(self):
        (self.root / "archive-path").write_text(REFERENCE + "\n")
        self.write_checksums()
        self.assert_rejected_before_nix()

    def test_all_references_are_validated_before_any_nix_operation(self):
        (self.root / "archive-references").write_text(REFERENCE + "\nnot-a-store-path\n")
        self.write_checksums()
        self.assert_rejected_before_nix()


if __name__ == "__main__":
    unittest.main()
