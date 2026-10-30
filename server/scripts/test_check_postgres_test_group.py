#!/usr/bin/env python3
"""Contract tests for the postgres test-group completeness check."""
import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap
import unittest

SCRIPT = Path(__file__).with_name("check_postgres_test_group.py")
spec = importlib.util.spec_from_file_location("check_postgres_test_group", SCRIPT)
check = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = check
spec.loader.exec_module(check)

NEXTEST = """\
[test-groups]
postgres = {{ max-threads = 1 }}

[[profile.default.overrides]]
filter = '''
package(demo) & (
    binary(cluster_e2e)
    | test(shielded::)
{extra})
'''
test-group = 'postgres'
"""

LIB = """\
mod shielded;
mod exposed;
#[path = "elsewhere/renamed_tests.rs"]
mod renamed;
mod nested {
    mod inner;
}
"""

READING_MODULE = """\
fn fixture() -> Option<String> {
    waddle_ws_test_support::require_test_postgres_url()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn postgres_case() {
        let Some(_url) = fixture() else { return };
    }

    #[test]
    fn sqlite_case() {
        // mentions require_test_postgres_url() only in a comment
        let _ = "fixture()";
    }
}
"""

DIRECT_READ = """\
#[tokio::test(flavor = "multi_thread")]
async fn direct() {
    let _ = std::env::var("WADDLE_TEST_POSTGRES_URL");
}
"""

INTEGRATION_SUPPORT = """\
pub fn open_db() -> Option<String> {
    waddle_ws_test_support::require_test_postgres_url()
}
"""

INTEGRATION_BINARY = """\
mod support;

#[tokio::test]
async fn uses_support() {
    let _ = support::open_db();
}

#[test]
fn pure() {}
"""


class PostgresGroupCheckTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.server = Path(self.directory.name)
        self.crate = self.server / "crates/demo"
        files = {
            "Cargo.toml": '[package]\nname = "demo"\n',
            "src/lib.rs": LIB,
            "src/shielded.rs": READING_MODULE,
            "src/exposed.rs": READING_MODULE,
            "src/elsewhere/renamed_tests.rs": DIRECT_READ,
            "src/nested/inner.rs": "#[test]\nfn no_database() {}\n",
            "tests/cluster_e2e.rs": INTEGRATION_BINARY,
            "tests/plain_e2e.rs": INTEGRATION_BINARY,
            "tests/support.rs": INTEGRATION_SUPPORT,
        }
        for name, content in files.items():
            path = self.crate / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(textwrap.dedent(content))
        (self.server / ".config").mkdir()
        self.allowlist = self.server / "scripts/postgres-schema-isolated.txt"
        self.allowlist.parent.mkdir()
        self.allowlist.write_text("# none\n")
        self.write_filter("")

    def write_filter(self, extra):
        (self.server / ".config/nextest.toml").write_text(NEXTEST.format(extra=extra))

    def run_check(self):
        return subprocess.run([sys.executable, "-B", str(SCRIPT), str(self.server)],
                              text=True, capture_output=True, check=False)

    def test_uncovered_tests_are_reported_with_their_nextest_names(self):
        result = self.run_check()
        self.assertEqual(result.returncode, 1, result.stdout)
        failures = {line.split(": ", 1)[1] for line in result.stderr.splitlines()
                    if line.startswith("Postgres-backed test outside")}
        self.assertEqual(failures, {
            "crates/demo/src/exposed.rs: demo exposed::tests::postgres_case",
            "crates/demo/src/elsewhere/renamed_tests.rs: demo renamed::direct",
            "crates/demo/tests/plain_e2e.rs: demo::plain_e2e uses_support",
        })

    def test_filter_and_allowlist_cover_everything(self):
        self.write_filter("    | test(exposed::tests::) | test(renamed::)\n")
        self.allowlist.write_text("crates/demo/tests/plain_e2e.rs  # uuid schema\n")
        result = self.run_check()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_stale_allowlist_entry_fails(self):
        self.write_filter("    | test(exposed::) | test(renamed::) | binary(plain_e2e)\n")
        self.allowlist.write_text("crates/demo/src/exposed.rs\n")
        result = self.run_check()
        self.assertEqual(result.returncode, 1)
        self.assertIn("stale allowlist entry", result.stderr)

    def test_unsupported_filter_is_rejected(self):
        self.write_filter("    | not test(x)\n")
        self.assertEqual(self.run_check().returncode, 2)

    def test_parse_filter_atoms(self):
        parsed = check.parse_filter("package(p) & (binary(a) | test(b::) )")
        self.assertEqual((parsed.package, parsed.binaries, parsed.tests), ("p", ["a"], ["b::"]))
        self.assertTrue(parsed.covers("p", "a", "anything"))
        self.assertTrue(parsed.covers("p", "lib", "x::b::y"))
        self.assertFalse(parsed.covers("q", "a", "x"))

    def test_repository_tree_passes(self):
        server = Path(__file__).resolve().parent.parent
        result = subprocess.run([sys.executable, "-B", str(SCRIPT), str(server)],
                                text=True, capture_output=True, check=False)
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
