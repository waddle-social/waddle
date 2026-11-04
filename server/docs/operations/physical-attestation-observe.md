# Read-only physical-attestation observation

`waddle-attestation-observe` provides a one-shot observation of the P2.1
physical-attestation installation. It does not enable a writer, verify runtime
readiness, change roles or grants, install anything, run migrations, or start
server services. Its JSON output is diagnostic evidence and is never an
activation token.

The operator must obtain the complete expected lineage and PostgreSQL identity
from an independent, trusted owner handoff. Reading the identity from the same
endpoint being examined is not an independent handoff. The original installation
operation UUID must also come from the installer caller. The observer compares
the exact operation marker and installation definition, but never calls the
installed SECURITY DEFINER function, even when its catalog definition matches.

Use one direct connection to the authoritative primary, authenticate its TLS
certificate and hostname with `sslmode=verify-full`, and separately establish
primary fencing. `--direct-endpoint` records the operator's assertion; neither
TLS nor this program can prove that a proxy is absent, that a primary is fenced,
or that an administrator is not concurrently changing objects or grants. The
observer assumes trusted administration excludes such non-cooperating changes
throughout the observation. It takes the same lineage and installer advisory
locks as cooperating operations, followed by one read-only repeatable-read
transaction and a relation lock. These transient locks can delay competing
operations. Row-security filtering is rejected instead of silently accepted.
The result is confined to this session and snapshot; it cannot establish future
identity, callable function behavior, migration completeness, writer exclusion,
lease state, endpoint topology, or runtime authorization.

## Connection and resource prerequisites

The preprovisioned restricted installer login and definer role must satisfy the
existing installer role, schema and read-grant checks. PostgreSQL 17 or newer is
required. Before running the program, configure these nonzero limits on the
installer role or database; smaller inherited values are accepted and preserved:

| PostgreSQL setting | Maximum accepted value |
| --- | --- |
| `lock_timeout` | 2 seconds |
| `statement_timeout` | 5 seconds |
| `idle_in_transaction_session_timeout` | 10 seconds |
| `idle_session_timeout` | 10 seconds |
| `transaction_timeout` | 20 seconds |

The diagnostic refuses an unset, zero, unrecognized or larger bound instead of
replacing it. This preserves stricter inherited limits, including before the
first catalog or lock query. The first commands normalize `search_path` and read
these settings; a role with different effective settings is indeterminate. The
outer client deadline is 30 seconds. On cancellation or a lost connection, the
server-side timeouts bound in-flight statements, transaction lifetime and idle
sessions, including release of session advisory locks; connection establishment
itself is subject to the outer deadline. A severely unhealthy backend or network
is still subject to PostgreSQL and operating-system behavior, not a guarantee of
instantaneous cleanup.

Supply the database URL only through `WADDLE_ATTESTATION_OBSERVE_DATABASE_URL`.
Use TCP, a trusted CA, and an explicit database and installer login. Startup
`options` and `PGOPTIONS` are rejected so they cannot override inherited bounds
or select another role. Embedded NULs in effective connection names are also
rejected before connecting, because PostgreSQL startup fields are NUL-terminated.
Keep credentials out of arguments and shell history.
For example, with independently obtained values in the operator's environment:

```sh
nix develop --command cargo build --locked -p waddle-server --bin waddle-attestation-observe
export WADDLE_ATTESTATION_OBSERVE_DATABASE_URL='postgresql://installer@db.example.test/waddle?sslmode=verify-full&sslrootcert=/private/ca.pem'
target/debug/waddle-attestation-observe \
  --direct-endpoint \
  --operation-id "$INSTALL_OPERATION_UUID" \
  --expected-deployment-uuid "$EXPECTED_DEPLOYMENT_UUID" \
  --expected-lineage-uuid "$EXPECTED_LINEAGE_UUID" \
  --expected-system-identifier "$EXPECTED_SYSTEM_IDENTIFIER" \
  --expected-database-oid "$EXPECTED_DATABASE_OID" \
  --expected-database-name "$EXPECTED_DATABASE_NAME" \
  --expected-schema-oid "$EXPECTED_SCHEMA_OID" \
  --expected-schema-name "$EXPECTED_SCHEMA_NAME" \
  --installer-login "$INSTALLER_LOGIN" --runtime-login "$RUNTIME_LOGIN" \
  --definer-role "$DEFINER_ROLE" --trusted-schema "$TRUSTED_SCHEMA"
```

The program emits a single timestamped JSON object containing the supplied
operation and complete expected identity, role names, and the effective endpoint
host, port, database, and TLS mode. The direct/fenced condition is explicitly an
operator assertion. It never emits the connection URL or credentials. Treat the
infrastructure identity and endpoint details as sensitive operational metadata.
If configuration fails before the scope is parsed, `scope` is null. Exit codes are:

| Code | Status | Meaning |
| --- | --- | --- |
| 0 | `observed` | The checked lineage, physical identity, role prerequisites and catalog definition matched the independently supplied specification and operation in this observation. |
| 1 | `conflict` | A checked identity, prerequisite, operation marker or catalog definition differed. |
| 2 | `indeterminate` | Configuration, connection, bounds, locking, permissions, missing identity or another read prevented a reliable observation. |

A missing function is `indeterminate` (`installation_not_visible`), never
proof of rollback: the original installation request may still commit. A
conflict also does not establish why drift occurred. Preserve the original
operation UUID and investigate through the separately controlled owner process.

## Focused verification

The `operator_observer_tests` cases in the `physical_attestation` integration
binary require the repository's native PostgreSQL test fixture and its
`WADDLE_TEST_REQUIRE_POSTGRES` enforcement. They exercise the real CLI and
catalog using a test-only TLS terminator; that fixture does not qualify a proxy
for production use. The cases cover absent and matching operations, wrong
identity and operation, catalog drift, spoofed functions, hostile inherited
search paths, startup-parameter injection, tighter limits, cancellation and
relevant catalog snapshots.
