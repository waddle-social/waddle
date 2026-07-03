//! Conformance guard for the CLAUDE.md hard rule: every implemented
//! XEP module (`src/xep/xepNNNN.rs`) must have a dedicated test suite
//! (`xep*NNNN*.rs`) in either `waddle-xmpp/tests/` or
//! `waddle-server/tests/`.

use std::collections::BTreeSet;
use std::path::Path;

fn xep_numbers_in(dir: &Path) -> BTreeSet<String> {
    let mut numbers = BTreeSet::new();
    for entry in std::fs::read_dir(dir).expect("readable directory") {
        let name = entry.expect("readable entry").file_name();
        let name = name.to_string_lossy();
        // Numbered modules only (`xep0047.rs`), not `xep_waddle_*.rs`.
        if let Some(stem) = name.strip_prefix("xep").and_then(|s| s.strip_suffix(".rs")) {
            if !stem.is_empty() && stem.chars().all(|c| c.is_ascii_digit()) {
                numbers.insert(stem.to_owned());
            }
        }
    }
    numbers
}

fn suite_exists_for(number: &str, test_dirs: &[&Path]) -> bool {
    test_dirs.iter().any(|dir| {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries.filter_map(Result::ok).any(|entry| {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    // Exact `xepNNNN` prefix followed by a suite name or
                    // extension — `name.contains(number)` would let a
                    // future `xep04471_foo.rs` satisfy module `xep0447`.
                    let Some(rest) = name
                        .strip_prefix("xep")
                        .and_then(|s| s.strip_prefix(number))
                    else {
                        return false;
                    };
                    (rest == ".rs" || rest.starts_with('_'))
                        && name.ends_with(".rs")
                        && non_empty_suite(&entry.path())
                })
            })
            .unwrap_or(false)
    })
}

/// A suite file must contain at least one test — an empty file would
/// satisfy an existence-only check without testing anything.
fn non_empty_suite(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .map(|src| src.contains("#[test]") || src.contains("#[tokio::test"))
        .unwrap_or(false)
}

#[test]
fn every_numbered_xep_module_has_a_dedicated_test_suite() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let xep_src_dir = manifest_dir.join("src/xep");
    let waddle_xmpp_tests = manifest_dir.join("tests");
    let waddle_server_tests = manifest_dir.join("../waddle-server/tests");
    let test_dirs = [waddle_xmpp_tests.as_path(), waddle_server_tests.as_path()];

    let missing: Vec<String> = xep_numbers_in(&xep_src_dir)
        .into_iter()
        .filter(|number| !suite_exists_for(number, &test_dirs))
        .collect();

    assert!(
        missing.is_empty(),
        "CLAUDE.md hard rule: every implemented XEP needs a dedicated test suite.\n\
         Modules without a `xep*NNNN*.rs` file in crates/waddle-xmpp/tests or \
         crates/waddle-server/tests: {}",
        missing
            .iter()
            .map(|n| format!("xep{n}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
}
