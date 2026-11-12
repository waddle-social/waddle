#!/usr/bin/env python3
"""Build the schema a deployment has today and dump its catalog.

Each path starts from an empty database and reaches the served schema the
way a deployment does, by running the real server binary:

  legacy      PostgreSQL, clustered, one login that owns everything. The
              operator's `waddle-cluster-keypool` enrollment SQL is applied,
              then the server starts without an attestation, runs the
              migrations and every store's own schema bootstrap, and is
              stopped once it reports ready.
  restricted  PostgreSQL, clustered, separate owner and runtime roles on a
              database that never held the legacy shape:
              `sql/restricted_runtime_provisioning.sql`, then
              `waddle-server owner-phase`, then `install-attestor`.
  upgraded    The legacy database carried through the same provisioning,
              owner phase and attestor installation. This is the shape an
              existing deployment has after its cutover.
  sqlite      A SQLite file: the server starts, migrates, bootstraps every
              store and is stopped once it reports ready.

Every path is dumped with `schema_catalog.py`, with row counts, into the
output directory, and the PostgreSQL paths are compared with each other.

The harness creates databases named `waddle_baseline_*` and roles named
`waddle_baseline_*` through the administrator URL and drops them again
unless `--keep` is given. It needs a superuser: only one can create roles
and run the provisioning procedure. It must never be pointed at a cluster
that serves anything.

The server needs SpiceDB unless its fixed test account is enabled, so the
harness enables it. That adds rows (one account), never schema.
"""

from __future__ import annotations

import argparse
import os
import shutil
import signal
import subprocess
import sys
import tempfile
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

