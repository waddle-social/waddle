#!/usr/bin/env python3
"""Contract tests for the canonical catalog dump and its comparison.

The SQLite cases always run. The PostgreSQL cases need a disposable cluster:
they run when `WADDLE_BASELINE_ADMIN_URL` holds a superuser URL and `psql` is
available (`WADDLE_PSQL` or `PATH`), and are skipped otherwise. They create
and drop databases and roles named `waddle_baseline_contract_*`.
"""
import importlib.util
import os
from pathlib import Path
import sqlite3
import sys
import tempfile
import unittest
import urllib.parse
import uuid

SCRIPTS = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPTS))


def _load(name):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


catalog = _load("schema_catalog")

SQLITE_DIRECT = """
CREATE TABLE _migrations (version INTEGER PRIMARY KEY, description TEXT NOT NULL);
CREATE TABLE rooms (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL DEFAULT 'a  ''quoted''  name',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    topic TEXT
);
CREATE TABLE members (
    room_id TEXT NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    jid TEXT NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('owner', 'member')),
    PRIMARY KEY (room_id, jid)
) WITHOUT ROWID;
CREATE INDEX idx_members_jid ON members (jid) WHERE role = 'owner';
CREATE TRIGGER rooms_touch AFTER UPDATE ON rooms BEGIN
    UPDATE rooms SET created_at = datetime('now') WHERE id = NEW.id;
END;
CREATE VIEW owners AS SELECT room_id, jid FROM members WHERE role = 'owner';
"""

# The same schema reached another way: other creation order, other layout,
# a comment, and a column added by ALTER TABLE.
SQLITE_MIGRATED = """
CREATE TABLE members (room_id TEXT NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
  jid TEXT NOT NULL, -- the bare JID
  role TEXT NOT NULL CHECK (role IN ('owner', 'member')),
  PRIMARY KEY (room_id, jid)) WITHOUT ROWID;
CREATE VIEW owners AS SELECT room_id, jid FROM members WHERE role = 'owner';
CREATE TABLE rooms (id TEXT PRIMARY KEY,
    name TEXT NOT NULL DEFAULT 'a  ''quoted''  name',
    created_at TEXT NOT NULL DEFAULT (datetime('now')));
ALTER TABLE rooms ADD COLUMN topic TEXT;
CREATE TRIGGER rooms_touch AFTER UPDATE ON rooms BEGIN
    UPDATE rooms SET created_at = datetime('now') WHERE id = NEW.id;
END;
CREATE INDEX idx_members_jid ON members (jid) WHERE role = 'owner';
CREATE TABLE _migrations (version INTEGER PRIMARY KEY, description TEXT NOT NULL);
"""


def sqlite_document(directory, name, script):
    path = Path(directory) / name
    connection = sqlite3.connect(path)
    connection.executescript(script)
    connection.commit()
    connection.close()
    return catalog.dump_sqlite(str(path), row_counts=True)


class SqliteCatalogTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)

    def document(self, name, script):
        return sqlite_document(self.directory.name, name, script)

    def test_the_same_schema_reached_two_ways_compares_identical(self):
        direct = self.document("direct.db", SQLITE_DIRECT)
        migrated = self.document("migrated.db", SQLITE_MIGRATED)
        self.assertEqual(catalog.compare(direct, migrated), [])
        self.assertEqual(
            catalog.render({**direct, "sources": {}}),
            catalog.render({**migrated, "sources": {}}),
        )

    def test_dump_describes_columns_indexes_and_foreign_keys(self):
        objects = self.document("direct.db", SQLITE_DIRECT)["objects"]
        self.assertTrue(objects["table members"]["without_rowid"])
        self.assertEqual(
            objects["table members"]["foreign_keys"],
            [{"columns": ["room_id"], "match": "NONE", "on_delete": "CASCADE",
              "on_update": "NO ACTION", "references": "rooms",
              "referenced_columns": ["id"]}],
        )
        self.assertEqual(objects["column rooms.topic"]["position"], 4)
        self.assertEqual(objects["column rooms.name"]["default"], "'a  ''quoted''  name'")
        self.assertTrue(objects["index idx_members_jid"]["partial"])
        self.assertEqual(objects["index sqlite_autoindex_rooms_1"]["origin"], "pk")

    def test_every_kind_of_change_is_reported(self):
        direct = self.document("direct.db", SQLITE_DIRECT)
        changed = self.document(
            "changed.db",
            SQLITE_DIRECT.replace("topic TEXT", "topic TEXT DEFAULT ''")
            .replace("WHERE role = 'owner';\nCREATE TRIGGER", "WHERE role = 'member';\nCREATE TRIGGER")
            .replace("CREATE VIEW owners AS SELECT room_id, jid FROM members WHERE role = 'owner';", "")
            + "CREATE INDEX idx_rooms_name ON rooms (name);",
        )
        lines = catalog.compare(direct, changed)
        text = "\n".join(lines)
        self.assertIn("~ column rooms.topic", text)
        self.assertIn("~ index idx_members_jid", text)
        self.assertIn("- view owners   (only in A)", text)
        self.assertIn("+ index idx_rooms_name   (only in B)", text)
        self.assertEqual(catalog.summarise(lines), "3 only in A, 1 only in B, 3 differ")

    def test_column_order_is_schema(self):
        first = self.document("first.db", "CREATE TABLE t (a TEXT, b TEXT);")
        second = self.document("second.db", "CREATE TABLE t (b TEXT, a TEXT);")
        self.assertIn("~ column t.a", catalog.compare(first, second))

    def test_normalisation_keeps_quoted_text_and_drops_layout(self):
        self.assertEqual(
            catalog.normalise_sqlite_sql(
                "CREATE TABLE t (\n  a TEXT DEFAULT 'x  --  y' , -- note\n  \"b  c\" INT /* gone */\n)"
            ),
            "CREATE TABLE t (a TEXT DEFAULT 'x  --  y', \"b  c\" INT)",
        )

    def test_row_counts_are_compared_only_on_request(self):
        empty = self.document("empty.db", "CREATE TABLE t (a TEXT);")
        filled = self.document("filled.db", "CREATE TABLE t (a TEXT); INSERT INTO t VALUES ('x');")
        self.assertEqual(catalog.compare(empty, filled), [])
        self.assertEqual(
            catalog.compare(empty, filled, include_row_counts=True),
            ["~ rows t: A has 0, B has 1"],
        )


