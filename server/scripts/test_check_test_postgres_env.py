#!/usr/bin/env python3
"""Regression coverage for the Postgres test-environment read check."""
import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("check-test-postgres-env.py")
spec = importlib.util.spec_from_file_location("check_test_postgres_env", SCRIPT)
check = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = check
spec.loader.exec_module(check)


class LiteralDetectionTests(unittest.TestCase):
    def assert_rejected(self, source, count=1):
        self.assertEqual(len(check.violations(source)), count, source)

    def test_direct_reads_are_rejected_in_every_spelling(self):
        for source in (
            'let Ok(url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else { return; };',
            'let url = env::var("WADDLE_TEST_POSTGRES_URL").ok()?;',
            'let url = std::env::var(\n    "WADDLE_TEST_POSTGRES_URL",\n);',
            'let url = std::env::var_os("WADDLE_TEST_POSTGRES_URL");',
            'let url = option_env!("WADDLE_TEST_POSTGRES_URL");',
            'let url = std::env::var(r"WADDLE_TEST_POSTGRES_URL");',
            'let url = std::env::var(r#"WADDLE_TEST_POSTGRES_URL"#);',
            'let url = std::env::var("WADDLE_TEST_\\x50OSTGRES_URL");',
            'let url = std::env::var("WADDLE_TEST_\\\n    POSTGRES_URL");',
            'const PG: &str = "WADDLE_TEST_POSTGRES_URL"; let url = env::var(PG);',
            'let required = env::var("WADDLE_TEST_REQUIRE_POSTGRES");',
        ):
            self.assert_rejected(source)

    def test_helper_calls_comments_and_messages_are_allowed(self):
        source = '''
        // Reads WADDLE_TEST_POSTGRES_URL through the helper.
        /* env::var("WADDLE_TEST_POSTGRES_URL") /* nested */ */
        let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
            eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set");
            return;
        };
        let quote = '"';
        let s = "escaped \\" WADDLE_TEST_POSTGRES_URL";
        fn f<'a>(x: &'a str) -> &'a str { x }
        '''
        self.assertEqual(check.violations(source), [])

    def test_line_numbers_point_at_the_literal(self):
        source = '// "WADDLE_TEST_POSTGRES_URL"\nlet a = "x";\nlet b = env::var("WADDLE_TEST_POSTGRES_URL");\n'
        self.assertEqual([line for line, _ in check.violations(source)], [3])


class TreeTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.server = Path(self.directory.name) / "server"
        helper = self.server / check.HELPER
        helper.parent.mkdir(parents=True)
        helper.write_text('pub const URL: &str = "WADDLE_TEST_POSTGRES_URL";\n')
        self.test_file = self.server / "crates/waddle-server/tests/pg.rs"
        self.test_file.parent.mkdir(parents=True)

    def run_check(self):
        return subprocess.run([sys.executable, str(SCRIPT), str(self.server.parent)],
                              text=True, capture_output=True, check=False)

    def test_helper_module_is_the_only_allowed_reader(self):
        self.test_file.write_text("fn f() { waddle_ws_test_support::require_test_postgres_url(); }\n")
        result = self.run_check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_direct_read_outside_the_helper_fails_with_its_location(self):
        self.test_file.write_text('fn f() {\n    std::env::var("WADDLE_TEST_POSTGRES_URL");\n}\n')
        result = self.run_check()
        self.assertEqual(result.returncode, 1)
        self.assertIn("server/crates/waddle-server/tests/pg.rs:2:", result.stdout)

    def test_missing_helper_fails(self):
        (self.server / check.HELPER).unlink()
        self.test_file.write_text("fn f() {}\n")
        self.assertNotEqual(self.run_check().returncode, 0)


if __name__ == "__main__":
    unittest.main()
