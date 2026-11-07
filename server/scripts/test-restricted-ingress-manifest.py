#!/usr/bin/env python3
"""Independently check manifest sources and rebuild it from a PG17 catalog."""

import hashlib
import json
import os
from pathlib import Path
import re
import runpy
import subprocess
import sys
import unittest

SERVER = Path(__file__).resolve().parents[1]
DB = SERVER / "crates/waddle-server/src/db"
SOURCES = {
    "candidate": DB / "migrations/owner/ingress_definer_api_candidate.sql",
    "relation": DB / "migrations/owner/ingress_epoch_relation_guard_candidate.sql",
    "waddle": DB / "migrations/waddle.rs",
    "global": DB / "migrations/global.rs",
    "pending_schema": DB.parent / "pending_delivery/database/schema.rs",
    "pending_recovery": DB.parent / "pending_delivery/database/recovery_order.rs",
    "structure_query": DB / "physical_attestation/restricted_ingress/structure.sql",
}
MANIFEST = DB / "physical_attestation/restricted_ingress_manifest_v2.json"


class RestrictedIngressManifestTest(unittest.TestCase):
    def test_literal_migration_include_and_invalid_catalog(self):
        generator = runpy.run_path(str(SERVER / "scripts/generate-restricted-ingress-manifest.py"))
        migrations = generator["migrations"]
        asset = "owner/p21_receipt_journal_postgres.sql"
        source = f'pub const V_EXAMPLE: &str = include_str!("{asset}");\n'
        source += "sql_postgres: V_EXAMPLE,\n"
        self.assertEqual(migrations(source), (DB / "migrations" / asset).read_text())
        with self.assertRaises(ValueError):
            migrations(
                'pub const V_EXAMPLE: &str = include_str!("../outside.sql");\n'
                "sql_postgres: V_EXAMPLE,"
            )
        with self.assertRaises(ValueError):
            migrations("sql_postgres: V_MISSING,")
        with self.assertRaises(ValueError):
            migrations(source + "sql_postgres: V_EXAMPLE,")

    def test_source_digests_and_every_candidate_body(self):
        manifest = json.loads(MANIFEST.read_text())
        self.assertEqual(manifest["version"], 2)
        self.assertEqual(set(manifest["source_sha256"]), set(SOURCES))
        sources = {name: path.read_bytes() for name, path in SOURCES.items()}
        for name, contents in sources.items():
            self.assertEqual(hashlib.sha256(contents).hexdigest(), manifest["source_sha256"][name])

        # Parse the candidate source independently of the catalog generator.
        # Match each named body to its exact byte slice, not only its checksum.
        pattern = re.compile(
            rb"CREATE\s+OR\s+REPLACE\s+FUNCTION\s+__SCHEMA__\.(\w+)\s*"
            rb"\([^;]*?\)\s*RETURNS\b.*?\bAS\s+\$\$(.*?)\$\$\s*;",
            re.DOTALL | re.IGNORECASE,
        )
        bodies = {name.decode(): body for name, body in pattern.findall(sources["candidate"])}
        functions = manifest["functions"]
        self.assertEqual(len(functions), len({function["name"] for function in functions}))
        candidate = {function["name"]: function for function in functions if function["source_file"] == "candidate"}
        self.assertEqual(set(candidate), set(bodies))
        for name, function in candidate.items():
            start, end = function["source_start"], function["source_end"]
            self.assertLess(start, end, name)
            self.assertEqual(sources["candidate"][start:end], bodies[name], name)

        # Every non-candidate slice must also be an actual function-body slice
        # in its named source, including the pending-recovery quoted body.
        for function in functions:
            source = sources[function["source_file"]]
            start, end = function["source_start"], function["source_end"]
            self.assertGreaterEqual(start, 0, function["name"])
            self.assertLess(start, end, function["name"])
            self.assertLessEqual(end, len(source), function["name"])
            before, after = source[:start], source[end:]
            if function["source_file"] == "pending_recovery":
                self.assertTrue(before.endswith(b'POSTGRES_IMMUTABLE_BODY: &str = "'))
                self.assertTrue(after.startswith(b'"'))
            else:
                self.assertTrue(before.endswith(b"AS $$"), function["name"])
                self.assertTrue(after.startswith(b"$$"), function["name"])

    @unittest.skipUnless(os.environ.get("WADDLE_TEST_POSTGRES_URL"), "PG17 fixture required")
    def test_independent_catalog_construction(self):
        subprocess.run(
            [sys.executable, str(SERVER / "scripts/generate-restricted-ingress-manifest.py"), "--check"],
            cwd=SERVER,
            check=True,
        )


if __name__ == "__main__":
    unittest.main()
