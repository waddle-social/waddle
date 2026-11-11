#!/usr/bin/env python3
"""Regression coverage for the ingress typed-entry attestation check."""
import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("check-ingress-typed-entry.py")
spec = importlib.util.spec_from_file_location("check_ingress_typed_entry", SCRIPT)
check = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = check
spec.loader.exec_module(check)


class CallDetectionTests(unittest.TestCase):
    def test_calls_are_found_in_every_spelling(self):
        for source in (
            "capability.attest_transaction(&mut tx).await?;",
            "self.writer\n    .attest_transaction(tx)\n    .await?;",
            "AttestedPostgresWriterCapability::attest_transaction(&capability, tx).await?;",
            "capability.attest_transaction (tx).await?;",
        ):
            self.assertEqual(len(check.call_lines(source)), 1, source)

    def test_comments_literals_definitions_and_other_names_are_ignored(self):
        source = '''
        // capability.attest_transaction(&mut tx)
        /// Calls `attest_transaction(` through the entry.
        /* attest_transaction(tx) */
        let message = "attest_transaction(tx) failed";
        let raw = r#"attest_transaction("tx")"#;
        let quote = '"';
        fn keep<'a>(x: &'a str) -> &'a str { x }
        pub(crate) async fn attest_transaction(&self) {}
        capability.reattest_transaction(tx);
        '''
        self.assertEqual(check.call_lines(source), [])

    def test_line_numbers_point_at_the_call(self):
        source = "// attest_transaction(\nlet a = 1;\ncapability\n    .attest_transaction(tx);\n"
        self.assertEqual(check.call_lines(source), [4])


class TreeTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.server = Path(self.directory.name) / "server"
        self.write(check.ENTRY, "async fn f() { capability.attest_transaction(tx).await; }\n")
        self.allowlist = self.server / check.ALLOWLIST
        self.allowlist.parent.mkdir(parents=True)
        self.allowlist.write_text("# no attach-time checks\n")

    def write(self, relative, text):
        path = self.server / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def run_check(self):
        return subprocess.run([sys.executable, str(SCRIPT), str(self.server.parent)],
                              text=True, capture_output=True, check=False)

    def test_entry_alone_passes(self):
        self.write(check.SOURCE / "ingress_uow/mod.rs", "fn f() { begin_ingress_entry(); }\n")
        result = self.run_check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_call_in_each_ingress_module_fails_with_its_location(self):
        for module in check.INGRESS_MODULES:
            with self.subTest(module=module):
                relative = check.SOURCE / module / "nested/writer.rs"
                self.write(relative, "fn f() {\n    capability.attest_transaction(tx);\n}\n")
                result = self.run_check()
                self.assertEqual(result.returncode, 1)
                self.assertIn(f"{relative}:2:", result.stdout)
                (self.server / relative).unlink()

    def test_calls_outside_the_ingress_modules_are_not_scanned(self):
        self.write(check.SOURCE / "mam_runtime.rs", "fn f() { capability.attest_transaction(tx); }\n")
        self.assertEqual(self.run_check().returncode, 0)

    def test_allowlisted_file_passes_only_at_its_exact_count(self):
        relative = check.SOURCE / "ingress_uow/relation_guard.rs"
        self.allowlist.write_text(f"{relative} 1  # verify-time check\n")
        self.write(relative, "fn f() { writer.attest_transaction(tx); }\n")
        self.assertEqual(self.run_check().returncode, 0)

        self.write(relative, "fn f() { writer.attest_transaction(a); writer.attest_transaction(b); }\n")
        result = self.run_check()
        self.assertEqual(result.returncode, 1)
        self.assertIn("allowlisted for 1", result.stdout)

        self.write(relative, "fn f() {}\n")
        self.assertEqual(self.run_check().returncode, 1)

    def test_stale_or_malformed_allowlist_entries_fail(self):
        for text in (
            f"{check.SOURCE / 'ingress/missing.rs'} 1\n",
            f"{check.ENTRY} 1\n",
            f"{check.SOURCE / 'mam_runtime.rs'} 1\n",
            f"{check.SOURCE / 'ingress/a.rs'}\n",
            f"{check.SOURCE / 'ingress/a.rs'} 0\n",
        ):
            with self.subTest(text=text):
                self.allowlist.write_text(text)
                self.assertEqual(self.run_check().returncode, 1)

    def test_entry_that_stops_attesting_fails(self):
        self.write(check.ENTRY, "async fn f() {}\n")
        self.assertEqual(self.run_check().returncode, 1)

    def test_missing_entry_or_allowlist_fails(self):
        self.allowlist.unlink()
        self.assertEqual(self.run_check().returncode, 1)
        self.allowlist.write_text("")
        (self.server / check.ENTRY).unlink()
        self.assertNotEqual(self.run_check().returncode, 0)


if __name__ == "__main__":
    unittest.main()
