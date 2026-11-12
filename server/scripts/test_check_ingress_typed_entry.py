#!/usr/bin/env python3
"""Regression coverage for the ingress typed-entry check."""
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

    def test_direct_begins_are_found_in_every_spelling(self):
        for source in (
            "let tx = db.begin().await?;",
            "let tx = self.db\n    .begin_immediate()\n    .await?;",
            "let tx = database.begin_discard_postgres_on_drop().await?;",
            "let tx = db.begin_catalog_fenced(&[lock]).await?;",
            "let tx = db.begin_catalog_fenced_read_only(&[lock]).await?;",
            "let tx = timeout(lock, self.main.begin()).await??;",
            "let tx = uow.begin_immediate().await?;",
            "let tx = pool().begin().await?;",
        ):
            self.assertEqual(len(check.occurrence_lines(source, check.BEGIN)), 1, source)

    def test_unit_of_work_begins_and_other_methods_are_not_direct_begins(self):
        source = '''
        let a = uow.begin().await?;
        let b = self.uow.begin().await?;
        let c = fixture.uow
            .begin()
            .await?;
        let d = uow.begin_with_timeouts(lock, statement).await?;
        let e = begin_ingress_entry(db, request).await?;
        // let f = db.begin().await?;
        async fn begin(&self) {}
        '''
        self.assertEqual(check.occurrence_lines(source, check.BEGIN), [])

    def test_epoch_locks_are_found_but_not_their_definitions_or_witness_forms(self):
        for name in (
            "acquire_epoch_lock_first",
            "acquire_supported_epoch_lock_first",
            "install_epoch_proof",
            "read_live_epoch_for_share",
        ):
            self.assertEqual(
                len(check.occurrence_lines(f"let epoch = {name}(tx).await?;", check.EPOCH_LOCK)), 1, name
            )
        source = '''
        pub(crate) async fn acquire_epoch_lock_first(tx: &mut Transaction<'_>) {}
        acquire_supported_epoch_in_prepared_transaction(tx, prepared).await?;
        read_live_epoch_for_share_in_prepared_transaction(tx, prepared).await?;
        '''
        self.assertEqual(check.occurrence_lines(source, check.EPOCH_LOCK), [])

    def test_autocommit_connections_are_found(self):
        for source in (
            "let connection = db.guard().await?;",
            "let conn = self.db\n    .guard()\n    .await?;",
        ):
            self.assertEqual(len(check.occurrence_lines(source, check.AUTOCOMMIT)), 1, source)
        source = "let guard = lock(); // db.guard()\nlet permit = relation_guard(tx);\n"
        self.assertEqual(check.occurrence_lines(source, check.AUTOCOMMIT), [])

    def test_test_modules_hide_begins_and_epoch_locks_but_not_attestation(self):
        source = '''
        fn production() {}
        #[cfg(test)]
        mod tests {
            async fn fixture() {
                let tx = db.begin_immediate().await.expect("begin {");
                acquire_epoch_lock_first(&mut tx).await;
                capability.attest_transaction(&mut tx).await;
            }
        }
        async fn after() { let tx = db.begin().await; }
        '''
        self.assertEqual(check.occurrence_lines(source, check.BEGIN), [11])
        self.assertEqual(check.occurrence_lines(source, check.EPOCH_LOCK), [])
        self.assertEqual(len(check.occurrence_lines(source, check.ATTEST)), 1)


