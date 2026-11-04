# P2.1 restricted runtime startup

The attested path is selected by setting `WADDLE_DB_RUNTIME_ATTESTATION` to a
versioned JSON object supplied independently by the operator. It is not a
secret and must not be generated from the runtime connection being checked.
The following shape uses illustrative values:

```json
{
  "version": 1,
  "operator_asserts_direct_fenced_primary": true,
  "deployment_uuid": "018f47b2-4b2e-7a3a-9a4c-52a5a6a90001",
  "lineage_uuid": "018f47b2-4b2e-7a3a-9a4c-52a5a6a90002",
  "system_identifier": "123456789",
  "database_oid": 16384,
  "database_name": "waddle",
  "schema_oid": 16385,
  "schema_name": "waddle",
  "installer_login": "waddle_attestor_installer",
  "runtime_login": "waddle_runtime",
  "definer_role": "waddle_attestor",
  "trusted_schema": "waddle_attestation"
}
```

`WADDLE_DEPLOYMENT_UUID` must equal the document's deployment UUID; enrollment
or adoption actions are refused in this mode. The runtime PostgreSQL URL must
name the expected database and login and select the expected schema when the
connection opens; the physical hook checks the active schema before normalizing
the search path. TCP connections require
`sslmode=verify-full`; Unix sockets depend on operator-controlled filesystem
permissions. A direct, session-preserving endpoint and the operator's fencing
of the authoritative primary and old sessions remain prerequisites. The
assertion records that requirement; it does not prove any of those conditions.

Before handing a pool to startup, the attested path checks each physical
connection, verifies the committed lineage and complete migration ledger,
checks the runtime role and read-only lineage/ledger privileges, and reserves
separate main and ingress admission on the same pool. Cluster mode also checks
a separately budgeted attested control-plane pool. Attachment cannot reopen a
DSN or fall back to owner bootstrap after a refusal. The checks are snapshots;
privileged DDL, ownership and grant changes must remain quiescent.

The production attachment checks for unexpected triggers and rewrite rules on
inbox, portable SM and pending-delivery relations. The only currently allowed
user trigger is pending delivery's separately verified immutable-ordinal
trigger. An epoch guard cannot be added to this allowlist until its definition,
owner and installation are verified. These checks do not establish an exact
schema, an independently attested relation owner, or a complete other-writer
ACL policy.

**This is not yet a serving cutover.** The server currently refuses an
attested pool before binding a listener or running store constructors. MAM,
PubSub, clustered SM, clustering claim/lease/MUC stores, push, observation and
outbox constructors still need owner-only preparation and verified runtime
attachments. The PostgreSQL relation-guard migration, old-writer drain,
physical primary fencing and complete epoch/ACL policy also remain gates.
Until those are resolved, the unconfigured legacy startup path still runs the
existing owner initialization. No role or epoch activation is implied by this
configuration or by a successful local test.

## Dark outbox attachment slice

The next restricted-startup slice covers the call-teardown and room-effect
outboxes on the exact global main pool. MUC-destroy completion already has a
read-only attachment path; startup can reuse it. The attested branch must
attach functional stores without opening another pool, running schema DDL,
repairing rows or falling back to owner initialization. The legacy SQLite and
owner-startup constructors remain available. `UnconvertedStores` stays fatal
before listeners until the other startup stores have equivalent boundaries.

The owner prepares the outbox schemas separately. Runtime attachment verifies
the active database and main lane, required relation and index shape, effective
runtime grants and expected ownership under a bounded read-only transaction.
Missing or changed shape, wrong pool identity and excess runtime privileges
fail closed. Startup must retain the existing node identity and outbox handles;
a verification marker alone is not a usable runtime store.

Disposable PostgreSQL tests use separate owner and restricted logins to check
successful same-pool attachment, DDL denial, malformed shape and grants,
owner schema changes racing with the attachment lock, and ordinary row writes
during attachment. SQLite constructor behavior remains covered. Passing these
tests does not permit serving with the attested pool; MAM, PubSub, clustered SM,
notification, push, observation and claim/lease/MUC startup paths still need
their own reviewed attachment and rollout evidence.
