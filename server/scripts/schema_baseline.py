#!/usr/bin/env python3
"""Generate the schema baseline from a catalog document, and check it.

`generate` turns a `schema_catalog.py` document of a database the migration
history built into the files under `crates/waddle-server/sql/baseline/`.

For PostgreSQL:

  postgres.sql                 structure and static rows of the application
                               schema, as a template: it names no role, and
                               the schema's name is a placeholder
  postgres.bootstrap.sql       the lineage and ledger tables, which the
                               server creates before it applies migrations
  postgres.privileges.sql      the source's roles, owners and privileges,
                               for the round-trip check
  postgres.fingerprint.sql     the query the server and the tooling run to
                               fingerprint a schema's structure
  postgres.fingerprint.json    the source's fingerprint (`--fingerprint`)
  postgres.retired-ledger.json the source's applied migrations
                               (`--retired-ledger`)

For SQLite: `sqlite.sql`, `sqlite.bootstrap.sql`, `sqlite.fingerprint.json`
and `sqlite.retired-ledger.json`.

The server reads `postgres.sql`, `sqlite.sql`, the fingerprints and the
retired ledgers, and only when `WADDLE_DB_BASELINE_ACTION` selects the
baseline.

`round-trip` applies the generated SQL to an empty database, dumps it and
compares the dump, and the rows of the static tables, with the document the
SQL was generated from. It exits 0 when they are identical and 1 when not.

`lockstep` compares a PostgreSQL document with a SQLite document by name:
tables, columns and explicitly created indexes. Tables that one engine alone
has by design are listed in `schema-engine-differences.txt`.

The generator refuses a document that holds an object kind or property it
cannot reproduce, naming each one, so nothing is left out silently. The
tables it leaves to an owner preparation are listed in
`OWNER_PREPARED_TABLES`. The output depends only on the document.

The design and the cut procedure are in
`docs/planning/2046-schema-baseline.md`.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sqlite3
import sys
import tempfile
import urllib.parse
import uuid
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))
import schema_catalog  # noqa: E402
from schema_catalog import CatalogError, quote_literal  # noqa: E402

# Tables the server creates itself before it applies any migration: the
# lineage row's table and the migration ledger. The baseline leaves them out.
BOOTSTRAP_TABLES = ("_lineage", "_migrations")

# Tables whose rows are the same in every deployment and are written by the
# retired migrations or by a store's one-time schema step, so the baseline
# carries them. Every other table's rows come from the owner phase, from
# deployment inputs, or from serving.
STATIC_TABLES = {
    "postgresql": (
        "ingress_epoch_guard_manifest",
        "ingress_protocol_epoch",
    ),
    "sqlite": (
        "ingress_protocol_epoch",
        # Created together with its one row; the store refuses the table
        # without it.
        "pending_notification_recovery_counter",
    ),
}

# PostgreSQL tables an owner preparation creates together with the row that
# marks them prepared, and refuses when it finds the table without that row.
# The baseline leaves them, and everything that hangs off them, to their
# preparation. An entry is removed when its preparation accepts the table
# from the baseline; the list is empty when every store has been converted.
OWNER_PREPARED_TABLES = {
    "clustering_keypair_slots": "recreated under the keypair-pool enrollment",
    "clustering_keypool": "created with the enrollment row",
    "clustering_keypool_slots": "created with the enrolled slots",
    "extension_room_observation_owner_state": "created with its owner-state row",
    "notification_activity_owner_state": "created with its owner-state row",
    "notification_outbox_owner_state": "created with its owner-state row",
    "pubsub_schema_marker": "created with its marker row",
    "push_service_schema_marker": "created with its marker row",
    "push_service_vapid_schema_marker": "created with its marker row",
}

# Functions that belong to an owner-prepared table's preparation, which
# takes their presence to mean the table exists.
OWNER_PREPARED_FUNCTIONS = {
    "clustering_keypair_slot_guard": "guards the enrolled keypair-slot table",
    "clustering_keypair_truncate_guard": "guards the enrolled keypair-slot table",
    "clustering_keypool_guard": "guards the keypair-pool enrollment",
    "clustering_keypool_slots_guard": "guards the enrolled slots",
}


def baseline_scope(document: dict[str, Any], schema: str) -> dict[str, Any]:
    """The PostgreSQL document without the owner-prepared tables and functions.

    A table goes with its columns, constraints, indexes, triggers and the
    sequences its columns own.
    """
    def outside(key: str, row: dict[str, Any]) -> bool:
        if row.get("schema") != schema:
            return False
        kind = key.split(" ", 1)[0]
        if kind == "function":
            return row["name"] in OWNER_PREPARED_FUNCTIONS
        owned = row.get("owned_by") or {}
        table = {
            "table": row.get("name"), "column": row.get("relation"),
            "constraint": row.get("parent"), "index": row.get("relation"),
            "trigger": row.get("relation"), "sequence": owned.get("relation"),
        }.get(kind)
        return table in OWNER_PREPARED_TABLES

    scoped = {**document, "objects": {
        key: row for key, row in document["objects"].items() if not outside(key, row)}}
    for field in ("rows", "row_counts"):
        if field in document:
            scoped[field] = {
                name: value for name, value in document[field].items()
                if name.rsplit(".", 1)[-1] not in OWNER_PREPARED_TABLES}
    return scoped


SCHEMA_QUOTED = "__SCHEMA_QUOTED__"
SCHEMA_LITERAL = "__SCHEMA_LITERAL__"
SCHEMA_BARE = "__SCHEMA__"


def render_postgres(sql: str, schema: str) -> str:
    """Replace the schema placeholders of a PostgreSQL baseline template.

    `__SCHEMA_QUOTED__` becomes the name in double quotes, `__SCHEMA_LITERAL__`
    the name as a string literal, and `__SCHEMA__` the name as `quote_ident`
    prints it. The server renders the same three forms.
    """
    return (
        sql.replace(SCHEMA_QUOTED, schema_catalog.quote_identifier(schema))
        .replace(SCHEMA_LITERAL, "'" + schema.replace("'", "''") + "'")
        .replace(SCHEMA_BARE, ident(schema))
    )
ROUND_TRIP_PREFIX = "waddle_baseline_roundtrip_"

# Words PostgreSQL will not accept as a bare identifier (reserved, or
# reserved as a column name). `quote_ident` quotes the same set, so a name
# quoted here is quoted in the definitions the catalog functions print.
_RESERVED = frozenset(
    """all analyse analyze and any array as asc asymmetric authorization between
    bigint binary bit boolean both case cast char character check coalesce
    collate collation column concurrently constraint create cross current_catalog
    current_date current_role current_schema current_time current_timestamp
    current_user dec decimal default deferrable desc distinct do else end except
    exists extract false fetch float for foreign freeze from full grant greatest
    group grouping having ilike in initially inner inout int integer intersect
    interval into is isnull join json json_array json_arrayagg json_exists
    json_object json_objectagg json_query json_scalar json_serialize json_table
    json_value lateral leading least left like limit localtime localtimestamp
    merge_action national natural nchar none normalize not notnull null nullif
    numeric offset on only or order out outer overlaps overlay placing position
    precision primary real references returning right row select session_user
    setof similar smallint some substring symmetric system_user table
    tablesample then time timestamp to trailing treat trim true union unique
    user using values varchar variadic verbose when where window with xmlattributes
    xmlconcat xmlelement xmlexists xmlforest xmlnamespaces xmlparse xmlpi xmlroot
    xmlserialize xmltable""".split()
)
_BARE = re.compile(r"^[a-z_][a-z0-9_$]*$")


def ident(name: str) -> str:
    if _BARE.match(name) and name not in _RESERVED:
        return name
    return schema_catalog.quote_identifier(name)


# `pg_trigger.tgenabled`: fires in origin and local mode (the default),
# always, in replica mode only, or never.
_TRIGGER_STATE = {"O": "", "A": "ENABLE ALWAYS", "R": "ENABLE REPLICA", "D": "DISABLE"}


class Unsupported(CatalogError):
    """The document holds something the generator cannot reproduce."""


# ---------------------------------------------------------------------------
# PostgreSQL
# ---------------------------------------------------------------------------


class PostgresGenerator:
    def __init__(self, document: dict[str, Any], schema: str) -> None:
        if document.get("engine") != "postgresql":
            raise CatalogError("the document is not a PostgreSQL catalog document")
        self.schema = schema
        self.document = document
        self.objects: dict[str, dict[str, Any]] = document["objects"]
        self.problems: list[str] = []
        self.by_kind: dict[str, list[dict[str, Any]]] = {}
        for key in sorted(self.objects):
            row = self.objects[key]
            self.by_kind.setdefault(key.split(" ", 1)[0], []).append(row)

    # -- helpers ------------------------------------------------------------

    def kind(self, kind: str) -> list[dict[str, Any]]:
        """Objects of `kind` in the application schema, in name order."""
        return [row for row in self.by_kind.get(kind, []) if row.get("schema") == self.schema]

    def problem(self, text: str) -> None:
        self.problems.append(text)

    def strip_schema(self, definition: str, qualified: str, bare: str, what: str) -> str:
        """Replace the one schema-qualified name the catalog always prints."""
        if definition.count(qualified) < 1:
            self.problem(f"{what}: expected {qualified!r} in its definition")
            return definition
        return definition.replace(qualified, bare, 1)

    def columns_of(self, relation: str) -> list[dict[str, Any]]:
        rows = [row for row in self.kind("column") if row["relation"] == relation]
        return sorted(rows, key=lambda row: row["position"])

    def sequence_for(self, relation: str, column: str, identity: bool) -> dict[str, Any] | None:
        for row in self.kind("sequence"):
            owned = row["owned_by"]
            if (
                owned
                and owned["schema"] == self.schema
                and owned["relation"] == relation
                and owned["column"] == column
                and owned["identity"] == identity
            ):
                return row
        return None

    @staticmethod
    def sequence_options(row: dict[str, Any]) -> str:
        options = (
            f"START WITH {row['start']} INCREMENT BY {row['increment']} "
            f"MINVALUE {row['minimum']} MAXVALUE {row['maximum']} CACHE {row['cache']}"
        )
        return options + (" CYCLE" if row["cycle"] else "")

    # -- structure ----------------------------------------------------------

    def check_supported(self) -> None:
        for kind in ("materialized_view", "foreign_table", "composite_type", "policy",
                     "rule", "statistics"):
            for row in self.kind(kind):
                self.problem(f"{kind} {row['name']}: this object kind is not generated")
        for kind in ("extension", "event_trigger"):
            for row in self.by_kind.get(kind, []):
                self.problem(f"{kind} {row['name']}: this object kind is not generated")
        for row in self.kind("type"):
            if row["kind"] != "e":
                self.problem(f"type {row['name']}: only enum types are generated")
        for name, count in self.objects.get("undescribed object counts", {}).items():
            if count:
                self.problem(f"the database holds {count} {name} object(s) the dump does not describe")
        for row in self.kind("table"):
            name = row["name"]
            for field, expected in (
                ("relkind", "r"), ("persistence", "p"), ("options", None),
                ("toast_options", None), ("row_security", False),
                ("force_row_security", False), ("replica_identity", "d"),
                ("partition_key", None), ("partition_bound", None), ("inherits", None),
                ("access_method", "heap"), ("tablespace", None), ("of_type", None),
            ):
                if row[field] != expected:
                    self.problem(f"table {name}: {field} = {row[field]!r} is not generated")
        for row in self.kind("column"):
            name = f"{row['relation']}.{row['name']}"
            for field, expected in (
                ("storage", None), ("compression", None), ("statistics_target", None),
                ("options", None), ("local", True), ("inherited_from", 0),
            ):
                if row[field] != expected:
                    self.problem(f"column {name}: {field} = {row[field]!r} is not generated")
        for row in self.kind("index"):
            name = row["name"]
            for field, expected in (
                ("clustered", False), ("valid", True), ("ready", True), ("live", True),
                ("replica_identity", False), ("tablespace", None), ("partition_of", None),
            ):
                if row[field] != expected:
                    self.problem(f"index {name}: {field} = {row[field]!r} is not generated")
            if row["constraint"] is not None and (row["constraint"] != name or row["options"]):
                self.problem(f"index {name}: differs from its constraint's default index")
        for row in self.kind("constraint"):
            name = f"{row['parent']}.{row['name']}"
            if row["parent_kind"] != "table":
                self.problem(f"constraint {name}: domain constraints are not generated")
            if row["type"] not in ("c", "p", "u", "f", "x"):
                self.problem(f"constraint {name}: type {row['type']!r} is not generated")
            if not row["local"] or row["inherited_from"] or row["partition_child"]:
                self.problem(f"constraint {name}: inherited constraints are not generated")
        for row in self.kind("trigger"):
            if row["enabled"] not in _TRIGGER_STATE or row["partition_child"]:
                self.problem(f"trigger {row['relation']}.{row['name']}: state is not generated")
        for row in self.kind("function"):
            if row["kind"] not in ("f", "p") or row["definition"] is None:
                self.problem(f"function {row['name']}: kind {row['kind']!r} is not generated")
            if row["sql_body"] is not None:
                self.problem(f"function {row['name']}: SQL-standard bodies are not generated")
        for row in self.kind("sequence"):
            if row["persistence"] != "p":
                self.problem(f"sequence {row['name']}: persistence is not generated")

    def column_line(self, row: dict[str, Any]) -> str:
        parts = [ident(row["name"]), row["type"]]
        if row["collation"]:
            parts.append(f"COLLATE {ident(row['collation'][0])}.{ident(row['collation'][1])}")
        if row["identity"]:
            sequence = self.sequence_for(row["relation"], row["name"], identity=True)
            if sequence is None:
                self.problem(f"column {row['relation']}.{row['name']}: identity sequence not found")
            else:
                mode = "ALWAYS" if row["identity"] == "a" else "BY DEFAULT"
                parts.append(
                    f"GENERATED {mode} AS IDENTITY (SEQUENCE NAME {ident(sequence['name'])} "
                    f"{self.sequence_options(sequence)})"
                )
        elif row["generated"]:
            if row["generated"] != "s":
                self.problem(f"column {row['relation']}.{row['name']}: virtual generated column")
            parts.append(f"GENERATED ALWAYS AS ({row['default']}) STORED")
        elif row["default"] is not None:
            parts.append(f"DEFAULT {row['default']}")
        if row["not_null"]:
            parts.append("NOT NULL")
        return " ".join(parts)

    def structure(self) -> str:
        out: list[str] = []
        constraints = self.kind("constraint")

        enums = [row for row in self.kind("type") if row["kind"] == "e"]
        for row in enums:
            labels = ", ".join(quote_literal(label) for label in row["labels"])
            out.append(f"CREATE TYPE {ident(row['name'])} AS ENUM ({labels});\n")

        sequences = [
            row for row in self.kind("sequence")
            if not (row["owned_by"] and row["owned_by"]["identity"])
        ]
        for row in sequences:
            out.append(
                f"CREATE SEQUENCE {ident(row['name'])} AS {row['type']} "
                f"{self.sequence_options(row)};\n"
            )

        # Functions written in a procedural language are not checked against
        # the tables they name, and triggers, defaults and constraints may
        # call them, so they precede the tables. SQL-language bodies are
        # checked when they are created, so they follow.
        functions = self.kind("function")
        early = [row for row in functions if row["language"] != "sql"]
        late = [row for row in functions if row["language"] == "sql"]
        for row in early:
            out.append(self.function(row))

        for table in self.kind("table"):
            if table["name"] in BOOTSTRAP_TABLES:
                continue
            out.append(self.table(table, constraints))

        foreign = [row for row in constraints if row["type"] == "f" or not row["validated"]]
        for row in foreign:
            if row["parent"] in BOOTSTRAP_TABLES:
                continue
            out.append(
                f"ALTER TABLE {ident(row['parent'])} ADD CONSTRAINT {ident(row['name'])} "
                f"{row['definition']};\n"
            )
        if foreign:
            out.append("")

        for row in self.kind("index"):
            if row["constraint"] is not None or row["relation"] in BOOTSTRAP_TABLES:
                continue
            qualified = f" ON {ident(self.schema)}.{ident(row['relation'])} "
            bare = f" ON {ident(row['relation'])} "
            out.append(self.strip_schema(row["definition"], qualified, bare,
                                         f"index {row['name']}") + ";\n")

        # Before the triggers, so a guard that refuses writes does not see them.
        out.extend(self.static_rows())

        for row in self.views():
            out.append(f"CREATE VIEW {ident(row['name'])} AS\n{row['definition']}\n")

        for row in late:
            out.append(self.function(row))

        for row in self.kind("trigger"):
            qualified = f" ON {ident(self.schema)}.{ident(row['relation'])} "
            bare = f" ON {ident(row['relation'])} "
            statement = self.strip_schema(row["definition"], qualified, bare,
                                          f"trigger {row['name']}") + ";\n"
            state = _TRIGGER_STATE.get(row["enabled"])
            if state:
                statement += (f"ALTER TABLE {ident(row['relation'])} {state} TRIGGER "
                              f"{ident(row['name'])};\n")
            out.append(statement)

        for row in sequences:
            owned = row["owned_by"]
            if owned:
                out.append(
                    f"ALTER SEQUENCE {ident(row['name'])} OWNED BY "
                    f"{ident(owned['relation'])}.{ident(owned['column'])};\n"
                )

        out.extend(self.role_free_privileges())
        out.extend(self.comments())
        return "\n".join(out)

    def role_free_privileges(self) -> list[str]:
        """Privileges the retired migrations set that name no deployment role.

        Those are grants to PUBLIC and to PostgreSQL's predefined roles, and
        the withdrawal of a function's or type's default PUBLIC privilege.
        Preparations that run after the migrations verify them, so they are
        part of the structure. Privileges of deployment roles are granted by
        the owner phase and are not written here.
        """
        out = []
        defaults = {"FUNCTION": "EXECUTE", "PROCEDURE": "EXECUTE", "TYPE": "USAGE"}

        def grants(target: str, word: str, row: dict[str, Any], column: str | None = None) -> None:
            builtin = [entry for entry in row["acl"]
                       if entry[0] == "PUBLIC" or entry[0].startswith("pg_")]
            if column is None and word in defaults and not any(
                    entry[0] == "PUBLIC" and entry[1] == defaults[word] for entry in row["acl"]):
                out.append(f"REVOKE ALL ON {target} FROM PUBLIC;\n")
            for grantee, privilege, grantable, _ in builtin:
                if column is None and grantee == "PUBLIC" and defaults.get(word) == privilege:
                    continue
                name = f"{privilege} ({ident(column)})" if column else privilege
                out.append(
                    f"GRANT {name} ON {target} TO {grantee}"
                    f"{' WITH GRANT OPTION' if grantable else ''};\n")

        for kind, word in (("table", "TABLE"), ("view", "TABLE"), ("sequence", "SEQUENCE"),
                           ("type", "TYPE")):
            for row in self.kind(kind):
                if row["name"] not in BOOTSTRAP_TABLES:
                    grants(f"{word} {ident(row['name'])}", word, row)
        for row in self.kind("column"):
            if row["acl"] and row["relation"] not in BOOTSTRAP_TABLES:
                grants(f"TABLE {ident(row['relation'])}", "TABLE", row, row["name"])
        for row in self.kind("function"):
            word = "PROCEDURE" if row["kind"] == "p" else "FUNCTION"
            grants(f"{word} {ident(row['name'])}({row['identity_arguments']})", word, row)
        return out

    def table(self, table: dict[str, Any], constraints: list[dict[str, Any]]) -> str:
        name = table["name"]
        lines = [self.column_line(row) for row in self.columns_of(name)]
        order = {"p": 0, "u": 1, "x": 2, "c": 3}
        inline = [
            row for row in constraints
            if row["parent"] == name and row["type"] in order and row["validated"]
        ]
        for row in sorted(inline, key=lambda row: (order[row["type"]], row["name"])):
            lines.append(f"CONSTRAINT {ident(row['name'])} {row['definition']}")
        body = ",\n".join("    " + line for line in lines)
        return f"CREATE TABLE {ident(name)} (\n{body}\n);\n"

    def function(self, row: dict[str, Any]) -> str:
        prefix = f"CREATE OR REPLACE FUNCTION {ident(self.schema)}.{ident(row['name'])}("
        if row["kind"] == "p":
            prefix = f"CREATE OR REPLACE PROCEDURE {ident(self.schema)}.{ident(row['name'])}("
        definition = row["definition"] or ""
        if not definition.startswith(prefix):
            self.problem(f"function {row['name']}: unexpected definition header")
            return ""
        bare = prefix.replace("CREATE OR REPLACE ", "CREATE ").replace(
            f"{ident(self.schema)}.", "", 1
        )
        text = bare + definition[len(prefix):].rstrip() + ";\n"
        # A function that spells out the schema's name is written with
        # placeholders, so the baseline can be rendered for any schema.
        if self.names_deployment("function", row, ("source", "config")):
            text = self.template(text)
        return text

    def views(self) -> list[dict[str, Any]]:
        """Views ordered so that each follows the views it selects from."""
        pending = {row["name"]: row for row in self.kind("view")}
        ordered: list[dict[str, Any]] = []
        while pending:
            ready = sorted(
                name for name, row in pending.items()
                if not any(
                    schema == self.schema and other in pending and other != name
                    for schema, other in row["depends_on"]
                )
            )
            if not ready:
                self.problem(f"views {sorted(pending)} depend on each other")
                break
            for name in ready:
                ordered.append(pending.pop(name))
        return ordered

    def comments(self) -> list[str]:
        out: list[str] = []

        def comment(target: str, text: str | None) -> None:
            if text is not None:
                out.append(f"COMMENT ON {target} IS {quote_literal(text)};\n")

        for kind, word in (("table", "TABLE"), ("view", "VIEW"), ("sequence", "SEQUENCE"),
                           ("index", "INDEX"), ("type", "TYPE")):
            for row in self.kind(kind):
                if row["name"] not in BOOTSTRAP_TABLES:
                    comment(f"{word} {ident(row['name'])}", row["comment"])
        for row in self.kind("column"):
            comment(f"COLUMN {ident(row['relation'])}.{ident(row['name'])}", row["comment"])
        for row in self.kind("constraint"):
            comment(f"CONSTRAINT {ident(row['name'])} ON {ident(row['parent'])}", row["comment"])
        for row in self.kind("trigger"):
            comment(f"TRIGGER {ident(row['name'])} ON {ident(row['relation'])}", row["comment"])
        for row in self.kind("function"):
            comment(f"FUNCTION {ident(row['name'])}({row['identity_arguments']})", row["comment"])
        return out

    def bootstrap(self) -> str:
        constraints = self.kind("constraint")
        out = []
        for table in self.kind("table"):
            if table["name"] not in BOOTSTRAP_TABLES:
                continue
            out.append(self.table(table, constraints))
            for row in self.kind("index"):
                if row["relation"] == table["name"] and row["constraint"] is None:
                    qualified = f" ON {ident(self.schema)}.{ident(table['name'])} "
                    out.append(self.strip_schema(row["definition"], qualified,
                                                 f" ON {ident(table['name'])} ",
                                                 f"index {row['name']}") + ";\n")
        return "\n".join(out)

    def static_rows(self) -> list[str]:
        """One statement per static table that holds rows in the source."""
        out = []
        rows = self.document.get("rows") or {}
        for table in STATIC_TABLES["postgresql"]:
            if f"table {self.schema}.{table}" not in self.objects:
                continue
            if table not in rows:
                self.problem(f"table {table}: the document was dumped without its rows")
                continue
            if any(row["identity"] == "a" for row in self.columns_of(table)):
                self.problem(f"table {table}: rows of an ALWAYS identity table are not generated")
            if rows[table]:
                literal = quote_literal(json.dumps(rows[table], sort_keys=True, ensure_ascii=False))
                out.append(
                    f"INSERT INTO {ident(table)}\n"
                    f"SELECT * FROM jsonb_populate_recordset(NULL::{ident(table)},\n"
                    f"    {literal}::jsonb);\n"
                )
        return out

    def template(self, text: str) -> str:
        """Replace the source schema's name, in its three spellings."""
        name = self.schema
        text = text.replace(schema_catalog.quote_identifier(name), SCHEMA_QUOTED)
        text = text.replace("'" + name.replace("'", "''") + "'", SCHEMA_LITERAL)
        return re.sub(
            rf"(?<![A-Za-z0-9_$]){re.escape(name)}(?![A-Za-z0-9_$])", SCHEMA_BARE, text)

    # -- privileges ---------------------------------------------------------

    def acl_statements(self, target: str, owner: str, acl: list[list[Any]],
                       column: str | None = None) -> list[str]:
        """Statements that leave `target` with exactly the privileges in `acl`."""
        out: list[str] = []
        if column is None:
            out.append(f"REVOKE ALL ON {target} FROM PUBLIC;")
            out.append(f"REVOKE ALL ON {target} FROM {ident(owner)};")
        grouped: dict[tuple[str, str, bool], list[str]] = {}
        for grantee, privilege, grantable, grantor in acl:
            grouped.setdefault((grantor, grantee, grantable), []).append(privilege)
        for (grantor, grantee, grantable), privileges in sorted(grouped.items()):
            names = sorted(privileges)
            if column is not None:
                names = [f"{name} ({ident(column)})" for name in names]
            statement = (
                f"GRANT {', '.join(names)} ON {target} TO "
                f"{'PUBLIC' if grantee == 'PUBLIC' else ident(grantee)}"
                f"{' WITH GRANT OPTION' if grantable else ''};"
            )
            if grantor != owner:
                statement = f"SET ROLE {ident(grantor)}; {statement} RESET ROLE;"
            out.append(statement)
        return out

    def privileges(self) -> str:
        out: list[str] = []
        roles = [
            row for row in self.by_kind.get("role", [])
            if not row["name"].startswith("pg_") and not row["superuser"]
        ]
        out.append("-- Roles. An existing role is left as it is.")
        for row in roles:
            attributes = " ".join(
                (word if row[field] else "NO" + word)
                for field, word in (
                    ("login", "LOGIN"), ("inherit", "INHERIT"), ("create_role", "CREATEROLE"),
                    ("create_database", "CREATEDB"), ("replication", "REPLICATION"),
                    ("bypass_rls", "BYPASSRLS"),
                )
            )
            if row["config"]:
                self.problem(f"role {row['name']}: role settings are not generated")
            out.append(
                "DO $role$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = "
                f"{quote_literal(row['name'])}) THEN CREATE ROLE {ident(row['name'])} "
                f"NOSUPERUSER {attributes} CONNECTION LIMIT {row['connection_limit']}; "
                "END IF; END $role$;"
            )
        for row in roles:
            for membership in row["member_of"]:
                flags = ", ".join(
                    f"{word} {'TRUE' if membership[field] else 'FALSE'}"
                    for field, word in (("inherit", "INHERIT"), ("set", "SET"), ("admin", "ADMIN"))
                )
                out.append(
                    f"GRANT {ident(membership['role'])} TO {ident(row['name'])} WITH {flags};"
                )

        database = self.objects.get("database current")
        if database:
            out.append("\n-- The database.")
            if database["settings"]:
                self.problem("database: per-database settings are not generated")
            out.append(
                "DO $database$ BEGIN EXECUTE pg_catalog.format('ALTER DATABASE %I OWNER TO %I', "
                f"pg_catalog.current_database(), {quote_literal(database['owner'])}); "
                "END $database$;"
            )
            # A database name cannot be a parameter, so the statements are
            # written for a placeholder and run through `format`.
            for statement in self.acl_statements("DATABASE %I", database["owner"], database["acl"]):
                out.append(
                    f"DO $database$ BEGIN EXECUTE pg_catalog.format({quote_literal(statement)}, "
                    "pg_catalog.current_database()); END $database$;"
                )

        out.append("\n-- The application schema.")
        for row in self.by_kind.get("schema", []):
            if row["name"] != self.schema:
                continue
            target = f"SCHEMA {ident(row['name'])}"
            out.append(f"ALTER {target} OWNER TO {ident(row['owner'])};")
            out.extend(self.acl_statements(target, row["owner"], row["acl"]))

        out.append("\n-- Owners. An owned or identity sequence follows its table.")
        for kind, word in (("table", "TABLE"), ("view", "VIEW")):
            for row in self.kind(kind):
                out.append(f"ALTER {word} {ident(row['name'])} OWNER TO {ident(row['owner'])};")
        for row in self.kind("sequence"):
            if not row["owned_by"]:
                out.append(f"ALTER SEQUENCE {ident(row['name'])} OWNER TO {ident(row['owner'])};")
        for row in self.kind("type"):
            out.append(f"ALTER TYPE {ident(row['name'])} OWNER TO {ident(row['owner'])};")
        for row in self.kind("function"):
            out.append(
                f"ALTER {'PROCEDURE' if row['kind'] == 'p' else 'FUNCTION'} "
                f"{ident(row['name'])}({row['identity_arguments']}) OWNER TO {ident(row['owner'])};"
            )

        out.append("\n-- Privileges.")
        for kind, word in (("table", "TABLE"), ("view", "TABLE"), ("sequence", "SEQUENCE"),
                           ("type", "TYPE")):
            for row in self.kind(kind):
                out.extend(self.acl_statements(f"{word} {ident(row['name'])}",
                                               row["owner"], row["acl"]))
        relation_owner = {row["name"]: row["owner"]
                          for kind in ("table", "view") for row in self.kind(kind)}
        for row in self.kind("column"):
            if row["acl"]:
                out.extend(self.acl_statements(
                    f"TABLE {ident(row['relation'])}", relation_owner[row["relation"]],
                    row["acl"], column=row["name"]))
        for row in self.kind("function"):
            target = (f"{'PROCEDURE' if row['kind'] == 'p' else 'FUNCTION'} "
                      f"{ident(row['name'])}({row['identity_arguments']})")
            out.extend(self.acl_statements(target, row["owner"], row["acl"]))

        system = self.by_kind.get("system_privilege", [])
        if system:
            out.append("\n-- Privileges on system objects.")
        for row in system:
            word = "FUNCTION" if row["class"] == "function" else "TABLE"
            out.extend(self.acl_statements(f"{word} {row['name']}", row["owner"], row["acl"]))
        for row in self.by_kind.get("default_acl", []):
            self.problem(f"default privileges of {row['role']} are not generated")
        return "\n".join(out) + "\n"

    def names_deployment(self, kind: str, row: dict[str, Any], fields: tuple[str, ...]) -> list[str]:
        """The deployment names (schema, roles) a definition spells out."""
        names = {self.schema: "schema"}
        for role in self.by_kind.get("role", []):
            names.setdefault(role["name"], "role")
        text = "\n".join(str(row[field]) for field in fields if row[field])
        if kind in ("trigger", "index"):
            text = text.replace(f" ON {ident(self.schema)}.{ident(row['relation'])} ", " ", 1)
        return sorted(
            f"{what} {name}" for name, what in names.items()
            if re.search(rf"(?<![A-Za-z0-9_]){re.escape(name)}(?![A-Za-z0-9_])", text)
        )

    def check_deployment_names(self) -> list[str]:
        """Report the templated functions; refuse any other deployment name.

        A function may name the schema: it is written as a template. A role
        name anywhere, or the schema's name in a trigger, constraint,
        default, index or view, has no rendering and is refused.
        """
        templated = []
        fields = {
            "function": ("source", "config"),
            "trigger": ("definition",),
            "constraint": ("definition",),
            "column": ("default",),
            "index": ("definition",),
            "view": ("definition",),
        }
        for kind, checked in fields.items():
            for row in self.kind(kind):
                hits = self.names_deployment(kind, row, checked)
                if not hits:
                    continue
                label = row["name"] if "relation" not in row else (
                    f"{row['relation']}.{row['name']}")
                if kind == "function" and hits == [f"schema {self.schema}"]:
                    templated.append(f"function {label}")
                else:
                    self.problem(f"{kind} {label} names {', '.join(hits)}")
        return templated