PATHS = ("legacy", "restricted", "upgraded", "sqlite")

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
OLD_LOGIN_LEGACY = PREFIX + "app_legacy"
OLD_LOGIN_RESTRICTED = PREFIX + "app_restricted"
# Both old logins are recorded under one name, so the two databases compare
# equal where only the old login's name differs.
ROLE_ALIASES = {
    OLD_LOGIN_LEGACY: PREFIX + "app",
    OLD_LOGIN_RESTRICTED: PREFIX + "app",
}


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

    def environment(self, extra: dict[str, str]) -> dict[str, str]:
        """A child environment with no ambient `WADDLE_*`, `OTEL_*` or `PG*`."""
        environment = {
            key: value
            for key, value in os.environ.items()
            if not key.startswith(("WADDLE_", "OTEL_", "PG"))
        }
        environment.update(extra)
        return environment

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

    def serve_until_ready(self, label: str, extra: dict[str, str]) -> None:
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
                "WADDLE_GIT_SHA": "0" * 40,
                "WADDLE_SESSION_KEY": SESSION_KEY,
                "WADDLE_OCCUPANT_ID_SECRET": OCCUPANT_ID_SECRET,
                "WADDLE_DEPLOYMENT_UUID": DEPLOYMENT_UUID,
                "WADDLE_DB_LINEAGE_ACTION": "enroll",
                **extra,
            }
        )
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

    def build_legacy(self) -> str:
        self.keypair_pool()
        self.create_role(OLD_LOGIN_LEGACY, login=True)
        database = self.create_database("legacy", OLD_LOGIN_LEGACY)
        login_url = self.url(database, OLD_LOGIN_LEGACY)
        assert self.pool is not None and self.enrollment_sql is not None
        schema_catalog.run_psql(self.psql, login_url, self.enrollment_sql)
        self.serve_until_ready(
            "legacy",
            {
                "WADDLE_DB_DRIVER": "postgres",
                "WADDLE_DATABASE_URL": login_url,
                "WADDLE_CLUSTERING_ENABLED": "true",
                "WADDLE_CLUSTERING_KEYPAIR_POOL": self.pool,
                "WADDLE_CLUSTERING_LISTEN_ADDRS": "/ip4/127.0.0.1/tcp/0",
                "WADDLE_CLUSTERING_NODE_ID_FILE": str(self.work / "legacy-node-id"),
                "WADDLE_CLUSTERING_LEASE_TTL_MS": str(LEASE_TTL_MS),
                "WADDLE_CLUSTERING_HEARTBEAT_INTERVAL_MS": str(LEASE_TTL_MS // 4),
            },
        )
        return database

    def ensure_restricted_roles(self) -> None:
        if ROLES["schema_owner"] in self.roles:
            return
        for key in ("database_owner", "schema_owner", "attestor_definer_role"):
            self.create_role(ROLES[key], login=False)
        for key in ("migrator_login", "runtime_login", "attestor_installer_login"):
            self.create_role(ROLES[key], login=True)

    def provision(self, database: str, old_login: str) -> None:
        settings = {"schema": SCHEMA, "old_login": old_login, "attestor_schema": ATTESTOR_SCHEMA}
        settings.update(ROLES)
        prelude = "".join(
            f"SET waddle.{key} = {schema_catalog.quote_literal(value)};\n"
            for key, value in settings.items()
        )
        self.admin(prelude + PROVISIONING_SQL.read_text(encoding="utf-8"), database)

    def owner_phase(self, label: str, database: str, upgrade: bool) -> None:
        assert self.pool is not None
        identity = self.work / f"{label}-identity.json"
        role_environment = {
            "WADDLE_XMPP_MAM_CLAIM_SCHEMA": SCHEMA,
            "WADDLE_DB_MIGRATOR_LOGIN": ROLES["migrator_login"],
            "WADDLE_XMPP_MAM_SCHEMA_OWNER": ROLES["schema_owner"],
            "WADDLE_DB_RUNTIME_LOGIN": ROLES["runtime_login"],
            "WADDLE_DB_ATTESTOR_DEFINER_ROLE": ROLES["attestor_definer_role"],
            "WADDLE_DB_ATTESTOR_INSTALLER_LOGIN": ROLES["attestor_installer_login"],
            "WADDLE_DB_ATTESTOR_SCHEMA": ATTESTOR_SCHEMA,
            "WADDLE_DEPLOYMENT_UUID": DEPLOYMENT_UUID,
        }
        owner_environment = {
            **role_environment,
            "WADDLE_DATABASE_URL": self.url(database, ROLES["migrator_login"]),
            "WADDLE_SESSION_KEY": SESSION_KEY,
            "WADDLE_CLUSTERING_ENABLED": "true",
            "WADDLE_CLUSTERING_KEYPAIR_POOL": self.pool,
            "WADDLE_CLUSTERING_KEYPOOL_GENERATION": str(uuid.uuid4()),
            "WADDLE_CLUSTERING_LEASE_TTL_MS": str(LEASE_TTL_MS),
            "WADDLE_CLUSTERING_HEARTBEAT_INTERVAL_MS": str(LEASE_TTL_MS // 4),
        }
        arguments = ["owner-phase", "--identity-output", str(identity)]
        if upgrade:
            # The legacy server created the keypair-slot table, so the phase
            # retires it; the lineage row already names this deployment.
            arguments += ["--legacy-keypair-slots", "retire",
                          "--legacy-ttl", f"{LEASE_TTL_MS}ms"]
        else:
            owner_environment["WADDLE_DB_LINEAGE_ACTION"] = "enroll"
        for attempt in range(6):
            completed = self.subcommand(f"{label}-owner-phase-{attempt}", arguments, owner_environment)
            if completed.returncode == 0:
                break
            if completed.returncode != 3:
                raise HarnessError(
                    f"{label}: owner-phase exited with {completed.returncode}:\n"
                    f"{completed.stderr[-4000:]}"
                )
            # Exit 3: the keypair-pool generation is pending its activation
            # delay. A rerun after the delay activates it.
            time.sleep(LEASE_TTL_MS / 1000 + 1)
        else:
            raise HarnessError(f"{label}: the keypair-pool generation never activated")
        completed = self.subcommand(
            f"{label}-install-attestor",
            ["install-attestor", "--identity", str(identity),
             "--operation-id", str(uuid.uuid4())],
            {**role_environment,
             "WADDLE_DATABASE_URL": self.url(database, ROLES["attestor_installer_login"])},
        )
        if completed.returncode != 0:
            raise HarnessError(
                f"{label}: install-attestor exited with {completed.returncode}:\n"
                f"{completed.stderr[-4000:]}"
            )

    def build_restricted(self) -> str:
        self.keypair_pool()
        self.create_role(OLD_LOGIN_RESTRICTED, login=True)
        database = self.create_database("restricted", OLD_LOGIN_RESTRICTED)
        self.ensure_restricted_roles()
        self.provision(database, OLD_LOGIN_RESTRICTED)
        self.owner_phase("restricted", database, upgrade=False)
        return database

    def upgrade(self, database: str) -> None:
        self.ensure_restricted_roles()
        self.provision(database, OLD_LOGIN_LEGACY)
        self.owner_phase("upgraded", database, upgrade=True)

    def build_sqlite(self) -> Path:
        path = self.work / "sqlite-history.db"
        self.serve_until_ready(
            "sqlite",
            {"WADDLE_DB_DRIVER": "sqlite", "WADDLE_DATABASE_URL": f"sqlite://{path}?mode=rwc"},
        )
        return path

    # -- dumps --------------------------------------------------------------

    def dump_postgres(self, name: str, database: str) -> Path:
        document = schema_catalog.dump_postgres(
            self.url(database), self.psql, SCHEMA, ROLE_ALIASES, row_counts=True
        )
        path = self.out / f"postgres-{name}.json"
        path.write_text(schema_catalog.render(document), encoding="utf-8")
        print(f"wrote {path}")
        return path

    def report(self, title: str, left: Path, right: Path, structure_only: bool) -> None:
        a, b = schema_catalog.load(str(left)), schema_catalog.load(str(right))
        if structure_only:
            a, b = schema_catalog.structure_only(a), schema_catalog.structure_only(b)
        lines = schema_catalog.compare(a, b, include_row_counts=True)
        path = self.out / f"{title}.diff"
        path.write_text("".join(line + "\n" for line in lines), encoding="utf-8")
        print(f"{title}: A={left.name} B={right.name}: "
              f"{schema_catalog.summarise(lines)} ({path})")


def _tail(path: Path, lines: int = 25) -> str:
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return ""
    return "\n".join(text.splitlines()[-lines:])


def run(arguments: argparse.Namespace) -> int:
    selected = [name.strip() for name in arguments.paths.split(",") if name.strip()]
    unknown = sorted(set(selected) - set(PATHS))
    if unknown:
        raise HarnessError(f"unknown path(s) {unknown}; choose from {list(PATHS)}")
    if "upgraded" in selected and "legacy" not in selected:
        raise HarnessError("the upgraded path starts from the legacy path; select both")
    harness = Harness(arguments, postgres=selected != ["sqlite"])
    dumps: dict[str, Path] = {}
    try:
        legacy_database = None
        if "legacy" in selected:
            legacy_database = harness.build_legacy()
            dumps["legacy"] = harness.dump_postgres("legacy", legacy_database)
        if "restricted" in selected:
            dumps["restricted"] = harness.dump_postgres(
                "restricted", harness.build_restricted()
            )
        if "upgraded" in selected and legacy_database is not None:
            harness.upgrade(legacy_database)
            dumps["upgraded"] = harness.dump_postgres("upgraded", legacy_database)
        if "sqlite" in selected:
            path = harness.out / "sqlite-history.json"
            document = schema_catalog.dump_sqlite(str(harness.build_sqlite()), row_counts=True)
            path.write_text(schema_catalog.render(document), encoding="utf-8")
            print(f"wrote {path}")
        if "legacy" in dumps and "restricted" in dumps:
            harness.report("legacy-vs-restricted.structure", dumps["legacy"],
                           dumps["restricted"], structure_only=True)
        if "upgraded" in dumps and "restricted" in dumps:
            harness.report("restricted-vs-upgraded.structure", dumps["restricted"],
                           dumps["upgraded"], structure_only=True)
            harness.report("restricted-vs-upgraded.full", dumps["restricted"],
                           dumps["upgraded"], structure_only=False)
    finally:
        harness.cleanup()
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
    parser.add_argument("--paths", default=",".join(PATHS),
                        help=f"comma-separated subset of {','.join(PATHS)}")
    parser.add_argument("--keep", action="store_true",
                        help="leave the databases, roles and working directory in place")
    try:
        return run(parser.parse_args(argv))
    except (HarnessError, schema_catalog.CatalogError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