class DocumentTests(unittest.TestCase):
    def document(self):
        acl = [["runtime", "SELECT", False, "owner"]]
        return {
            "format": catalog.FORMAT,
            "engine": "postgresql",
            "environment": {},
            "row_counts": {"public.t": 1, "other.u": 2},
            "objects": {
                "schema public": {"name": "public", "owner": "owner", "acl": []},
                "schema other": {"name": "other", "owner": "definer", "acl": []},
                "table public.t": {"schema": "public", "name": "t", "owner": "owner", "acl": acl},
                "table other.u": {"schema": "other", "name": "u", "owner": "definer", "acl": []},
                "database current": {"owner": "owner", "acl": [], "settings": []},
                "role owner": {"name": "owner", "member_of": []},
                "role runtime": {"name": "runtime", "member_of": [{"role": "group"}]},
                "role group": {"name": "group", "member_of": []},
                "role definer": {"name": "definer", "member_of": []},
            },
        }

    def test_only_schema_keeps_the_schema_and_the_roles_it_names(self):
        restricted = catalog.only_schema(self.document(), "public")
        self.assertEqual(
            sorted(restricted["objects"]),
            ["database current", "role group", "role owner", "role runtime",
             "schema public", "table public.t"],
        )
        self.assertEqual(restricted["row_counts"], {"public.t": 1})

    def test_structure_only_drops_owners_privileges_and_roles(self):
        structure = catalog.structure_only(self.document())
        self.assertEqual(structure["objects"]["table public.t"], {"schema": "public", "name": "t"})
        self.assertFalse([key for key in structure["objects"] if key.startswith(("role", "database"))])

    def test_list_properties_are_reported_by_element(self):
        left, right = self.document(), self.document()
        right["objects"]["table public.t"]["acl"] = [["runtime", "INSERT", False, "owner"]]
        self.assertEqual(
            catalog.compare(left, right),
            ["~ table public.t", "    acl:",
             '      A only: ["runtime", "SELECT", false, "owner"]',
             '      B only: ["runtime", "INSERT", false, "owner"]'],
        )

    def test_engines_are_not_compared_with_each_other(self):
        other = {**self.document(), "engine": "sqlite"}
        self.assertEqual(
            catalog.compare(self.document(), other),
            ["! engine: A is postgresql, B is sqlite"],
        )

    def test_a_connection_url_becomes_client_environment(self):
        self.assertEqual(
            catalog.url_environment("postgres://user%40x:p%2Fw@db.example:6432/app?sslmode=require"),
            {"PGHOST": "db.example", "PGPORT": "6432", "PGUSER": "user@x",
             "PGPASSWORD": "p/w", "PGDATABASE": "app", "PGSSLMODE": "require"},
        )
        with self.assertRaises(catalog.CatalogError):
            catalog.url_environment("postgres://db.example/app?unknown=1")



POSTGRES_OBJECTS = """
CREATE TYPE mood AS ENUM ('sad', 'ok', 'happy');
CREATE SEQUENCE epoch_seq AS bigint MINVALUE 0 START WITH 1;
CREATE FUNCTION touch() RETURNS trigger LANGUAGE plpgsql
    SET search_path TO pg_catalog AS $body$
BEGIN
    NEW.touched_at := now();
    RETURN NEW;
END
$body$;
CREATE FUNCTION room_count() RETURNS bigint LANGUAGE sql STABLE SECURITY DEFINER
    SET search_path TO pg_catalog, pg_temp AS $body$ SELECT 1::bigint $body$;
"""

POSTGRES_DIRECT = POSTGRES_OBJECTS + """
CREATE TABLE rooms (
    id BIGSERIAL PRIMARY KEY,
    name TEXT NOT NULL,
    "user" TEXT,
    mood mood NOT NULL DEFAULT 'ok',
    ordinal BIGINT GENERATED ALWAYS AS IDENTITY,
    name_length INTEGER GENERATED ALWAYS AS (length(name)) STORED,
    touched_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT rooms_name_key UNIQUE (name),
    CONSTRAINT rooms_name_check CHECK (name <> '')
);
CREATE TABLE members (
    room_id BIGINT NOT NULL REFERENCES rooms (id) ON DELETE CASCADE,
    jid TEXT NOT NULL,
    PRIMARY KEY (room_id, jid)
);
CREATE INDEX idx_members_jid ON members (lower(jid)) WHERE jid <> '';
CREATE VIEW room_names AS SELECT id, name FROM rooms;
CREATE TRIGGER rooms_touch BEFORE UPDATE ON rooms FOR EACH ROW EXECUTE FUNCTION touch();
ALTER TABLE rooms ENABLE ALWAYS TRIGGER rooms_touch;
COMMENT ON TABLE rooms IS 'it''s a room';
COMMENT ON COLUMN rooms.name IS 'display name';
"""