def generate_postgres(document: dict[str, Any], schema: str) -> dict[str, Any]:
    document = baseline_scope(document, schema)
    generator = PostgresGenerator(document, schema)
    generator.check_supported()
    templated = generator.check_deployment_names()
    structure = generator.structure()
    bootstrap = generator.bootstrap()
    privileges = generator.privileges()
    if generator.problems:
        raise Unsupported(
            "the document cannot be reproduced:\n  " + "\n  ".join(generator.problems)
        )
    outside = sorted(
        key for key, row in document["objects"].items()
        if row.get("schema") not in (None, schema)
        or (key.startswith("schema ") and row["name"] != schema)
    )
    return {
        "files": {
            "postgres.sql": structure,
            "postgres.bootstrap.sql": bootstrap,
            "postgres.privileges.sql": privileges,
            "postgres.fingerprint.sql": schema_catalog.fingerprint_sql(),
        },
        "outside": outside,
        "names": templated,
    }


# ---------------------------------------------------------------------------
# SQLite
# ---------------------------------------------------------------------------


def generate_sqlite(document: dict[str, Any]) -> dict[str, Any]:
    if document.get("engine") != "sqlite":
        raise CatalogError("the document is not a SQLite catalog document")
    sources: dict[str, str] = document["sources"]
    objects: dict[str, dict[str, Any]] = document["objects"]
    problems = [
        f"{key}: virtual tables are not generated"
        for key, row in objects.items()
        if key.startswith("table ") and row.get("virtual")
    ]
    if problems:
        raise Unsupported("the document cannot be reproduced:\n  " + "\n  ".join(problems))
    structure: list[str] = []
    bootstrap: list[str] = []
    rows = document.get("rows") or {}
    # A table precedes its indexes, and views and triggers follow every
    # table. SQLite does not resolve the tables a foreign key, a view or a
    # trigger names until they are used.
    # Static rows precede the triggers.
    for kind in ("table", "index", "rows", "view", "trigger"):
        if kind == "rows":
            for table in STATIC_TABLES["sqlite"]:
                if f"table {table}" not in objects:
                    continue
                if table not in rows:
                    raise Unsupported(f"table {table}: the document was dumped without its rows")
                for row in rows[table]:
                    columns = ", ".join(schema_catalog.quote_identifier(name) for name in row)
                    structure.append(
                        f"INSERT INTO {schema_catalog.quote_identifier(table)} ({columns}) "
                        f"VALUES ({', '.join(row.values())});\n")
            continue
        for key in sorted(key for key in sources if key.startswith(kind + " ")):
            row = objects[key]
            owner = row["name"] if kind in ("table", "view") else row["relation"]
            (bootstrap if owner in BOOTSTRAP_TABLES else structure).append(
                sources[key].rstrip() + ";\n")
    pragma = objects.get("pragma schema", {})
    for name in ("user_version", "application_id"):
        if pragma.get(name):
            structure.append(f"PRAGMA {name} = {pragma[name]};\n")
    return {
        "files": {"sqlite.sql": "\n".join(structure), "sqlite.bootstrap.sql": "\n".join(bootstrap)},
        "outside": [],
        "names": [],
    }