class TypedHandleTests(unittest.TestCase):
    def test_raw_methods_must_take_the_entered_handle_first(self):
        typed = '''
        pub(crate) async fn lock_raw(
            writer: &mut EnteredTransaction<'_>,
            message_key: MessageKey,
        ) -> Result<(), IngressUowError> {}
        pub(crate) async fn copy_raw(tx: &mut crate::ingress_substrate::EnteredTransaction<'_>) {}
        async fn lock_on(tx: &mut Transaction<'_>) {}
        // pub(crate) async fn settle_all_raw(tx: &mut Transaction<'_>) {}
        '''
        self.assertEqual(check.untyped_raw_methods(typed), [])
        for source in (
            "pub(crate) async fn lock_raw(tx: &mut Transaction<'_>, key: MessageKey) {}",
            "pub(crate) async fn attach_raw(\n    tx: &mut Transaction<'_>,\n) {}",
            "pub async fn settle_all_raw(key: MessageKey, tx: &mut EnteredTransaction<'_>) {}",
            "async fn copy_raw(tx: &mut IngressUowTransaction<'_>) {}",
            "fn lock_raw() {}",
        ):
            self.assertEqual(len(check.untyped_raw_methods(source)), 1, source)

    def test_handle_literals_are_found_but_not_its_definition(self):
        self.assertEqual(
            check.handle_constructions("Ok(EnteredTransaction { transaction: tx })"), [1]
        )
        source = '''
        pub(crate) struct EnteredTransaction<'a> { transaction: Transaction<'a> }
        impl<'a> EnteredTransaction<'a> {}
        impl<'a> std::ops::Deref for EnteredTransaction<'a> {}
        impl std::ops::DerefMut for EnteredTransaction<'_> {}
        fn f(tx: &mut EnteredTransaction<'_>) -> EnteredTransaction<'_> { todo!() }
        '''
        self.assertEqual(check.handle_constructions(source), [])


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
        self.allowlist.write_text(f"{relative} attest 1  # verify-time check\n")
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
            f"{check.SOURCE / 'ingress/missing.rs'} attest 1\n",
            f"{check.ENTRY} attest 1\n",
            f"{check.SOURCE / 'mam_runtime.rs'} attest 1\n",
            f"{check.SOURCE / 'ingress/a.rs'} 1\n",
            f"{check.SOURCE / 'ingress/a.rs'} attest 0\n",
            f"{check.SOURCE / 'ingress/a.rs'} commit 1\n",
            f"{check.SOURCE / 'ingress/a_tests.rs'} begin 1\n",
        ):
            with self.subTest(text=text):
                self.allowlist.write_text(text)
                self.assertEqual(self.run_check().returncode, 1)

    def test_direct_begin_and_epoch_lock_fail_with_their_location(self):
        relative = check.SOURCE / "ingress_uow/writer.rs"
        self.write(relative, "async fn f() {\n    let tx = db.begin().await;\n    install_epoch_proof(tx).await;\n}\n")
        result = self.run_check()
        self.assertEqual(result.returncode, 1)
        self.assertIn(f"{relative}:2: begin", result.stdout)
        self.assertIn(f"{relative}:3: epoch-lock", result.stdout)

        reader = check.SOURCE / "ingress_substrate/reader.rs"
        self.write(reader, "async fn f() {\n    let connection = db.guard().await;\n}\n")
        result = self.run_check()
        self.assertIn(f"{reader}:2: autocommit", result.stdout)
        (self.server / reader).unlink()

        self.allowlist.write_text(f"{relative} begin 1\n{relative} epoch-lock 1\n")
        self.assertEqual(self.run_check().returncode, 0)

        # One kind's allowance does not cover another kind in the same file.
        self.allowlist.write_text(f"{relative} begin 1\n")
        result = self.run_check()
        self.assertEqual(result.returncode, 1)
        self.assertIn("epoch-lock", result.stdout)

    def test_test_files_may_begin_directly_but_not_attest(self):
        for name in ("ingress/tests.rs", "ingress/commit_tests.rs", "ingress_uow/tests/a.rs",
                     "ingress/test_support.rs", "ingress/execute_test_hooks.rs"):
            with self.subTest(name=name):
                relative = check.SOURCE / name
                self.write(relative, "async fn f() { db.begin().await; acquire_epoch_lock_first(tx).await; }\n")
                self.assertEqual(self.run_check().returncode, 0)
                self.write(relative, "async fn f() { capability.attest_transaction(tx).await; }\n")
                self.assertEqual(self.run_check().returncode, 1)
                (self.server / relative).unlink()

    def test_untyped_raw_repository_method_fails(self):
        self.write(
            check.RAW_REPOSITORY,
            "impl R {\n    pub(crate) async fn lock_raw(tx: &mut Transaction<'_>) {}\n}\n",
        )
        result = self.run_check()
        self.assertEqual(result.returncode, 1)
        self.assertIn(f"{check.RAW_REPOSITORY}:2: lock_raw must take", result.stdout)
        self.write(
            check.RAW_REPOSITORY,
            "impl R {\n    pub(crate) async fn lock_raw(tx: &mut EnteredTransaction<'_>) {}\n}\n",
        )
        self.assertEqual(self.run_check().returncode, 0)

    def test_handle_constructed_outside_the_entry_fails_anywhere_in_the_crate(self):
        relative = check.SOURCE / "push_service/worker.rs"
        self.write(relative, "fn f(tx: Transaction<'_>) {\n    EnteredTransaction { transaction: tx };\n}\n")
        result = self.run_check()
        self.assertEqual(result.returncode, 1)
        self.assertIn(f"{relative}:2: EnteredTransaction constructed outside", result.stdout)

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
