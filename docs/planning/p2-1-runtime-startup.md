# P2.1 restricted runtime startup

The attested path is selected by setting `WADDLE_DB_RUNTIME_ATTESTATION` to a
versioned JSON object supplied independently by the operator. It is not a
secret and must not be generated from the runtime connection being checked.
The following shape uses illustrative values:

```json
{
  "version": 2,
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
  "trusted_schema": "waddle_attestation",
  "pins": {
    "database_owner_role_oid": 16386,
    "schema_owner_role_oid": 16387,
    "runtime_role_oid": 16388,
    "installer_role_oid": 16389,
    "definer_role_oid": 16390,
    "trusted_schema_oid": 16391,
    "lineage_relation_oid": 16392,
    "lineage_primary_index_oid": 16393,
    "attestor_function_oid": 16394,
    "installation_operation_id": "018f47b2-4b2e-7a3a-9a4c-52a5a6a90003",
    "ledger_relation_oid": 16395,
    "ledger_primary_index_oid": 16396,
    "epoch_relation_oid": 16397,
    "epoch_primary_index_oid": 16398,
    "guard_manifest_relation_oid": 16399
  }
}
```

These OIDs are illustrative, not an installation recipe. A trusted operator
must record the actual values independently of the runtime connection, after
positive attestor installation or same-operation reconciliation on the fenced
primary. Version 1 and incomplete pin bundles are refused.

`WADDLE_DEPLOYMENT_UUID` must equal the document's deployment UUID; enrollment
or adoption actions are refused in this mode. The runtime PostgreSQL URL must
name the expected database and login and select the expected schema when the
connection opens; the physical hook checks the active schema before normalizing
the search path. TCP connections require
`sslmode=verify-full`; Unix sockets depend on operator-controlled filesystem
permissions. A direct, session-preserving endpoint and the operator's fencing
of the authoritative primary and old sessions remain prerequisites. The
assertion records that requirement; it does not prove any of those conditions.

The v2 production attachment currently refuses before opening a connection.
Its former name-based, REPEATABLE READ lineage/ledger entry runs before a
pinned catalog proof, so it cannot safely consume the new pin bundle yet.
The opt-in dark hook and ledger helpers must be integrated into that bootstrap
entry and each transaction caller before this path may hand out a pool.
Attachment cannot reopen a DSN or fall back to owner bootstrap after a
refusal. Privileged DDL, ownership and grant changes must remain quiescent
through any future proof and use.

The prior attachment implementation checks for unexpected triggers and rewrite
rules on inbox, portable SM and pending-delivery relations. Those checks remain
available to the legacy test path, but are not a v2 serving authorization. An
epoch guard cannot be added to its allowlist until its definition, owner and
installation are verified.

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

The next restricted-startup slice splits call-teardown outbox owner preparation
from read-only attachment on the exact global main pool. Its attested branch
must return a functional store without opening another pool, running schema
DDL, repairing rows or falling back to owner initialization. The legacy SQLite
and owner-startup constructors remain available. `UnconvertedStores` stays
fatal before listeners until the other startup stores have equivalent
boundaries.

The owner prepares the outbox schemas separately. Runtime attachment verifies
the active database and main lane, required relation and index shape, effective
runtime grants and expected ownership under a bounded read-only transaction.
Missing or changed shape, wrong pool identity and excess runtime privileges
fail closed. Startup must retain the existing node identity.
If an existing outbox relation belongs to a different login, the trusted
provisioner must transfer it to the NOLOGIN schema owner before owner
preparation; the initializer will not silently reassign ownership.

Room-effect outbox attachment currently returns an opaque verification marker,
not a functional store. That type must remain non-mutating until lifecycle and
claim authority can be proven alongside its schema. The MUC-destroy attachment
preflight is likewise not a replacement for the MUC runtime store, which still
runs owner constructors. Neither path is activated in this slice.

Disposable PostgreSQL tests use separate owner and restricted logins to check
successful same-pool attachment, DDL denial, malformed shape and grants,
owner schema changes racing with the attachment lock, and ordinary row writes
during attachment. SQLite constructor behavior remains covered. Passing these
tests does not permit serving with the attested pool; RoomEffect, MAM, PubSub,
clustered SM, notification, push, observation and claim/lease/MUC startup paths
still need their own reviewed authority and rollout evidence.