# ---------------------------------------------------------------------------
# Files
# ---------------------------------------------------------------------------

_HEADERS = {
    "postgres.sql": "Structure and static rows of the application schema, as a template:\n"
                    "-- __SCHEMA__, __SCHEMA_QUOTED__ and __SCHEMA_LITERAL__ stand for the schema's\n"
                    "-- name. Run it rendered, with that schema first on the search path, as the\n"
                    "-- role that is to own the objects.",
    "postgres.bootstrap.sql": "The lineage and migration ledger tables, which the server creates\n"
                              "-- itself before it applies a migration.",
    "postgres.fingerprint.sql": "The query that fingerprints the structure of the schema on the\n"
                                "-- search path. The server runs it before it stamps a database.",
    "postgres.privileges.sql": "Roles, owners and privileges of the source database. Run as a\n"
                               "-- superuser after the structure, for the round-trip check only.",
    "sqlite.sql": "Structure and static rows of the SQLite database.",
    "sqlite.bootstrap.sql": "The lineage and migration ledger tables, which the server creates\n"
                            "-- itself before it applies a migration.",
}


def header(name: str, commit: str, source: str) -> str:
    return (
        "-- GENERATED by server/scripts/schema_baseline.py. Do not edit.\n"
        f"-- Source commit: {commit}\n"
        f"-- Source catalog: {source}\n"
        "--\n"
        "-- Part of the schema baseline (docs/planning/2046-schema-baseline.md). The\n"
        "-- server uses the baseline only when WADDLE_DB_BASELINE_ACTION selects it;\n"
        "-- otherwise the migration history in src/db/migrations is the catalog.\n"
        "--\n"
        f"-- {_HEADERS[name]}\n\n"
    )


