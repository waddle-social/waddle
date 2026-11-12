#!/usr/bin/env python3
"""Build a deployment's schema the ways it can be reached, and compare them.

Each path starts from an empty database and runs the real server binary.

History paths, which build the schema from the migration history and every
store's own bootstrap:

  legacy      PostgreSQL, clustered, one login that owns everything. The
              operator's `waddle-cluster-keypool` enrollment SQL is applied,
              then the server starts without an attestation and is stopped
              once it reports ready.
  restricted  PostgreSQL, clustered, separate owner and runtime roles:
              `sql/restricted_runtime_provisioning.sql`, `owner-phase`,
              `install-attestor`.
  tenant      As restricted, in a schema that is not `public`.
  upgraded    The legacy database carried through provisioning, the owner
              phase and the attestor installation.
  sqlite      A SQLite file the server starts on.

Baseline paths, which set `WADDLE_DB_BASELINE_ACTION=apply` so the schema
baseline is the migration catalog, and must give what the history gives:

  baseline         As restricted. The handoff is then emitted and the server
                   is started under the runtime login until it is ready.
  baseline-tenant  As tenant.
  baseline-sqlite  As sqlite; the server is started twice.

Stamp paths, which set `WADDLE_DB_BASELINE_ACTION=stamp` on a database the
history built:

  stamp           The restricted database: the catalog must not change, the
                  ledger must equal the baseline database's, a second stamp
                  must change nothing, the restricted server must start, and
                  a run without the baseline must refuse the database.
  stamp-sqlite    The same on the SQLite file.
  stamp-refusals  `apply` on a database that still holds the history, a stamp
                  on a schema that has an extra table, and the baseline on a
                  PostgreSQL server that owns its schema: each must refuse
                  and leave the database as it was.

Every database is dumped with `schema_catalog.py` into the output directory,
with row counts, the contents of the tables whose rows do not depend on the
deployment, its structural fingerprint and its applied migration rows. Each
comparison prints `ok` or `FAIL`; the exit code is 1 when one failed.

`--paths history` runs the history paths, which is what the baseline is
generated from. The default runs everything, which needs a binary built
with the generated baseline.

The harness creates databases and roles named `waddle_baseline_*` through
the administrator URL and drops them again unless `--keep` is given. It
needs a superuser: only one can create roles and run the provisioning
procedure. It must never be pointed at a cluster that serves anything.

The server needs SpiceDB unless its fixed test account is enabled, so the
harness enables it. That adds rows (one account), never schema.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import signal
import socket
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import schema_catalog  # noqa: E402

SERVER_ROOT = Path(__file__).resolve().parent.parent
PROVISIONING_SQL = (
    SERVER_ROOT / "crates/waddle-server/sql/restricted_runtime_provisioning.sql"
)

PREFIX = "waddle_baseline_"
SCHEMA = "public"
ATTESTOR_SCHEMA = "waddle_attestation"
DEPLOYMENT_UUID = "018f47b2-4b2e-7a3a-9a4c-52a5a6ba5e00"
SESSION_KEY = "schema-baseline-harness-session-key-32-bytes"
OCCUPANT_ID_SECRET = "schema-baseline-harness-occupant-id-secret-32b"
LEASE_TTL_MS = 2000
START_TIMEOUT_SECONDS = 180

TENANT_SCHEMA = "tenant_a"
RELEASE_GIT_SHA = "0" * 40

# Tables whose rows do not depend on the deployment or on the clock. Their
# contents are recorded with every dump and compared between paths.
ROW_TABLES = [
    "clustering_orphan_reaper_cursors",
    "ingress_epoch_guard_manifest",
    "ingress_protocol_epoch",
    "pending_notification_recovery_counter",
    "pubsub_schema_version",
]

HISTORY_PATHS = ("legacy", "restricted", "sqlite", "tenant", "upgraded")
PATHS = (
    "legacy", "restricted", "sqlite", "tenant",
    "baseline", "baseline-tenant", "baseline-sqlite",
    "stamp", "stamp-sqlite", "stamp-refusals",
    "upgraded",
)
SQLITE_PATHS = ("sqlite", "baseline-sqlite", "stamp-sqlite")
REQUIRES = {
    "upgraded": "legacy", "baseline": "restricted", "baseline-tenant": "tenant",
    "baseline-sqlite": "sqlite", "stamp": "baseline", "stamp-sqlite": "baseline-sqlite",
    "stamp-refusals": "tenant",
}

# Differences that are expected between two databases with one schema.
ATTESTOR_COMMENT = ("~ function waddle_attestation.read_physical_identity()",)
# A database that served holds the rows the server wrote while it ran.
SERVED_ROWS = ("~ rows ",)
LEDGER_ROWS = (
    "~ rows public._migrations", "~ rows _migrations", f"~ rows {TENANT_SCHEMA}._migrations")

# The seven roles of the restricted shape. They are cluster-wide, so the
# restricted and the upgraded database share six of them; each has its own
# old login, because the provisioning reassigns everything a login owns.
ROLES = {
    "database_owner": PREFIX + "database_owner",
    "schema_owner": PREFIX + "owner",
    "migrator_login": PREFIX + "migrator",
    "runtime_login": PREFIX + "runtime",
    "attestor_installer_login": PREFIX + "attestor_installer",
    "attestor_definer_role": PREFIX + "attestor",
}
# Each database gets its own old login, because the provisioning reassigns
# everything a login owns. All are recorded under one name.
OLD_LOGINS = {
    label: PREFIX + "app_" + label.replace("-", "_")
    for label in ("legacy", "restricted", "tenant", "baseline", "baseline-tenant")
}
ROLE_ALIASES = {login: PREFIX + "app" for login in OLD_LOGINS.values()}


class HarnessError(Exception):
    """A step of a path did not complete."""


class Harness:
    def __init__(self, arguments: argparse.Namespace, postgres: bool) -> None:
        self.admin_url = ""
        self.psql = ""
        if postgres:
            admin_url = os.environ.get(arguments.admin_url_env)
            if not admin_url:
                raise HarnessError(f"{arguments.admin_url_env} is not set")
            self.admin_url = admin_url
            self.psql = schema_catalog.find_psql(arguments.psql)
        self.server = self._executable(arguments.server_bin)
        self.keypool = (
            self._executable(arguments.keypool_bin) if arguments.keypool_bin else None
        )
        self.out = Path(arguments.out).resolve()
        self.out.mkdir(parents=True, exist_ok=True)
        self.keep = arguments.keep
        self.work = Path(tempfile.mkdtemp(prefix="waddle-schema-history-"))
        self.suffix = uuid.uuid4().hex[:8]
        self.databases: list[str] = []
        self.roles: list[str] = []
        self.databases_by_label: dict[str, str] = {}
        self.generations: dict[str, str] = {}
        self.operations: dict[str, str] = {}
        self.results: list[tuple[str, bool]] = []
        self.pool: str | None = None
        self.enrollment_sql: str | None = None

    @staticmethod
    def _executable(path: str) -> str:
        resolved = Path(path).resolve()
        if not resolved.is_file() or not os.access(resolved, os.X_OK):
            raise HarnessError(f"{path} is not an executable file")
        return str(resolved)

    # -- database plumbing --------------------------------------------------

    def url(self, database: str, login: str | None = None) -> str:
        parts = urllib.parse.urlsplit(self.admin_url)
        netloc = parts.netloc
        if login is not None:
            host = netloc.rsplit("@", 1)[-1]
            netloc = f"{urllib.parse.quote(login, safe='')}@{host}"
        return urllib.parse.urlunsplit(
            (parts.scheme, netloc, "/" + database, parts.query, "")
        )

    def admin(self, sql: str, database: str | None = None) -> str:
        url = self.url(database) if database else self.admin_url
        return schema_catalog.run_psql(self.psql, url, sql)

    def create_role(self, name: str, login: bool) -> None:
        exists = self.admin(
            f"SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = "
            f"{schema_catalog.quote_literal(name)}"
        ).strip()
        if exists:
            raise HarnessError(
                f"role {name} already exists; a previous run may have been "
                f"interrupted. Drop its {PREFIX}* databases and roles first"
            )
        self.roles.append(name)
        self.admin(
            f"CREATE ROLE {schema_catalog.quote_identifier(name)} "
            f"{'LOGIN' if login else 'NOLOGIN'} NOINHERIT NOSUPERUSER NOCREATEDB "
            "NOCREATEROLE NOREPLICATION NOBYPASSRLS"
        )

    def create_database(self, label: str, owner: str) -> str:
        name = f"{PREFIX}{label}_{self.suffix}"
        self.databases.append(name)
        self.databases_by_label[label] = name
        self.admin(
            f"CREATE DATABASE {schema_catalog.quote_identifier(name)} "
            f"OWNER {schema_catalog.quote_identifier(owner)} TEMPLATE template0"
        )
        return name

    def cleanup(self) -> None:
        if self.keep:
            print(f"kept databases {self.databases} and roles {self.roles}")
            print(f"kept working directory {self.work}")
            return
        for database in self.databases:
            self.admin(
                f"DROP DATABASE IF EXISTS {schema_catalog.quote_identifier(database)} "
                "WITH (FORCE)"
            )
        for role in reversed(self.roles):
            self.admin(f"DROP ROLE IF EXISTS {schema_catalog.quote_identifier(role)}")
        shutil.rmtree(self.work, ignore_errors=True)

    # -- processes ----------------------------------------------------------

    def environment(self, extra: dict[str, str | None]) -> dict[str, str]:
        """A child environment with no ambient `WADDLE_*`, `OTEL_*` or `PG*`."""
        environment = {
            key: value
            for key, value in os.environ.items()
            if not key.startswith(("WADDLE_", "OTEL_", "PG"))
        }
        environment.update(extra)
        return {key: value for key, value in environment.items() if value is not None}

    def keypair_pool(self) -> None:
        if self.pool is not None:
            return
        if self.keypool is None:
            raise HarnessError("the PostgreSQL paths need --keypool-bin")
        pool, peers, sql = (self.work / name for name in ("pool", "peers", "enroll.sql"))
        completed = subprocess.run(
            [self.keypool, "generate", "--count", "3", "--pool-output", str(pool),
             "--peer-ids-output", str(peers), "--sql-output", str(sql)],
            capture_output=True, text=True, check=False,
        )
        if completed.returncode != 0:
            raise HarnessError(f"waddle-cluster-keypool failed: {completed.stderr}")
        self.pool = pool.read_text(encoding="utf-8").strip()
        self.enrollment_sql = sql.read_text(encoding="utf-8")

    def serve_until_ready(self, label: str, extra: dict[str, str | None]) -> None:
        """Start the server, wait until `/ready` answers 200, and stop it."""
        directory = self.work / label
        directory.mkdir()
        port_file = directory / "port"
        log = directory / "server.log"
        environment = self.environment(
            {
                "WADDLE_CERTS_EPHEMERAL": "true",
                "WADDLE_TEST_FIXED_ACCOUNT_ENABLED": "true",
                "WADDLE_TEST_FIXED_ACCOUNT_PASSWORD": uuid.uuid4().hex,
                "WADDLE_TEST_PROFILE_PUBLISH_TOKEN": uuid.uuid4().hex,
                "WADDLE_SERVER_OWNER_LOCALPARTS": "admin",
                "WADDLE_HTTP_ADDR": "127.0.0.1:0",
                "WADDLE_HTTP_PORT_FILE": str(port_file),
                "WADDLE_XMPP_DOMAIN": "localhost",
                "WADDLE_UPLOAD_DIR": str(directory / "uploads"),
                "WADDLE_GIT_SHA": RELEASE_GIT_SHA,
                "WADDLE_SESSION_KEY": SESSION_KEY,
                "WADDLE_OCCUPANT_ID_SECRET": OCCUPANT_ID_SECRET,
                "WADDLE_DEPLOYMENT_UUID": DEPLOYMENT_UUID,
                "WADDLE_DB_LINEAGE_ACTION": "enroll",
                **extra,
            }
        )
        environment = {key: value for key, value in environment.items() if value is not None}
        with log.open("wb") as output:
            process = subprocess.Popen(
                [self.server], env=environment, stdout=output, stderr=subprocess.STDOUT
            )
        try:
            deadline = time.monotonic() + START_TIMEOUT_SECONDS
            while True:
                if process.poll() is not None:
                    raise HarnessError(
                        f"the {label} server exited with {process.returncode} before "
                        f"it was ready:\n{_tail(log)}"
                    )
                if time.monotonic() > deadline:
                    raise HarnessError(
                        f"the {label} server was not ready in "
                        f"{START_TIMEOUT_SECONDS} s:\n{_tail(log)}"
                    )
                if port_file.is_file() and port_file.read_text().strip():
                    port = port_file.read_text().strip()
                    try:
                        with urllib.request.urlopen(
                            f"http://127.0.0.1:{port}/ready", timeout=5
                        ) as response:
                            if response.status == 200:
                                break
                    except (urllib.error.URLError, OSError):
                        pass
                time.sleep(0.25)
        finally:
            if process.poll() is None:
                process.send_signal(signal.SIGTERM)
                try:
                    process.wait(timeout=90)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
        if process.returncode != 0:
            raise HarnessError(
                f"the {label} server stopped with {process.returncode}:\n{_tail(log)}"
            )

    def subcommand(
        self, label: str, arguments: list[str], extra: dict[str, str]
    ) -> subprocess.CompletedProcess[str]:
        completed = subprocess.run(
            [self.server, *arguments],
            env=self.environment(extra),
            capture_output=True,
            text=True,
            check=False,
            timeout=600,
        )
        (self.work / f"{label}.log").write_text(
            completed.stdout + completed.stderr, encoding="utf-8"
        )
        return completed

    # -- paths --------------------------------------------------------------

    def cluster_environment(self, label: str) -> dict[str, str]:
        assert self.pool is not None
        return {
            "WADDLE_DB_DRIVER": "postgres",
            "WADDLE_CLUSTERING_ENABLED": "true",
            "WADDLE_CLUSTERING_KEYPAIR_POOL": self.pool,
            "WADDLE_CLUSTERING_LISTEN_ADDRS": "/ip4/127.0.0.1/tcp/0",
            "WADDLE_CLUSTERING_NODE_ID_FILE": str(self.work / f"{label}-node-id"),
            "WADDLE_CLUSTERING_LEASE_TTL_MS": str(LEASE_TTL_MS),
            "WADDLE_CLUSTERING_HEARTBEAT_INTERVAL_MS": str(LEASE_TTL_MS // 4),
        }

    def build_legacy(self, label: str = "legacy", action: str | None = None,
                     enroll_peers: bool = True) -> str:
        """A database one login owns, built by a server without an attestation."""
        self.keypair_pool()
        login = OLD_LOGINS[label]
        self.create_role(login, login=True)
        database = self.create_database(label, login)
        login_url = self.url(database, login)
        assert self.enrollment_sql is not None
        if enroll_peers:
            schema_catalog.run_psql(self.psql, login_url, self.enrollment_sql)
        self.serve_until_ready(
            label, {**self.cluster_environment(label), "WADDLE_DATABASE_URL": login_url,
                    **baseline_environment(action)})
        return database

    def serve_legacy(self, label: str, database: str, login: str, action: str | None) -> None:
        self.serve_until_ready(
            label, {**self.cluster_environment(label),
                    "WADDLE_DATABASE_URL": self.url(database, login),
                    **baseline_environment(action)})

    def ensure_restricted_roles(self) -> None:
        if ROLES["schema_owner"] in self.roles:
            return
        for key in ("database_owner", "schema_owner", "attestor_definer_role"):
            self.create_role(ROLES[key], login=False)
        for key in ("migrator_login", "runtime_login", "attestor_installer_login"):
            self.create_role(ROLES[key], login=True)

    def provision(self, database: str, old_login: str, schema: str) -> None:
        settings = {"schema": schema, "old_login": old_login, "attestor_schema": ATTESTOR_SCHEMA}
        settings.update(ROLES)
        prelude = "".join(
            f"SET waddle.{key} = {schema_catalog.quote_literal(value)};\n"
            for key, value in settings.items()
        )
        self.admin(prelude + PROVISIONING_SQL.read_text(encoding="utf-8"), database)

    def role_environment(self, schema: str) -> dict[str, str]:
        return {
            "WADDLE_XMPP_MAM_CLAIM_SCHEMA": schema,
            "WADDLE_DB_MIGRATOR_LOGIN": ROLES["migrator_login"],
            "WADDLE_XMPP_MAM_SCHEMA_OWNER": ROLES["schema_owner"],
            "WADDLE_DB_RUNTIME_LOGIN": ROLES["runtime_login"],
            "WADDLE_DB_ATTESTOR_DEFINER_ROLE": ROLES["attestor_definer_role"],
            "WADDLE_DB_ATTESTOR_INSTALLER_LOGIN": ROLES["attestor_installer_login"],
            "WADDLE_DB_ATTESTOR_SCHEMA": ATTESTOR_SCHEMA,
            "WADDLE_DEPLOYMENT_UUID": DEPLOYMENT_UUID,
        }

    def owner_phase(self, label: str, database: str, schema: str = SCHEMA, *,
                    enroll: bool = False, retire_legacy_slots: bool = False,
                    action: str | None = None, expect: int = 0) -> subprocess.CompletedProcess[str]:
        """Run `owner-phase` until it stops reporting a pending generation."""
        assert self.pool is not None
        generation = self.generations.setdefault(database, str(uuid.uuid4()))
        environment = {
            **self.role_environment(schema),
            **baseline_environment(action),
            "WADDLE_DATABASE_URL": self.url(database, ROLES["migrator_login"]),
            "WADDLE_SESSION_KEY": SESSION_KEY,
            "WADDLE_CLUSTERING_ENABLED": "true",
            "WADDLE_CLUSTERING_KEYPAIR_POOL": self.pool,
            "WADDLE_CLUSTERING_KEYPOOL_GENERATION": generation,
            "WADDLE_CLUSTERING_LEASE_TTL_MS": str(LEASE_TTL_MS),
            "WADDLE_CLUSTERING_HEARTBEAT_INTERVAL_MS": str(LEASE_TTL_MS // 4),
        }
        arguments = ["owner-phase", "--identity-output", str(self.identity(database))]
        if retire_legacy_slots:
            # The legacy server created the keypair-slot table.
            arguments += ["--legacy-keypair-slots", "retire",
                          "--legacy-ttl", f"{LEASE_TTL_MS}ms"]
        if enroll:
            environment["WADDLE_DB_LINEAGE_ACTION"] = "enroll"
        for attempt in range(6):
            completed = self.subcommand(f"{label}-owner-phase-{attempt}", arguments, environment)
            # Exit 3: the keypair-pool generation is pending its activation
            # delay. A rerun after the delay activates it.
            if completed.returncode != 3:
                break
            time.sleep(LEASE_TTL_MS / 1000 + 1)
        if completed.returncode != expect:
            raise HarnessError(
                f"{label}: owner-phase exited with {completed.returncode}, expected {expect}:\n"
                f"{completed.stderr[-4000:]}")
        return completed

    def identity(self, database: str) -> Path:
        return self.work / f"{database}-identity.json"

    def install_attestor(self, label: str, database: str, schema: str = SCHEMA,
                         action: str | None = None) -> None:
        operation = str(uuid.uuid4())
        completed = self.subcommand(
            f"{label}-install-attestor",
            ["install-attestor", "--identity", str(self.identity(database)),
             "--operation-id", operation],
            {**self.role_environment(schema), **baseline_environment(action),
             "WADDLE_DATABASE_URL": self.url(database, ROLES["attestor_installer_login"])},
        )
        if completed.returncode != 0:
            raise HarnessError(
                f"{label}: install-attestor exited with {completed.returncode}:\n"
                f"{completed.stderr[-4000:]}")
        self.operations[database] = operation

    def build_restricted(self, label: str, schema: str = SCHEMA,
                         action: str | None = None) -> str:
        """A database provisioned restricted from empty, then owner-prepared."""
        self.keypair_pool()
        login = OLD_LOGINS[label]
        self.create_role(login, login=True)
        database = self.create_database(label, login)
        if schema != SCHEMA:
            self.admin(
                f"CREATE SCHEMA {schema_catalog.quote_identifier(schema)} AUTHORIZATION "
                f"{schema_catalog.quote_identifier(login)}", database)
        self.ensure_restricted_roles()
        self.provision(database, login, schema)
        self.owner_phase(label, database, schema, enroll=True, action=action)
        self.install_attestor(label, database, schema, action)
        return database

    def upgrade(self, database: str) -> None:
        self.ensure_restricted_roles()
        self.provision(database, OLD_LOGINS["legacy"], SCHEMA)
        self.owner_phase("upgraded", database, retire_legacy_slots=True)
        self.install_attestor("upgraded", database)

    def serve_restricted(self, label: str, database: str, old_login: str,
                         schema: str = SCHEMA, action: str | None = None) -> None:
        """Compose the handoff and serve under the runtime login until ready.

        The restricted server accepts an unencrypted connection only over a
        Unix socket, so a relay in the working directory fronts the cluster.
        """
        relay = SocketRelay(self.work / f"{label}-socket", self.admin_url)
        try:
            runtime_url = (
                f"postgresql:///{database}?host={relay.directory}&port={relay.port}"
                f"&user={ROLES['runtime_login']}&options=-c%20search_path%3D{schema}")
            old_oid = int(self.admin(
                "SELECT oid FROM pg_catalog.pg_roles WHERE rolname = "
                f"{schema_catalog.quote_literal(old_login)}").strip())
            record = lambda: str(uuid.uuid4())  # noqa: E731
            cutover = self.work / f"{label}-cutover.json"
            cutover.write_text(json.dumps({
                "transition_id": record(),
                "primary": {
                    "provider": "schema-history-harness", "cluster_id": "disposable-cluster",
                    "instance_id": "disposable-primary",
                    "endpoint_host": "localhost", "endpoint_port": relay.port,
                    "endpoint_socket_dir": str(relay.directory),
                    "observation_record_id": record(),
                },
                "old_runtime": {"login": old_login, "role_oid": old_oid,
                                "credential_cutoff_record_id": record()},
                "listener_cutoff_record_id": record(),
                "backend_cutoff_record_id": record(),
                "prepared_transaction_cutoff_record_id": record(),
                "mutator_policy": {"mode": "quiesced_for_serving_interval",
                                   "record_id": record()},
            }), encoding="utf-8")
            handoff = self.work / f"{label}-handoff.json"
            completed = self.subcommand(
                f"{label}-emit-handoff",
                ["emit-handoff", "--release-git-sha", RELEASE_GIT_SHA,
                 "--installation-operation-id", self.operations[database],
                 "--cutover", str(cutover), "--output", str(handoff)],
                {**self.role_environment(schema), **baseline_environment(action),
                 "WADDLE_DATABASE_URL": self.url(database, ROLES["migrator_login"])},
            )
            if completed.returncode != 0:
                raise HarnessError(
                    f"{label}: emit-handoff exited with {completed.returncode}:\n"
                    f"{completed.stderr[-4000:]}")
            self.serve_until_ready(label, {
                **self.cluster_environment(label),
                **baseline_environment(action),
                "WADDLE_DATABASE_URL": runtime_url,
                "WADDLE_DB_RUNTIME_ATTESTATION": handoff.read_text(encoding="utf-8"),
                "WADDLE_DB_MIGRATOR_LOGIN": ROLES["migrator_login"],
                "WADDLE_XMPP_MAM_SCHEMA_OWNER": ROLES["schema_owner"],
                "WADDLE_XMPP_MAM_CLAIM_SCHEMA": schema,
                "WADDLE_DB_LINEAGE_ACTION": None,
            })
        finally:
            relay.close()

    def serve_sqlite(self, label: str, path: Path, action: str | None = None) -> None:
        self.serve_until_ready(label, {
            "WADDLE_DB_DRIVER": "sqlite",
            "WADDLE_DATABASE_URL": f"sqlite://{path}?mode=rwc",
            **baseline_environment(action),
        })

    # -- dumps --------------------------------------------------------------

    def dump_postgres(self, name: str, database: str, schema: str = SCHEMA) -> Path:
        url = self.url(database)
        document = schema_catalog.dump_postgres(
            url, self.psql, schema, ROLE_ALIASES, row_counts=True, rows=ROW_TABLES)
        path = self.out / f"postgres-{name}.json"
        path.write_text(schema_catalog.render(document), encoding="utf-8")
        (self.out / f"postgres-{name}.fingerprint.json").write_text(
            schema_catalog.render_fingerprint(
                schema_catalog.fingerprint_postgres(url, self.psql, schema)), encoding="utf-8")
        rows = json.loads(schema_catalog.run_psql(
            self.psql, url,
            "SELECT coalesce(jsonb_agg(jsonb_build_array(version, description, checksum) "
            "ORDER BY version), '[]'::jsonb)::text "
            f"FROM {schema_catalog.quote_identifier(schema)}._migrations"))
        self.write_ledger(f"postgres-{name}", rows)
        print(f"wrote {path} ({len(rows)} applied migration(s))")
        return path

    def dump_sqlite(self, name: str, database: Path) -> Path:
        path = self.out / f"sqlite-{name}.json"
        document = schema_catalog.dump_sqlite(str(database), row_counts=True, rows=ROW_TABLES)
        path.write_text(schema_catalog.render(document), encoding="utf-8")
        (self.out / f"sqlite-{name}.fingerprint.json").write_text(
            schema_catalog.render_fingerprint(schema_catalog.fingerprint_sqlite(str(database))),
            encoding="utf-8")
        connection = sqlite3.connect(f"file:{database}?mode=ro", uri=True)
        try:
            rows = [list(row) for row in connection.execute(
                "SELECT version, description, checksum FROM _migrations ORDER BY version")]
        finally:
            connection.close()
        self.write_ledger(f"sqlite-{name}", rows)
        print(f"wrote {path} ({len(rows)} applied migration(s))")
        return path

    def write_ledger(self, name: str, rows: list[list[object]]) -> None:
        (self.out / f"{name}.ledger.json").write_text(
            json.dumps(rows, indent=1, ensure_ascii=False) + "\n", encoding="utf-8")

    def same_file(self, title: str, left: str, right: str) -> None:
        equal = (self.out / left).read_bytes() == (self.out / right).read_bytes()
        self.verdict(title, equal, f"{left} and {right} {'are equal' if equal else 'DIFFER'}")

    def verdict(self, title: str, passed: bool, detail: str) -> None:
        self.results.append((title, passed))
        print(f"{'ok  ' if passed else 'FAIL'} {title}: {detail}")

    def report(self, title: str, left: Path, right: Path, *, structure_only: bool = False,
               rows: bool = True, allow: tuple[str, ...] = (), expect_equal: bool = True) -> None:
        """Compare two dumps and record whether they differ only as allowed.

        `allow` holds the first words of difference lines that are expected,
        such as the attestor's comment, which names the installation.
        """
        a, b = schema_catalog.load(str(left)), schema_catalog.load(str(right))
        if structure_only:
            a, b = schema_catalog.structure_only(a), schema_catalog.structure_only(b)
        lines = schema_catalog.compare(a, b, include_row_counts=rows, include_rows=rows)
        path = self.out / f"{title}.diff"
        path.write_text("".join(line + "\n" for line in lines), encoding="utf-8")
        unexpected = [
            line for line in lines
            if line[:1] in "+-~!" and not any(line.startswith(prefix) for prefix in allow)]
        detail = (f"A={left.name} B={right.name}: {schema_catalog.summarise(lines)}, "
                  f"{len(unexpected)} unexpected ({path.name})")
        if expect_equal:
            self.verdict(title, not unexpected, detail)
        else:
            print(f"info {title}: {detail}")


class SocketRelay:
    """A Unix socket that forwards every connection to the cluster's TCP port."""

    def __init__(self, directory: Path, admin_url: str) -> None:
        parts = urllib.parse.urlsplit(admin_url)
        self.target = (parts.hostname or "127.0.0.1", parts.port or 5432)
        self.directory = directory
        self.port = self.target[1]
        directory.mkdir()
        self.listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.listener.bind(str(directory / f".s.PGSQL.{self.port}"))
        self.listener.listen(64)
        threading.Thread(target=self._accept, daemon=True).start()

    def _accept(self) -> None:
        while True:
            try:
                client, _ = self.listener.accept()
            except OSError:
                return
            try:
                upstream = socket.create_connection(self.target)
            except OSError:
                client.close()
                continue
            for source, sink in ((client, upstream), (upstream, client)):
                threading.Thread(target=self._copy, args=(source, sink), daemon=True).start()

    @staticmethod
    def _copy(source: socket.socket, sink: socket.socket) -> None:
        try:
            while data := source.recv(65536):
                sink.sendall(data)
        except OSError:
            pass
        finally:
            for end in (source, sink):
                try:
                    end.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass

    def close(self) -> None:
        self.listener.close()


