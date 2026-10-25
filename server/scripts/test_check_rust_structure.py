#!/usr/bin/env python3
"""Regression coverage for the Rust import policy's permitted scopes."""
import importlib.util
import sys
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "check_rust_structure", Path(__file__).with_name("check-rust-structure.py")
)
structure = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = structure
spec.loader.exec_module(structure)


class ImportPolicyTests(unittest.TestCase):
    def test_allowed_imports(self):
        source = '''
        pub use child::*;
        pub(crate) use child::*;
        use std::collections::{HashMap, HashSet};
        use std::io::{self, Read};
        mod tests { use super::*; }
        mod imp { use super::*; }
        fn render() { use ErrorKind::*; }
        '''
        self.assertEqual(structure.violations(source), [])

    def test_external_file_parent_glob_is_rejected(self):
        self.assertEqual(len(structure.violations("use super::*;")), 1)

    def test_preludes_are_explicit_even_in_inline_modules(self):
        self.assertEqual(len(structure.violations(
            "mod tests { use gtk::prelude::*; }"
        )), 1)

    def test_module_level_enum_glob_is_rejected(self):
        self.assertEqual(len(structure.violations("use ErrorKind::*;")), 1)

    def test_mixed_imports_are_rejected(self):
        failures = structure.violations(
            "use std::{collections::HashMap, sync::{Arc, atomic::AtomicBool}};"
        )
        self.assertEqual(len(failures), 1)
        self.assertIn("multiple modules", failures[0][1])

    def test_comments_literals_and_nested_groups_do_not_change_scope(self):
        source = r'''
        // mod fake { use super::*;
        /* nested /* } */ mod fake { */
        const TEXT: &str = r##"mod fake { use super::*;"##;
        const OTHER: &str = "mod fake { \\";
        const BRACE: char = '{';
        use super::*;
        '''
        self.assertEqual(len(structure.violations(source)), 1)

    def test_use_tree_braces_do_not_count_as_inline_modules(self):
        source = "use std::collections::{HashMap, HashSet}; use super::*;"
        self.assertEqual(len(structure.violations(source)), 1)


if __name__ == "__main__":
    unittest.main()