def generate(document: dict[str, Any], schema: str, commit: str, source: str) -> dict[str, Any]:
    result = (
        generate_postgres(document, schema)
        if document.get("engine") == "postgresql"
        else generate_sqlite(document)
    )
    result["files"] = {
        name: header(name, commit, source) + text for name, text in result["files"].items()
    }
    result["engine"] = document.get("engine")
    return result


# ---------------------------------------------------------------------------
# Round trip
# ---------------------------------------------------------------------------


_CREATED_ROLE = re.compile(r"WHERE rolname = '([^']+)'\) THEN CREATE ROLE ")


def _read(directory: Path, name: str) -> str:
    try:
        return (directory / name).read_text(encoding="utf-8")
    except OSError as error:
        raise CatalogError(f"could not read {directory / name}: {error}") from error


def round_trip_sqlite(directory: Path) -> dict[str, Any]:
    with tempfile.TemporaryDirectory(prefix="waddle-baseline-") as work:
        path = Path(work) / "candidate.db"
        connection = sqlite3.connect(path)
        try:
            connection.executescript(_read(directory, "sqlite.bootstrap.sql"))
            connection.executescript(_read(directory, "sqlite.sql"))
            connection.commit()
        except sqlite3.DatabaseError as error:
            raise CatalogError(f"the SQLite candidate did not apply: {error}") from error
        finally:
            connection.close()
        return schema_catalog.dump_sqlite(
            str(path), row_counts=True, rows=list(STATIC_TABLES["sqlite"]))


