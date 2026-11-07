# P2.1 restricted runtime startup

The attested path is selected by setting `WADDLE_DB_RUNTIME_ATTESTATION` to a
versioned JSON object supplied independently by the operator. It is not a
secret and must not be generated from the runtime connection being checked.
The following shape uses illustrative values:

```json
{
  "version": 3,
  "release_git_sha": "0123456789abcdef0123456789abcdef01234567",
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
  "cutover": {
    "transition_id": "018f47b2-4b2e-7a3a-9a4c-52a5a6a90004",
    "primary": {
      "provider": "example-provider",
      "cluster_id": "example-cluster",
      "instance_id": "example-primary",
      "endpoint_host": "db-primary.example.com",
      "endpoint_port": 5432,
      "endpoint_socket_dir": null,
      "observation_record_id": "018f47b2-4b2e-7a3a-9a4c-52a5a6a90005"
    },
    "old_runtime": {
      "login": "waddle_old_runtime",
      "role_oid": 16400,
      "credential_cutoff_record_id": "018f47b2-4b2e-7a3a-9a4c-52a5a6a90006"
    },
    "listener_cutoff_record_id": "018f47b2-4b2e-7a3a-9a4c-52a5a6a90007",
    "backend_cutoff_record_id": "018f47b2-4b2e-7a3a-9a4c-52a5a6a90008",
    "prepared_transaction_cutoff_record_id": "018f47b2-4b2e-7a3a-9a4c-52a5a6a90009",
    "mutator_policy": {
      "mode": "quiesced_for_serving_interval",
      "record_id": "018f47b2-4b2e-7a3a-9a4c-52a5a6a9000a"
    }
  },
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

These OIDs and record IDs are illustrative, not an installation recipe. A
trusted operator must record actual pins independently of the runtime
connection, after positive attestor installation or same-operation
reconciliation on the original primary. Version 1, version 2, and incomplete
handoffs are refused. The record IDs describe an operator procedure; the
runtime has no trusted signing key or anti-replay store and does not verify
that old credentials, backends, listeners, prepared transactions, or privileged
mutators were actually fenced or remain fenced. The old login/OID pair is
checked for contradictions against other name/OID pairs in this handoff;
it is not checked against a possibly retired live role.
Only `quiesced_for_serving_interval` is accepted: the current read-only startup
entry does not hold the shared catalog fence from before its checkout hook
through rollback. The operator must keep privileged catalog, ACL, role,
function, and concurrent-index mutation quiescent for the entire preflight
and any future selected serving interval. The runtime does not prove this
quiescence; `cooperative_catalog_fence_v1` is rejected.

`WADDLE_DEPLOYMENT_UUID` and the exact 40-character lowercase `WADDLE_GIT_SHA`
must match the handoff; enrollment or adoption actions are refused. The Git SHA
is supplied by the launcher and is not independently authenticated as an
artifact identity. The runtime PostgreSQL URL must name the recorded database
and runtime login and match the recorded host, port, and optional Unix socket
directory. A TCP endpoint requires `sslmode=verify-full`; a Unix socket depends
on operator-controlled filesystem permissions. The direct, session-preserving
primary and old-writer cutoff are external prerequisites.
The focused startup integration fixture requires a PostgreSQL test URL using
a Unix socket or authenticated TCP with `sslmode=verify-full`; a plain TCP
test URL cannot exercise its positive preflight case. The checked-in Nix
lane uses a Unix socket.

The selected restricted startup branch now opens a one-connection throwaway
pinned pool. Its hook checks physical identity before lending a connection;
the same backend runs a bounded read-only pinned ledger and epoch preflight,
then the runtime awaits SQLx pool close. That local completion does not prove
PostgreSQL terminated the original backend. A successful preflight yields only a sealed preparation
marker. Startup still exits with `UnconvertedStores` before listener
publication or effectful workers. No pool or serving authority is handed to
the application, and the legacy owner startup is not a fallback from refusal.
Cancellation, a lost response, or a pool-close timeout cannot prove that the
original backend stopped. Even a completed local close does not establish
server-side exit. Fence and reconcile the original primary and backend before
retrying an indeterminate attempt; do not treat a missing local
marker as rollback. Privileged DDL, ownership, grant, and concurrent-index
mutations need an enforceable catalog-fence protocol or independent quiescence
through any future serving interval.

The prior attachment implementation checks for unexpected triggers and rewrite
rules on inbox, portable SM and pending-delivery relations. Those checks remain
available to the legacy test path, but are not a serving authorization. An
epoch guard cannot be added to its allowlist until its definition, owner and
installation are verified.

**This is not yet a serving cutover.** The server refuses after the pinned
preflight and before binding a listener or running store constructors. MAM,
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