def baseline_environment(action: str | None) -> dict[str, str]:
    return {"WADDLE_DB_BASELINE_ACTION": action} if action else {}


def _tail(path: Path, lines: int = 8) -> str:
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return ""
    return "\n".join(line[:600] for line in text.splitlines()[-lines:])


def run(arguments: argparse.Namespace) -> int:
    selected = [name.strip() for name in arguments.paths.split(",") if name.strip()]
    if selected == ["all"]:
        selected = list(PATHS)
    if selected == ["history"]:
        selected = list(HISTORY_PATHS)
    unknown = sorted(set(selected) - set(PATHS))
    if unknown:
        raise HarnessError(f"unknown path(s) {unknown}; choose from {list(PATHS)}")
    for path, needs in REQUIRES.items():
        if path in selected and needs not in selected:
            raise HarnessError(f"the {path} path starts from the {needs} path; select both")
    harness = Harness(arguments, postgres=any(name not in SQLITE_PATHS for name in selected))
    dumps: dict[str, Path] = {}
    try:
        legacy_database = restricted_database = None
        sqlite_history = None
        if "legacy" in selected:
            legacy_database = harness.build_legacy()
            dumps["legacy"] = harness.dump_postgres("legacy", legacy_database)
        if "restricted" in selected:
            restricted_database = harness.build_restricted("restricted")
            dumps["restricted"] = harness.dump_postgres("restricted", restricted_database)
        if "sqlite" in selected:
            sqlite_history = harness.work / "sqlite-history.db"
            harness.serve_sqlite("sqlite", sqlite_history)
            dumps["sqlite"] = harness.dump_sqlite("history", sqlite_history)
        if "tenant" in selected:
            database = harness.build_restricted("tenant", TENANT_SCHEMA)
            dumps["tenant"] = harness.dump_postgres("tenant", database, TENANT_SCHEMA)

        if "legacy" in dumps and "restricted" in dumps:
            harness.report("legacy-vs-restricted.structure", dumps["legacy"],
                           dumps["restricted"], structure_only=True, expect_equal=False)

        # The baseline builds what the history builds.
        if "baseline" in selected:
            database = harness.build_restricted("baseline", action="apply")
            harness.serve_restricted("baseline-serve", database, OLD_LOGINS["baseline"],
                                     action="apply")
            dumps["baseline"] = harness.dump_postgres("baseline", database)
            harness.report("history-vs-baseline.postgres", dumps["restricted"],
                           dumps["baseline"], allow=ATTESTOR_COMMENT + SERVED_ROWS)
            harness.verdict("baseline.postgres.restricted-server-starts", True, "ready")
            harness.same_file("history-vs-baseline.postgres.fingerprint",
                              "postgres-restricted.fingerprint.json",
                              "postgres-baseline.fingerprint.json")
        if "baseline-tenant" in selected:
            database = harness.build_restricted("baseline-tenant", TENANT_SCHEMA, action="apply")
            dumps["baseline-tenant"] = harness.dump_postgres(
                "baseline-tenant", database, TENANT_SCHEMA)
            harness.report("history-vs-baseline.tenant", dumps["tenant"],
                           dumps["baseline-tenant"], allow=ATTESTOR_COMMENT + LEDGER_ROWS)
            harness.same_file("fingerprint.public-vs-tenant",
                              "postgres-restricted.fingerprint.json",
                              "postgres-tenant.fingerprint.json")
        if "baseline-sqlite" in selected:
            path = harness.work / "sqlite-baseline.db"
            harness.serve_sqlite("baseline-sqlite", path, "apply")
            harness.serve_sqlite("baseline-sqlite-restart", path, "apply")
            dumps["baseline-sqlite"] = harness.dump_sqlite("baseline", path)
            harness.report("history-vs-baseline.sqlite", dumps["sqlite"],
                           dumps["baseline-sqlite"], allow=LEDGER_ROWS)
            harness.same_file("history-vs-baseline.sqlite.fingerprint",
                              "sqlite-history.fingerprint.json",
                              "sqlite-baseline.fingerprint.json")

        # The stamp records the baseline on a database the history built.
        if "stamp" in selected and restricted_database is not None:
            harness.owner_phase("stamp", restricted_database, action="stamp")
            dumps["stamp"] = harness.dump_postgres("stamped", restricted_database)
            harness.report("stamp.postgres.catalog-unchanged", dumps["restricted"],
                           dumps["stamp"], allow=LEDGER_ROWS)
            harness.same_file("stamp.postgres.ledger-equals-baseline",
                              "postgres-stamped.ledger.json", "postgres-baseline.ledger.json")
            harness.owner_phase("stamp-again", restricted_database, action="stamp")
            again = harness.dump_postgres("stamped-again", restricted_database)
            harness.report("stamp.postgres.second-is-a-no-op", dumps["stamp"], again)
            harness.same_file("stamp.postgres.second-ledger", "postgres-stamped.ledger.json",
                              "postgres-stamped-again.ledger.json")
            harness.serve_restricted("stamp-serve", restricted_database,
                                     OLD_LOGINS["restricted"], action="apply")
            harness.verdict("stamp.postgres.restricted-server-starts", True, "ready")
            refused = harness.owner_phase("stamp-unselected", restricted_database, expect=4)
            harness.verdict("stamp.postgres.history-catalog-refuses-stamped-database",
                            "migration ledger" in refused.stderr,
                            refused.stderr.strip().splitlines()[-1][:160])
        if "stamp-sqlite" in selected and sqlite_history is not None:
            harness.serve_sqlite("stamp-sqlite", sqlite_history, "stamp")
            dumps["stamp-sqlite"] = harness.dump_sqlite("stamped", sqlite_history)
            harness.report("stamp.sqlite.catalog-unchanged", dumps["sqlite"],
                           dumps["stamp-sqlite"], allow=LEDGER_ROWS)
            harness.same_file("stamp.sqlite.ledger-equals-baseline",
                              "sqlite-stamped.ledger.json", "sqlite-baseline.ledger.json")
            harness.serve_sqlite("stamp-sqlite-again", sqlite_history, "stamp")
            again = harness.dump_sqlite("stamped-again", sqlite_history)
            harness.report("stamp.sqlite.second-is-a-no-op", dumps["stamp-sqlite"], again)
            harness.serve_sqlite("stamp-sqlite-apply", sqlite_history, "apply")
            harness.verdict("stamp.sqlite.server-starts", True, "ready")
        if "stamp-refusals" in selected and legacy_database is not None:
            tenant = harness.databases_by_label["tenant"]
            refused = harness.owner_phase("apply-on-history", tenant, TENANT_SCHEMA,
                                          action="apply", expect=4)
            harness.verdict("apply.refuses-a-retired-ledger",
                            "WADDLE_DB_BASELINE_ACTION=stamp" in refused.stderr,
                            refused.stderr.strip().splitlines()[-1][:150])
            # A schema that is not the one the history builds is not stamped,
            # and the refusal leaves the ledger as it was.
            harness.admin(f"CREATE TABLE {TENANT_SCHEMA}.stray (id integer)", tenant)
            refused = harness.owner_phase("stamp-on-changed-schema", tenant, TENANT_SCHEMA,
                                          action="stamp", expect=4)
            harness.verdict("stamp.refuses-a-changed-schema",
                            "unexpected relation stray" in refused.stderr,
                            refused.stderr.strip().splitlines()[-1][:150])
            harness.admin(f"DROP TABLE {TENANT_SCHEMA}.stray", tenant)
            after = harness.dump_postgres("tenant-after-refusal", tenant, TENANT_SCHEMA)
            harness.report("stamp.refusal-changes-nothing", dumps["tenant"], after,
                           allow=ATTESTOR_COMMENT)
            harness.same_file("stamp.refusal-keeps-the-ledger", "postgres-tenant.ledger.json",
                              "postgres-tenant-after-refusal.ledger.json")
            # A PostgreSQL server that owns its schema does not take the
            # baseline: its stores create some tables in another shape.
            try:
                harness.serve_legacy("stamp-legacy", legacy_database, OLD_LOGINS["legacy"],
                                     "stamp")
                harness.verdict("baseline.refused-by-one-login-postgres", False, "it started")
            except HarnessError as error:
                harness.verdict("baseline.refused-by-one-login-postgres",
                                "owner-phase" in str(error),
                                str(error).strip().splitlines()[-1][:150])

        if "upgraded" in selected and legacy_database is not None:
            harness.upgrade(legacy_database)
            dumps["upgraded"] = harness.dump_postgres("upgraded", legacy_database)
            harness.report("restricted-vs-upgraded", dumps["restricted"], dumps["upgraded"],
                           allow=ATTESTOR_COMMENT + SERVED_ROWS)
    finally:
        harness.cleanup()
    failed = [title for title, passed in harness.results if not passed]
    if failed:
        print(f"{len(failed)} check(s) failed: {', '.join(failed)}")
        return 1
    print(f"all {len(harness.results)} check(s) passed")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument(
        "--admin-url-env", default="WADDLE_BASELINE_ADMIN_URL", metavar="VARIABLE",
        help="environment variable that holds a superuser PostgreSQL URL of a "
             "disposable cluster (default: WADDLE_BASELINE_ADMIN_URL)",
    )
    parser.add_argument("--server-bin", required=True,
                        help="waddle-server built with the clustering feature")
    parser.add_argument("--keypool-bin", help="waddle-cluster-keypool (PostgreSQL paths)")
    parser.add_argument("--psql", help="psql executable (default: WADDLE_PSQL, then PATH)")
    parser.add_argument("--out", required=True, help="directory for the dumps and diffs")
    parser.add_argument("--paths", default="all",
                        help=f"`all`, `history`, or a comma-separated subset of {','.join(PATHS)}")
    parser.add_argument("--keep", action="store_true",
                        help="leave the databases, roles and working directory in place")
    try:
        return run(parser.parse_args(argv))
    except (HarnessError, schema_catalog.CatalogError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