def round_trip_postgres(
    directory: Path, admin_url: str, psql: str | None, schema: str, privileges: bool = True
) -> dict[str, Any]:
    """Apply the candidate to a new database, dump it, and drop it again.

    Roles the privileges file names are created when they are missing and
    dropped afterwards; a role that already exists is neither changed nor
    dropped.
    """
    psql = schema_catalog.find_psql(psql)
    parts = urllib.parse.urlsplit(admin_url)
    database = ROUND_TRIP_PREFIX + uuid.uuid4().hex[:8]
    target = urllib.parse.urlunsplit((parts.scheme, parts.netloc, "/" + database, parts.query, ""))
    # The roles the privileges file creates when they are missing.
    wanted = sorted(set(_CREATED_ROLE.findall(
        _read(directory, "postgres.privileges.sql")))) if privileges else []
    existing = set(schema_catalog.run_psql(
        psql, admin_url,
        "SELECT rolname FROM pg_catalog.pg_roles WHERE rolname = ANY (ARRAY["
        + ", ".join(quote_literal(name) for name in wanted) + "]::text[])",
    ).split()) if wanted else set()
    created = [name for name in wanted if name not in existing]
    schema_catalog.run_psql(
        psql, admin_url,
        f"CREATE DATABASE {schema_catalog.quote_identifier(database)} TEMPLATE template0",
    )
    try:
        if schema != "public":
            schema_catalog.run_psql(
                psql, target, f"CREATE SCHEMA {schema_catalog.quote_identifier(schema)}")
        path = f"SET search_path = {schema_catalog.quote_identifier(schema)};\n"
        # The privileges file names its source's roles and schema, so it is
        # applied only when the candidate is checked against that source.
        names = ["postgres.bootstrap.sql", "postgres.sql"]
        if privileges:
            names.append("postgres.privileges.sql")
        for name in names:
            text = render_postgres(_read(directory, name), schema)
            schema_catalog.run_psql(psql, target, f"{path}BEGIN;\n{text}\nCOMMIT;\n")
        return schema_catalog.dump_postgres(
            target, psql, schema, row_counts=True, rows=list(STATIC_TABLES["postgresql"]))
    finally:
        schema_catalog.run_psql(
            psql, admin_url,
            f"DROP DATABASE IF EXISTS {schema_catalog.quote_identifier(database)} WITH (FORCE)",
        )
        for name in created:
            schema_catalog.run_psql(
                psql, admin_url, f"DROP ROLE IF EXISTS {schema_catalog.quote_identifier(name)}")