# The same schema in another order, with columns and constraints added later.
POSTGRES_MIGRATED = POSTGRES_OBJECTS + """
CREATE TABLE members (room_id BIGINT NOT NULL, jid TEXT NOT NULL);
CREATE TABLE rooms (id BIGSERIAL PRIMARY KEY, name TEXT NOT NULL);
ALTER TABLE rooms ADD COLUMN "user" TEXT;
ALTER TABLE rooms ADD COLUMN mood mood NOT NULL DEFAULT 'ok';
ALTER TABLE rooms ADD COLUMN ordinal BIGINT GENERATED ALWAYS AS IDENTITY;
ALTER TABLE rooms ADD COLUMN name_length INTEGER GENERATED ALWAYS AS (length(name)) STORED;
ALTER TABLE rooms ADD COLUMN touched_at TIMESTAMPTZ NOT NULL DEFAULT now();
CREATE TRIGGER rooms_touch BEFORE UPDATE ON rooms FOR EACH ROW EXECUTE FUNCTION touch();
ALTER TABLE rooms ENABLE ALWAYS TRIGGER rooms_touch;
ALTER TABLE rooms ADD CONSTRAINT rooms_name_check CHECK (name <> '');
ALTER TABLE rooms ADD CONSTRAINT rooms_name_key UNIQUE (name);
ALTER TABLE members ADD PRIMARY KEY (room_id, jid);
ALTER TABLE members ADD FOREIGN KEY (room_id) REFERENCES rooms (id) ON DELETE CASCADE;
CREATE VIEW room_names AS SELECT id, name FROM rooms;
COMMENT ON COLUMN rooms.name IS 'display name';
COMMENT ON TABLE rooms IS 'it''s a room';
CREATE INDEX idx_members_jid ON members (lower(jid)) WHERE jid <> '';
"""

POSTGRES_PRIVILEGES = """
ALTER TABLE rooms OWNER TO {owner};
ALTER TABLE members OWNER TO {owner};
ALTER VIEW room_names OWNER TO {owner};
ALTER FUNCTION touch() OWNER TO {owner};
ALTER FUNCTION room_count() OWNER TO {owner};
ALTER SEQUENCE epoch_seq OWNER TO {owner};
ALTER TYPE mood OWNER TO {owner};
ALTER SCHEMA public OWNER TO {owner};
REVOKE ALL ON SCHEMA public FROM PUBLIC;
GRANT USAGE ON SCHEMA public TO {reader};
GRANT SELECT, INSERT ON rooms TO {reader};
GRANT UPDATE (name) ON rooms TO {reader};
GRANT USAGE ON SEQUENCE rooms_id_seq TO {reader};
REVOKE ALL ON FUNCTION room_count() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION room_count() TO {reader};
"""


def _postgres_available():
    if not os.environ.get("WADDLE_BASELINE_ADMIN_URL"):
        return False
    try:
        catalog.find_psql(None)
    except catalog.CatalogError:
        return False
    return True


@unittest.skipUnless(
    _postgres_available(),
    "WADDLE_BASELINE_ADMIN_URL and psql are needed for the PostgreSQL cases",
)
class PostgresFixture(unittest.TestCase):
    """Two databases that hold one schema, built in different orders."""

    @classmethod
    def setUpClass(cls):
        cls.admin_url = os.environ["WADDLE_BASELINE_ADMIN_URL"]
        cls.psql = catalog.find_psql(None)
        cls.tag = uuid.uuid4().hex[:8]
        cls.owner = f"waddle_baseline_contract_owner_{cls.tag}"
        cls.reader = f"waddle_baseline_contract_reader_{cls.tag}"
        cls.databases = []
        for role in (cls.owner, cls.reader):
            catalog.run_psql(cls.psql, cls.admin_url, f"CREATE ROLE {role} NOLOGIN NOINHERIT")
        privileges = POSTGRES_PRIVILEGES.format(owner=cls.owner, reader=cls.reader)
        cls.direct = cls.build("direct", POSTGRES_DIRECT + privileges)
        cls.migrated = cls.build("migrated", POSTGRES_MIGRATED + privileges)

    @classmethod
    def tearDownClass(cls):
        for database in cls.databases:
            catalog.run_psql(cls.psql, cls.admin_url, f"DROP DATABASE IF EXISTS {database} WITH (FORCE)")
        for role in (cls.owner, cls.reader):
            catalog.run_psql(cls.psql, cls.admin_url, f"DROP ROLE IF EXISTS {role}")

    @classmethod
    def url(cls, database):
        parts = urllib.parse.urlsplit(cls.admin_url)
        return urllib.parse.urlunsplit((parts.scheme, parts.netloc, "/" + database, parts.query, ""))

    @classmethod
    def build(cls, label, script):
        database = f"waddle_baseline_contract_{label}_{cls.tag}"
        cls.databases.append(database)
        catalog.run_psql(cls.psql, cls.admin_url, f"CREATE DATABASE {database} TEMPLATE template0")
        catalog.run_psql(cls.psql, cls.url(database), script)
        return catalog.dump_postgres(cls.url(database), cls.psql, row_counts=True)



