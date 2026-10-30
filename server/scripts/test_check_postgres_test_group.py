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
mod fixture;
mod via_include;
mod offline;
mod caller;
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

# Shared by an integration binary and, through include!, by the library.
FIXTURE_SUPPORT = """\
pub struct DbFixture;

impl DbFixture {
    pub async fn isolated(name: &str) -> Option<Self> {
        Self::isolated_with_pool(name, 1).await
    }

    pub async fn isolated_with_pool(_name: &str, _pool: u32) -> Option<Self> {
        let _url = waddle_ws_test_support::require_test_postgres_url()?;
        Some(Self)
    }
}
"""

LIB_FIXTURE = """\
include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixture_support.rs"
));
"""

VIA_INCLUDE = """\
#[tokio::test]
async fn uses_included_fixture() {
    let _ = crate::fixture::DbFixture::isolated("x").await;
}
"""

OFFLINE = """\
pub(crate) async fn shared_state() -> Option<String> {
    waddle_ws_test_support::require_test_postgres_url()
}
"""

CALLER = """\
#[tokio::test]
async fn calls_module_qualified_fixture() {
    let _ = crate::offline::shared_state().await;
}
"""

DIR_BINARY_MAIN = """\
mod helpers;
"""

DIR_BINARY_HELPERS = """\
#[tokio::test]
async fn reads_db() {
    let _ = waddle_ws_test_support::require_test_postgres_url();
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
            "tests/fixture_support.rs": FIXTURE_SUPPORT,
            "src/fixture.rs": LIB_FIXTURE,
            "src/via_include.rs": VIA_INCLUDE,
            "src/offline.rs": OFFLINE,
            "src/caller.rs": CALLER,
            "tests/dir_e2e/main.rs": DIR_BINARY_MAIN,
            "tests/dir_e2e/helpers.rs": DIR_BINARY_HELPERS,
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
            # include!d fixture reached by a qualified call from another file
            "crates/demo/src/via_include.rs: demo via_include::uses_included_fixture",
            # module-qualified call to a fixture in another file
            "crates/demo/src/caller.rs: demo caller::calls_module_qualified_fixture",
            # tests/<name>/main.rs binary
            "crates/demo/tests/dir_e2e/helpers.rs: demo::dir_e2e helpers::reads_db",
        })

    def test_filter_and_allowlist_cover_everything(self):
        self.write_filter("    | test(exposed::tests::) | test(renamed::) | test(caller::)"
                          " | binary(dir_e2e)\n")
        self.allowlist.write_text(
            "crates/demo/tests/plain_e2e.rs  # uuid schema\n"
            "crates/demo/tests/fixture_support.rs::DbFixture::isolated_with_pool  # uuid schema\n")
        result = self.run_check()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_isolated_fixture_entry_covers_callers_reached_through_include(self):
        self.write_filter("    | test(exposed::tests::) | test(renamed::) | test(caller::)"
                          " | binary(dir_e2e) | binary(plain_e2e)\n")
        result = self.run_check()
        self.assertIn("demo via_include::uses_included_fixture", result.stderr)
        self.allowlist.write_text(
            "crates/demo/tests/fixture_support.rs::DbFixture::isolated_with_pool\n")
        result = self.run_check()
        self.assertEqual(result.returncode, 0, result.stderr)
        # A fixture entry that matches nothing is stale.
        self.allowlist.write_text("crates/demo/tests/fixture_support.rs::DbFixture::isolated\n")
        result = self.run_check()
        self.assertEqual(result.returncode, 1)
        self.assertIn("stale allowlist entry", result.stderr)

    def test_stale_allowlist_entry_fails(self):
        self.write_filter("    | test(exposed::) | test(renamed::) | binary(plain_e2e) | test(via_include::)"
                          " | test(caller::) | binary(dir_e2e)\n")
        self.allowlist.write_text("crates/demo/src/exposed.rs\n")
        result = self.run_check()
        self.assertEqual(result.returncode, 1)
        self.assertIn("stale allowlist entry", result.stderr)

    def test_unsupported_filter_is_rejected(self):
        for extra in ("    | not test(x)\n", "    | test(=exact)\n", "    | test(/re/)\n",
                      "    | binary(#glob)\n"):
            self.write_filter(extra)
            self.assertEqual(self.run_check().returncode, 2, extra)

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