def round_trip(arguments: argparse.Namespace) -> int:
    document = schema_catalog.load(arguments.source)
    directory = Path(arguments.directory)
    if document["engine"] == "sqlite":
        candidate = round_trip_sqlite(directory)
        expected = document
    else:
        admin_url = os.environ.get(arguments.admin_url_env)
        if not admin_url:
            raise CatalogError(f"{arguments.admin_url_env} is not set")
        candidate = round_trip_postgres(
            directory, admin_url, arguments.psql, arguments.schema,
            privileges=not arguments.structure_only)
        document = baseline_scope(document, arguments.schema)
        expected = schema_catalog.only_schema(document, arguments.schema)
        candidate = schema_catalog.only_schema(candidate, arguments.schema)
    if arguments.structure_only:
        expected = schema_catalog.structure_only(expected)
        candidate = schema_catalog.structure_only(candidate)
    if arguments.candidate_output:
        Path(arguments.candidate_output).write_text(
            schema_catalog.render(candidate), encoding="utf-8")
    # The static tables' rows are part of the baseline and are compared. A
    # source dumped without them cannot prove the generated statements.
    if "rows" not in document:
        raise CatalogError("the source document was dumped without --rows for the static tables")
    static = STATIC_TABLES[document["engine"]]
    expected = {**expected, "rows": {
        name: rows for name, rows in document["rows"].items() if name in static}}
    lines = schema_catalog.compare(expected, candidate, include_rows=True)
    for line in lines:
        print(line)
    # Other tables that hold rows in the source get them after the baseline,
    # from the owner phase or from serving.
    seeded = sorted(
        name for name, count in (document.get("row_counts") or {}).items()
        if count and name.rsplit(".", 1)[-1] not in static + BOOTSTRAP_TABLES)
    if seeded:
        print(f"note: {len(seeded)} table(s) hold rows in the source that the baseline does "
              f"not carry: {', '.join(seeded)}", file=sys.stderr)
    print(f"round trip ({document['engine']}): {schema_catalog.summarise(lines)}",
          file=sys.stderr)
    return 1 if lines else 0


