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
PostgreSQL terminated the original backend. A successful preflight yields a sealed preparation
marker, which `RuntimeStartupIdentity::attach_serving` consumes to build the
serving pool: the attested, non-pinned pool, whose every checkout attests the
login, schema and lineage. The pinned pool never serves. Startup then attaches
each store to relations the owner phase prepared; it runs no DDL, and the
owner startup is not a fallback from a refused attachment.
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

## Owner phase and runtime attachment

The runtime role holds no DDL privilege. A separate owner phase,
`server::runtime_owner::prepare_restricted_runtime_owner`, runs on a handle
whose session user is the migrator login and whose current role is the NOLOGIN
schema owner. It runs the registered migrations, each non-clustered store's
owner preparation (room observation, pending delivery, inbox, stream
management, PubSub, spaces, push registrations, notification activity, call
teardown, Push Service, notification outbox, VAPID, room effect outbox, MUC
destroy outbox, MAM and the webhook delivery ledger) and the runtime grants.
The attestor is installed afterwards by its own installer login.

The runtime additionally needs three operator inputs that are never inferred
from its connection: `WADDLE_DB_MIGRATOR_LOGIN` (the login that ran the owner
phase, checked by the room observation verifier), and
`WADDLE_XMPP_MAM_SCHEMA_OWNER` with `WADDLE_XMPP_MAM_CLAIM_SCHEMA`.

Startup refuses, before binding a listener, when clustering is enabled
together with the attested pool: the clustering stores (keypair lease, claims,
durable MUC rooms, fenced stream management) have no owner-prepared
attachment. A store whose relation is missing or drifted fails its attachment;
the runtime never creates it.

The non-clustered server serves under the runtime role.
`tests/restricted_runtime_startup.rs` starts the real binary on the attested
pool against a database seeded with a dangling roster row, probes `/healthz`
and `/readyz`, and runs WebSocket sessions that cover a direct message, roster
set/get with its push, vCard, private XML storage, the block list, a room
created, joined, written to, read back through MAM and left, a personal MAM
query, an upload slot, push device registration with enable and disable, and
XEP-0198 enable, acknowledgement and same-node resumption. It then checks that
the catalog is unchanged and that a relay in front of PostgreSQL saw no
catalog-changing statement and no `insufficient_privilege` error on any
runtime connection. Old-writer drain, physical primary fencing and typed
transaction authority remain gates. No role or epoch activation is implied by
this configuration or by a successful local test.

Three verifier rules make the joint role graph satisfiable. Each admits one
exact shape and refuses any deviation:

- **Schema ACL.** MAM attachment admits `pg_monitor` in the schema ACL only
  for owner-granted, non-grantable `USAGE`, the grant room observation
  preparation issues and verifies. MAM's table and column ACL checks admit
  only the owner and the runtime.
- **Attach-lock function.** Room observation verification refuses a runtime
  that can execute an owner-owned `SECURITY DEFINER` routine, except the
  PubSub attach-lock function in its exact owner-prepared form: name, schema,
  no arguments, `sql`, returns `void`, `search_path=pg_catalog, pg_temp`,
  exact body, and an ACL of the owner plus one non-grantable runtime
  `EXECUTE`. Both verifiers build on one predicate in
  `pubsub::runtime_attach`.
- **Epoch row.** The startup preflight runs the pinned catalog check in the
  V1009 stage. It requires table-level `SELECT` on `ingress_protocol_epoch`
  plus exactly one column item, a non-grantable owner-granted `UPDATE (id)`,
  which the registered guard and the ingress substrate need to read the row
  `FOR SHARE`. `CHECK (id = 1)` and the guard trigger keep that grant from
  changing the epoch. A database granting `SELECT` alone, table-level
  `UPDATE`, or `UPDATE` on another column fails the preflight. The guard
  manifest is `SELECT`-only. The pinned receipt and selected-entry paths keep
  the `SELECT`-only stage.

The target schema must not grant anything to PUBLIC. The owner phase refuses
such a schema, which includes the default `public` schema, before it runs any
DDL.

### Runtime grant matrix

The owner phase classifies every table and sequence in the schema before it
grants anything. A relation the matrix does not name fails the phase with
`UnclassifiedRelation`; a privilege the runtime already holds beyond the
matrix, or any privilege held with the grant option, fails it with
`ExcessRuntimePrivilege` and is left in place. The phase ends by checking the
runtime role as the attachments will (login attributes, no role membership,
no database or schema CREATE, no TEMP, no default privileges) and fails with
`UnsafeRuntimeRole` otherwise. Row locks
and `ON CONFLICT DO UPDATE` count as `UPDATE`. Deletes that happen only
through `ON DELETE CASCADE` need no grant.

