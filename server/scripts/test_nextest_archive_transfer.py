import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("nextest-archive-transfer.sh")
EXPECTED = "/nix/store/00000000000000000000000000000000-test-archive"
REFERENCE = "/nix/store/11111111111111111111111111111111-runtime"
CONTENT_FILES = ("archive.tar.zst", "whole.filter", "shared.filter", "whole-inventory.json.gz",
                 "shared-inventory.json.gz", "runtime-references")
METADATA_FILES = ("archive-path", "archive-references", "archive-content-checksums")
PAYLOAD_FILES = (*METADATA_FILES, "archive-1.nar")


def checksum_manifest(directory, names):
    return "".join(f"{hashlib.sha256((directory / name).read_bytes()).hexdigest()}  {name}\n" for name in names)


class ArchiveTransferTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.log = self.root / "nix-calls"
        binary = self.root / "nix-store"
        # Most tests stop at the first Nix operation. Cache cases explicitly
        # model the pinned client's miss, error and unexpected-success replies.
        binary.write_text('''#!/usr/bin/env python3
import json, os, sys
with open(os.environ["NIX_CALL_LOG"], "a") as stream:
    stream.write(json.dumps(sys.argv[1:]) + "\\n")
mode = os.environ.get("NIX_TEST_REPLY", "stop")
if mode in ("miss", "warning-then-miss"):
    if mode == "warning-then-miss":
        print("warning: cache returned HTTP 500", file=sys.stderr)
    path = sys.argv[2]
    print("don't know how to build these paths:", file=sys.stderr)
    print("  " + path, file=sys.stderr)
    print("error: path '" + path + "' is required, but there is no substituter that can build it", file=sys.stderr)
    sys.exit(1)
if mode == "error":
    print("error: NAR hash mismatch", file=sys.stderr)
    sys.exit(1)
if mode == "wrong-output":
    print(os.environ["NIX_TEST_WRONG_PATH"])
    sys.exit(0)
sys.exit(42)
''')
        binary.chmod(0o755)
        self.env = dict(os.environ, PATH=f"{self.root}:{os.environ['PATH']}",
                        NIX_CALL_LOG=str(self.log), NIX_TEST_WRONG_PATH=REFERENCE)
        (self.root / "archive-path").write_text(EXPECTED + "\n")
        (self.root / "archive-references").write_text("")
        (self.root / "archive-1.nar").write_bytes(b"complete producer NAR payload")
        (self.root / "archive-content-checksums").write_text("".join(f"{'a' * 64}  {name}\n" for name in CONTENT_FILES))
        self.write_checksums()

    def write_checksums(self, partition="1"):
        (self.root / "archive-checksums").write_text(checksum_manifest(self.root, (*METADATA_FILES, f"archive-{partition}.nar")))

    def run_mode(self, mode="import", partition="1"):
        args = ["bash", str(SCRIPT), mode]
        if mode != "validate":
            args.append(EXPECTED)
        return subprocess.run([*args, str(self.root), partition], env=self.env, capture_output=True, text=True)

    def calls(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def assert_rejected_before_nix(self):
        for mode in ("import", "cache", "verify"):
            result = self.run_mode(mode)
            self.assertEqual(result.returncode, 1, (mode, result.stderr))
            self.assertFalse(self.log.exists(), result.stderr)

    def test_empty_reference_file_is_valid(self):
        self.assertEqual(self.run_mode().returncode, 42)
        self.assertEqual(self.calls(), [["--import"]])

    def test_selected_shard_requires_its_own_payload_and_manifest(self):
        self.assertEqual(self.run_mode(partition="2").returncode, 1)
        self.assertFalse(self.log.exists())
        (self.root / "archive-1.nar").rename(self.root / "archive-2.nar")
        self.assertEqual(self.run_mode(partition="2").returncode, 1)
        self.assertFalse(self.log.exists())
        self.write_checksums("2")
        self.assertEqual(self.run_mode(partition="2").returncode, 42)

    def test_invalid_partition_is_rejected_before_nix(self):
        for partition in ("0", "5", "../archive", "1\n2"):
            with self.subTest(partition=partition):
                self.assertEqual(self.run_mode(partition=partition).returncode, 1)
                self.assertFalse(self.log.exists())

    def test_runtime_references_cannot_build_locally_or_remotely(self):
        (self.root / "archive-references").write_text(REFERENCE + "\n")
        self.write_checksums()
        self.assertEqual(self.run_mode().returncode, 42)
        self.assertEqual(self.calls(), [["--realise", "--max-jobs", "0", "--builders", "",
                                        "--option", "fallback", "false", REFERENCE]])

    def test_every_metadata_file_is_required_before_nix(self):
        for name in (*METADATA_FILES, "archive-checksums"):
            with self.subTest(file=name):
                file = self.root / name
                content = file.read_bytes()
                file.unlink()
                self.assert_rejected_before_nix()
                file.write_bytes(content)

    def test_metadata_tampering_is_rejected_before_nix(self):
        for name in (*METADATA_FILES, "archive-checksums"):
            with self.subTest(file=name):
                file = self.root / name
                content = file.read_bytes()
                file.write_bytes(content[:-1] if content else b"unexpected reference")
                self.assert_rejected_before_nix()
                file.write_bytes(content)

    def test_raw_payload_is_required_and_fully_checksummed_before_import(self):
        payload = self.root / "archive-1.nar"
        for content in (None, b"", b"truncated NAR"):
            with self.subTest(content=content):
                if content is None:
                    payload.unlink()
                else:
                    payload.write_bytes(content)
                self.assertEqual(self.run_mode().returncode, 1)
                self.assertFalse(self.log.exists())

    def test_manifest_cannot_skip_payload_validation(self):
        manifest = self.root / "archive-checksums"
        manifest.write_text("".join(manifest.read_text().splitlines(keepends=True)[:-1]))
        self.assert_rejected_before_nix()

    def test_content_manifest_cannot_select_arbitrary_files(self):
        manifest = self.root / "archive-content-checksums"
        for replacement in ("../archive.tar.zst", "/etc/passwd", "archiveXtarXzst"):
            with self.subTest(replacement=replacement):
                manifest.write_text("".join(f"{'a' * 64}  {replacement if name == 'archive.tar.zst' else name}\n" for name in CONTENT_FILES))
                self.write_checksums()
                self.assert_rejected_before_nix()

    def test_wrong_expected_path_is_rejected_even_with_valid_checksums(self):
        (self.root / "archive-path").write_text(REFERENCE + "\n")
        self.write_checksums()
        self.assert_rejected_before_nix()

    def test_all_references_are_validated_before_any_nix_operation(self):
        (self.root / "archive-references").write_text(REFERENCE + "\nnot-a-store-path\n")
        self.write_checksums()
        self.assert_rejected_before_nix()

    def test_metadata_preflight_needs_neither_nix_nor_raw_payload(self):
        (self.root / "archive-1.nar").unlink()
        self.assertEqual(self.run_mode("validate").returncode, 0)
        self.assertFalse(self.log.exists())

    def test_only_an_exact_ordinary_cache_miss_authorizes_fallback(self):
        (self.root / "archive-1.nar").unlink()
        self.env["NIX_TEST_REPLY"] = "miss"
        self.assertEqual(self.run_mode("cache").returncode, 2)
        self.assertEqual(self.calls(), [["--realise", EXPECTED, "--max-jobs", "0", "--builders", "",
                                        "--option", "fallback", "false", "--log-format", "raw"]])

    def test_cache_errors_cannot_take_raw_fallback(self):
        for reply in ("error", "warning-then-miss", "wrong-output"):
            with self.subTest(reply=reply):
                self.env["NIX_TEST_REPLY"] = reply
                self.assertEqual(self.run_mode("cache").returncode, 1)

    def test_verify_requires_registered_store_output(self):
        self.assertEqual(self.run_mode("verify").returncode, 42)
        self.assertEqual(self.calls(), [["--check-validity", EXPECTED]])


@unittest.skipUnless(shutil.which("nix-store"), "native transfer contracts require Nix (installed before CI helper checks)")
class NativeArchiveTransferTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        source = self.root / "archive-output"
        source.mkdir()
        for name in CONTENT_FILES:
            (source / name).write_bytes(b"" if name == "runtime-references" else f"producer {name}\n".encode())
        (source / "archive-content-checksums").write_text(checksum_manifest(source, CONTENT_FILES))
        # Register a tiny fixture, not a derivation: these tests never compile.
        self.expected = subprocess.check_output(["nix-store", "--add", str(source)], text=True).strip()
        # Leave the tiny deterministic store object for runner teardown: deleting
        # it here could race an active binary-cache upload watching the Nix store.
        self.transfer = self.root / "transfer"
        exported = self.run_mode("export")
        self.assertEqual(exported.returncode, 0, exported.stderr)

    def run_mode(self, mode):
        return subprocess.run(["bash", str(SCRIPT), mode, self.expected, str(self.transfer), "1"], capture_output=True, text=True)

    def test_cache_hit_works_without_raw_payload_and_is_reverified(self):
        (self.transfer / "archive-1.nar").unlink()
        restored = self.run_mode("cache")
        self.assertEqual(restored.returncode, 0, restored.stderr)
        verified = self.run_mode("verify")
        self.assertEqual(verified.returncode, 0, verified.stderr)

    def test_cache_content_mismatch_is_fatal_even_with_valid_metadata(self):
        manifest = self.transfer / "archive-content-checksums"
        manifest.chmod(0o644)
        lines = manifest.read_text().splitlines(keepends=True)
        lines[0] = "0" * 64 + "  archive.tar.zst\n"
        manifest.write_text("".join(lines))
        (self.transfer / "archive-checksums").write_text(checksum_manifest(self.transfer, PAYLOAD_FILES))
        for mode in ("cache", "verify", "import"):
            result = self.run_mode(mode)
            self.assertEqual(result.returncode, 1, (mode, result.stderr))
            self.assertIn("content checksum mismatch", result.stderr)

    def test_raw_fallback_roundtrip_retains_payload_and_content_checks(self):
        imported = self.run_mode("import")
        self.assertEqual(imported.returncode, 0, imported.stderr)
        (self.transfer / "archive-1.nar").write_bytes(b"truncated NAR")
        self.assertEqual(self.run_mode("import").returncode, 1)


if __name__ == "__main__":
    unittest.main()