# ---------------------------------------------------------------------------
# Engine lockstep
# ---------------------------------------------------------------------------


def read_engine_differences(path: Path) -> dict[str, set[str]]:
    """Tables that one engine has and the other, by design, does not."""
    allowed: dict[str, set[str]] = {"postgres-only": set(), "sqlite-only": set()}
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as error:
        raise CatalogError(f"could not read {path}: {error}") from error
    for number, line in enumerate(text.splitlines(), 1):
        entry = line.split("#", 1)[0].split()
        if not entry:
            continue
        if len(entry) != 2 or entry[0] not in allowed:
            raise CatalogError(
                f"{path}:{number}: expected `postgres-only TABLE` or `sqlite-only TABLE`")
        allowed[entry[0]].add(entry[1])
    return allowed


def lockstep(
    postgres: dict[str, Any], sqlite: dict[str, Any], schema: str,
    allowed: dict[str, set[str]],
) -> list[str]:
    """Names one engine's schema has and the other's lacks.

    The engines spell types, defaults and constraints differently, so only
    names are compared: tables, the columns of tables both have, and the
    explicitly created indexes of those tables. A table listed in `allowed`
    may exist on its engine alone; a listing that no longer applies is
    reported too.
    """
    if postgres.get("engine") != "postgresql" or sqlite.get("engine") != "sqlite":
        raise CatalogError("lockstep takes a PostgreSQL document, then a SQLite document")

    def names(document: dict[str, Any], kind: str, keep: Any) -> set[tuple[str, ...]]:
        return {keep(row) for key, row in document["objects"].items()
                if key.startswith(kind + " ") and row.get("schema", schema) == schema
                and keep(row) is not None}

    pg_tables = {name for (name,) in names(postgres, "table", lambda row: (row["name"],))}
    lite_tables = {name for (name,) in names(sqlite, "table", lambda row: (row["name"],))}
    lines = []
    for label, only, listed in (
        ("postgres-only", pg_tables - lite_tables, allowed["postgres-only"]),
        ("sqlite-only", lite_tables - pg_tables, allowed["sqlite-only"]),
    ):
        lines += [f"{label} table {name} is not listed" for name in sorted(only - listed)]
        lines += [f"{label} listing {name} no longer applies" for name in sorted(listed - only)]
    shared = pg_tables & lite_tables
    column = lambda row: (row["relation"], row["name"]) if row["relation"] in shared else None
    pg_columns = names(postgres, "column", column)
    lite_columns = names(sqlite, "column", column)
    lines += [f"postgres-only column {'.'.join(name)}" for name in sorted(pg_columns - lite_columns)]
    lines += [f"sqlite-only column {'.'.join(name)}" for name in sorted(lite_columns - pg_columns)]
    pg_indexes = names(postgres, "index", lambda row: (row["relation"], row["name"])
                       if row["relation"] in shared and row["constraint"] is None else None)
    lite_indexes = names(sqlite, "index", lambda row: (row["relation"], row["name"])
                         if row["relation"] in shared and row["origin"] == "c" else None)
    lines += [f"postgres-only index {'.'.join(name)}" for name in sorted(pg_indexes - lite_indexes)]
    lines += [f"sqlite-only index {'.'.join(name)}" for name in sorted(lite_indexes - pg_indexes)]
    return lines