| Privileges | Relations |
|---|---|
| `SELECT` | `_lineage`, `_migrations`, `_waddle_space_store_owner`, `notification_activity_owner_state`, `notification_outbox_owner_state`, `pending_delivery_startup_migrations`, `pubsub_schema_marker`, `pubsub_schema_version`, `push_service_schema_marker`, `push_service_vapid_schema_marker`, `push_service_vapid_keys`, `ingress_epoch_guard_manifest`, `extension_room_observation_owner_state` |
| `SELECT`, `UPDATE (id)` | `ingress_protocol_epoch` |
| `SELECT`, `INSERT` | `ingress_owner_rejections`, `ingress_effect_receipts`, `ingress_carbon_receipts`, `ingress_delivery_receipts`, `ingress_archive_dispatch`, `muc_invite_claims`, `pubsub_owner_configured` |
| `SELECT`, `INSERT`, column `UPDATE` | `mam_messages` (`body`, `stanza_xml`, `thread_id`, `parent_thread_id`, `reply_to_id`, `reply_to_jid`, `rich_payload`), `mam_archive_sequences` (`next_seq`) |
| `SELECT`, `INSERT`, `UPDATE` | `users`, `auth_identities`, `private_xml_storage`, `user_avatar_source`, `user_avatar_fetch_state`, `upload_slots`, `link_preview_media_refs`, `xmpp_occupancy_authority`, `extension_grants`, `provider_webhook_deliveries`, `ingress_effect_intents`, `ingress_effect_descendants`, `inbox_entries`, `pubsub_push_publications`, `dnd_projection_source_version`, `notification_settings_projection_source_version`, `notification_activity`, `push_owner_locks`, `push_node_locks`, `push_publication_orders` |
| `SELECT`, `INSERT`, `DELETE` | `pending_auth`, `xmpp_auth_codes`, `blocking_list`, `extension_bot_rooms`, `ingress_sm_refs`, `ingress_deliveries`, `sm_unacked`, `sm_acknowledged_windows`, `pending_delivery_sm_promotions`, `pubsub_subscriptions`, `push_delivery_attempts` |
| `SELECT`, `INSERT`, `UPDATE`, `DELETE` | `ingress_origin_aliases`, `roster_versions`, `native_users`, `sessions`, `device_auth`, `permission_tuples`, `roster_items`, `vcard_storage`, `channels`, `muc_pending_invites`, `group_dm_archive_boundaries`, `webhook_deliveries`, `ingress_messages`, `ingress_sm_streams`, `ingress_send_attempts`, `groupchat_notification_recovery`, `sm_sessions`, `sm_ingress_appends`, `pending_delivery`, `pubsub_nodes`, `pubsub_items`, `pubsub_affiliations`, `dnd_projection`, `notification_settings_projection`, `spaces_metadata`, `channel_space_links`, `push_registrations`, `notification_candidates`, `notification_outbox`, `notification_outbox_lineage`, `push_nodes`, `push_devices`, `push_publish_jobs`, `call_teardown_outbox`, `clustering_muc_room_effects`, `clustering_muc_destroy_outbox`, and the six room observation data tables |
| none | `messages`, `attachments`, `reactions`, `p21_receipt_operation_results`, every other `clustering_*` relation, `sm_revision_authority`, `sm_revision_generations`, `sm_revision_operations` |
| sequence `USAGE` | `blocking_list_id_seq`, `native_users_id_seq`, `roster_items_id_seq`, `pending_delivery_notification_recovery_ordinal_seq` |
| sequence `SELECT` | `pubsub_items_seq_seq` |
| sequence none | `reactions_id_seq`, `clustering_claim_epoch_seq` |
| function `EXECUTE` | `pubsub_runtime_attach_lock()` |

The room observation tables take their grants from that store's own owner
preparation. Where a store's attachment verifies an exact set, the matrix is
that set. The remaining entries follow the statements the runtime issues:
every `INSERT`, `UPDATE`, `DELETE`, upsert and row-locking `SELECT` in the
server and `waddle-xmpp` sources outside test, migration, clustering-only and
pinned-path modules, including statements whose table name is formatted in.
`ingress_origin_aliases` needs `UPDATE` because alias resolution locks its row
with `FOR UPDATE OF`, and `roster_versions` needs `DELETE` because startup
prunes the version of an owner whose roster holds a dangling contact.

## Dark outbox attachment slice

The next restricted-startup slice splits call-teardown outbox owner preparation
from read-only attachment on the exact global main pool. Its attested branch
must return a functional store without opening another pool, running schema
DDL, repairing rows or falling back to owner initialization. The legacy SQLite
and owner-startup constructors remain available.

The owner prepares the outbox schemas separately. Runtime attachment verifies
the active database and main lane, required relation and index shape, effective
runtime grants and expected ownership under a bounded read-only transaction.
Missing or changed shape, wrong pool identity and excess runtime privileges
fail closed. Startup must retain the existing node identity.
If an existing outbox relation belongs to a different login, the trusted
provisioner must transfer it to the NOLOGIN schema owner before owner
preparation; the initializer will not silently reassign ownership.

Room-effect outbox attachment has two forms. `attach_existing` returns an
opaque verification marker; `attach_serving` returns the functional store
after the same verification, and queue mutation stays gated by the lifecycle
and claim authority each operation presents. The MUC-destroy attachment
preflight is not a replacement for the clustered MUC runtime store, which
still runs owner constructors and is refused under the attested pool.

Disposable PostgreSQL tests use separate owner and restricted logins to check
successful same-pool attachment, DDL denial, malformed shape and grants,
owner schema changes racing with the attachment lock, and ordinary row writes
during attachment. SQLite constructor behavior remains covered. Passing these
tests does not permit serving with the attested pool; the verifier conflicts
above and the clustered SM and claim/lease/MUC startup paths still need their
own reviewed authority and rollout evidence.
