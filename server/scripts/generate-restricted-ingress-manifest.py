#!/usr/bin/env python3
"""Derive the dark PG17 manifest in a rollback-only disposable database.

Requires an administrator URL in WADDLE_TEST_POSTGRES_URL and psql on PATH
(or PSQL pointing to it). The database's default collation must be C, for
example one created from template0 with LOCALE_PROVIDER builtin and
BUILTIN_LOCALE 'C'. Builds the schema from the PostgreSQL schema baseline
and the unregistered owner candidates. No existing target objects are
dropped. This is a developer artifact producer, never a startup installer.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[1] / "crates/waddle-server/src/db"
BASELINE = ROOT.parents[1] / "sql/baseline/postgres.sql"
SCHEMA = "restricted_manifest_build"
API = "restricted_manifest_api"
RUNTIME = "restricted_manifest_runtime"
FILES = {
    "candidate": ROOT / "migrations/owner/ingress_definer_api_candidate.sql",
    "relation": ROOT / "migrations/owner/ingress_epoch_relation_guard_candidate.sql",
    "baseline": BASELINE,
    "structure_query": ROOT / "physical_attestation/restricted_ingress/structure.sql",
}
OUTPUT = ROOT / "physical_attestation/restricted_ingress_manifest_v3.json"
VERSION = 3


def render(source):
    for placeholder, value in [
        # The baseline's template spellings of the schema's name.
        ("__SCHEMA_QUOTED__", f'"{SCHEMA}"'),
        ("__SCHEMA_LITERAL__", f"'{SCHEMA}'"),
        ("__SCHEMA_PATH__", f'"{SCHEMA}"'),
        ("__SCHEMA__", f'"{SCHEMA}"'),
        ("__API_OWNER_LITERAL__", API),
        ("__RUNTIME_ROLE_LITERAL__", RUNTIME),
        ("__API_OWNER_IDENT__", API),
        ("__RUNTIME_ROLE_IDENT__", RUNTIME),
    ]:
        source = source.replace(placeholder, value)
    return source


def produce(sources):
    sql = (
        "BEGIN; DO $$ BEGIN IF current_setting('server_version_num')::integer "
        "NOT BETWEEN 170000 AND 179999 THEN RAISE EXCEPTION 'requires PG17'; END IF; "
        # The structure query orders trigger definitions with the database's
        # default collation, and the pinned manifest records byte order.
        "IF NOT ('RI_x'::text < 'ingress'::text) THEN RAISE EXCEPTION "
        "'requires a database whose default collation is C'; END IF; END $$; "
        f"CREATE ROLE {API}; CREATE ROLE {RUNTIME}; "
        f"CREATE SCHEMA {SCHEMA}; SET search_path={SCHEMA};\n"
    )
    sql += render(sources["baseline"]) + "\n"
    sql += sources["relation"] + render(sources["candidate"])
    sql += f"""
SELECT jsonb_agg(jsonb_build_object(
  'name',p.proname,
  'properties',(to_jsonb(p)-ARRAY['oid','pronamespace','proowner','prolang','prosrc','proacl']),
  'source',p.prosrc,
  'owner',CASE WHEN p.proowner=(SELECT oid FROM pg_roles WHERE rolname='{API}') THEN 'api' ELSE 'schema' END,
  'public_execute',EXISTS(SELECT 1 FROM aclexplode(COALESCE(p.proacl,acldefault('f',p.proowner))) a WHERE a.grantee=0)))
FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace
WHERE n.nspname='{SCHEMA}' AND (pg_catalog.starts_with(p.proname,'waddle_ingress_') OR p.proname='pending_notification_recovery_ordinal_immutable');
SELECT jsonb_agg(jsonb_build_object('table',c.relname,'function',p.proname,
  'properties',(to_jsonb(t)-ARRAY['oid','tgrelid','tgfoid','tgconstraint','tgconstrrelid','tgconstrindid','tgparentid'])))
FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid JOIN pg_namespace n ON n.oid=c.relnamespace
JOIN pg_proc p ON p.oid=t.tgfoid
WHERE n.nspname='{SCHEMA}' AND pg_catalog.starts_with(c.relname,'ingress_') AND NOT t.tgisinternal;
SET search_path=pg_catalog,pg_temp;
"""
    tables = (
        "ARRAY(SELECT relname::text FROM pg_class "
        f"WHERE relnamespace='{SCHEMA}'::regnamespace AND relkind='r' "
        "AND (pg_catalog.starts_with(relname,'ingress_') OR relname IN ('sessions','pending_delivery')))"
    )
    sql += sources["structure_query"].replace("$1", f"'{SCHEMA}'").replace("$2", tables)
    sql += ";\nROLLBACK;\n"
    environment = os.environ.copy()
    process = subprocess.run(
        [environment.get("PSQL", "psql"), "-X", "-qAt", "-v", "ON_ERROR_STOP=1", "--dbname", environment["WADDLE_TEST_POSTGRES_URL"]],
        input=sql, text=True, capture_output=True, env=environment, check=False,
    )
    if process.returncode:
        raise SystemExit(process.stderr)
    lines = process.stdout.strip().splitlines()
    structure, triggers, functions = (json.loads(lines[-i]) for i in [1, 2, 3])
    for function in functions:
        body = function.pop("source")
        found = False
        for key, source in sources.items():
            pattern = (
                r"\nAS \$function\$(.*?)\$function\$;\n"
                if key == "baseline" else r"AS \$\$(.*?)\$\$"
            )
            for match in re.finditer(pattern, source, re.S):
                if render(match[1]) == body:
                    function.update(
                        source_file=key,
                        source_start=len(source[:match.start(1)].encode()),
                        source_end=len(source[:match.end(1)].encode()),
                    )
                    found = True
                    break
            if found:
                break
        if not found:
            raise ValueError(f"exact source body not found: {function['name']}")
    return {
        "version": VERSION,
        "postgres_major": 17,
        "source_sha256": {key: hashlib.sha256(value.encode()).hexdigest() for key, value in sources.items()},
        "functions": sorted(functions, key=lambda value: value["name"]),
        "triggers": sorted(triggers, key=lambda value: (value["table"], value["properties"]["tgname"])),
        "structure": json.loads(json.dumps(structure).replace(SCHEMA, "__SCHEMA_NAME__")),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="compare without rewriting the manifest")
    arguments = parser.parse_args()
    manifest = produce({key: path.read_text() for key, path in FILES.items()})
    encoded = json.dumps(manifest, indent=2) + "\n"
    if arguments.check:
        if OUTPUT.read_text() != encoded:
            raise SystemExit("restricted ingress manifest differs")
    else:
        OUTPUT.write_text(encoded)
    print(f"PG17 restricted ingress v{VERSION}: {len(manifest['functions'])} functions, {len(manifest['structure'])} tables")


if __name__ == "__main__":
    main()