class PostgresCatalogTests(PostgresFixture):
    def test_the_same_schema_reached_two_ways_dumps_to_the_same_bytes(self):
        self.assertEqual(catalog.compare(self.direct, self.migrated), [])
        self.assertEqual(catalog.render(self.direct), catalog.render(self.migrated))

    def test_dump_describes_definitions_privileges_and_roles(self):
        objects = self.direct["objects"]
        function = objects["function public.room_count()"]
        self.assertTrue(function["security_definer"])
        self.assertEqual(function["volatility"], "s")
        self.assertEqual(function["config"], ["search_path=pg_catalog, pg_temp"])
        self.assertEqual(function["acl"], [
            [self.owner, "EXECUTE", False, self.owner],
            [self.reader, "EXECUTE", False, self.owner],
        ])
        self.assertEqual(objects["trigger public.rooms.rooms_touch"]["enabled"], "A")
        self.assertEqual(objects["column public.rooms.ordinal"]["identity"], "a")
        self.assertEqual(objects["column public.rooms.name_length"]["generated"], "s")
        self.assertEqual(objects["column public.rooms.name"]["acl"],
                         [[self.reader, "UPDATE", False, self.owner]])
        self.assertEqual(objects["index public.idx_members_jid"]["predicate"], "(jid <> ''::text)")
        self.assertEqual(objects["type public.mood"]["labels"], ["sad", "ok", "happy"])
        self.assertEqual(objects["sequence public.rooms_id_seq"]["owned_by"],
                         {"schema": "public", "relation": "rooms", "column": "id", "identity": False})
        self.assertEqual(objects["table public.rooms"]["comment"], "it's a room")
        self.assertIn(f"role {self.reader}", objects)
        self.assertEqual(set(objects["undescribed object counts"].values()), {0})

    def test_role_aliases_rename_owners_and_grantees(self):
        database = self.databases[0]
        aliased = catalog.dump_postgres(
            self.url(database), self.psql, role_aliases={self.owner: "owner", self.reader: "reader"})
        table = aliased["objects"]["table public.rooms"]
        self.assertEqual(table["owner"], "owner")
        self.assertIn(["reader", "SELECT", False, "owner"], table["acl"])
        self.assertIn("role reader", aliased["objects"])

    def test_structural_and_privilege_changes_are_reported_separately(self):
        changed = self.build(
            "changed",
            POSTGRES_DIRECT.replace("WHERE jid <> ''", "WHERE jid <> 'x'")
            + POSTGRES_PRIVILEGES.format(owner=self.owner, reader=self.reader)
            + f"REVOKE INSERT ON rooms FROM {self.reader};",
        )
        full = "\n".join(catalog.compare(self.direct, changed))
        self.assertIn("~ index public.idx_members_jid", full)
        self.assertIn("~ table public.rooms", full)
        structure = catalog.compare(
            catalog.structure_only(self.direct), catalog.structure_only(changed))
        self.assertEqual([line for line in structure if line.startswith("~")],
                         ["~ index public.idx_members_jid"])



if __name__ == "__main__":
    unittest.main()