# ---------------------------------------------------------------------------
# Command line
# ---------------------------------------------------------------------------


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    commands = parser.add_subparsers(dest="command", required=True)
    generate_command = commands.add_parser("generate", help="write the candidate SQL")
    generate_command.add_argument("source", help="catalog document to generate from")
    generate_command.add_argument("--out", required=True, help="directory to write into")
    generate_command.add_argument("--commit", required=True,
                                  help="commit the source database was built from")
    generate_command.add_argument("--schema", default="public",
                                  help="PostgreSQL application schema (default: public)")
    generate_command.add_argument(
        "--fingerprint", metavar="FILE",
        help="fingerprint of the source database, stored as <engine>.fingerprint.json")
    generate_command.add_argument(
        "--retired-ledger", metavar="FILE",
        help="applied migration rows of the source database, stored as "
             "<engine>.retired-ledger.json")
    check = commands.add_parser("round-trip", help="apply the candidate and compare")
    check.add_argument("source", help="catalog document the candidate was generated from")
    check.add_argument("--directory", required=True, help="directory that holds the candidate")
    check.add_argument("--schema", default="public",
                       help="PostgreSQL application schema (default: public)")
    check.add_argument("--admin-url-env", default="WADDLE_BASELINE_ADMIN_URL", metavar="VARIABLE",
                       help="environment variable that holds a superuser PostgreSQL URL of a "
                            "disposable cluster (default: WADDLE_BASELINE_ADMIN_URL)")
    check.add_argument("--psql", help="psql executable (default: WADDLE_PSQL, then PATH)")
    check.add_argument("--structure-only", action="store_true",
                       help="ignore owners, privileges and roles")
    check.add_argument("--candidate-output", help="also write the candidate's catalog document")
    engines = commands.add_parser(
        "lockstep", help="compare table, column and index names across the two engines")
    engines.add_argument("postgres", help="PostgreSQL catalog document")
    engines.add_argument("sqlite", help="SQLite catalog document")
    engines.add_argument("--schema", default="public",
                         help="PostgreSQL application schema (default: public)")
    engines.add_argument(
        "--differences", default=str(Path(__file__).with_name("schema-engine-differences.txt")),
        help="file that lists the tables one engine alone has")
    return parser


def main(argv: list[str] | None = None) -> int:
    arguments = _parser().parse_args(argv)
    try:
        if arguments.command == "round-trip":
            return round_trip(arguments)
        if arguments.command == "lockstep":
            lines = lockstep(
                schema_catalog.load(arguments.postgres), schema_catalog.load(arguments.sqlite),
                arguments.schema, read_engine_differences(Path(arguments.differences)))
            for line in lines:
                print(line)
            print(f"lockstep: {len(lines)} difference(s)", file=sys.stderr)
            return 1 if lines else 0
        document = schema_catalog.load(arguments.source)
        result = generate(document, arguments.schema, arguments.commit,
                          Path(arguments.source).name)
        out = Path(arguments.out)
        out.mkdir(parents=True, exist_ok=True)
        engine = "postgres" if document["engine"] == "postgresql" else "sqlite"
        for option, suffix in ((arguments.fingerprint, "fingerprint"),
                               (arguments.retired_ledger, "retired-ledger")):
            if option:
                try:
                    data = json.loads(Path(option).read_text(encoding="utf-8"))
                except (OSError, ValueError) as error:
                    raise CatalogError(f"could not read {option}: {error}") from error
                result["files"][f"{engine}.{suffix}.json"] = json.dumps(
                    data, indent=1, sort_keys=True, ensure_ascii=False) + "\n"
        for name, text in result["files"].items():
            (out / name).write_text(text, encoding="utf-8")
            print(f"wrote {out / name}")
        for key in result["outside"]:
            print(f"note: outside the application schema, not generated: {key}", file=sys.stderr)
        if document["engine"] == "postgresql":
            for name, reason in sorted(OWNER_PREPARED_TABLES.items()):
                if f"table {arguments.schema}.{name}" in document["objects"]:
                    print(f"note: left to its owner preparation: table {name} ({reason})",
                          file=sys.stderr)
            for name, reason in sorted(OWNER_PREPARED_FUNCTIONS.items()):
                print(f"note: left to its owner preparation: function {name} ({reason})",
                      file=sys.stderr)
        for name in result["names"]:
            print(f"note: written as a schema template: {name}", file=sys.stderr)
        return 0
    except CatalogError as error:
        print(f"error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
