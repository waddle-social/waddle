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
schema owner. It runs the registered migrations, each store's owner
preparation (room observation, pending delivery, inbox, stream management,
PubSub, spaces, push registrations, notification activity, call teardown,
Push Service, notification outbox, VAPID, room effect outbox, MUC destroy
outbox, MAM and the webhook delivery ledger) and the runtime grants. A plan
with a clustering section also prepares the clustering stores; see
[Clustered owner phase](#clustered-owner-phase). The attestor is installed
afterwards by its own installer login.

The runtime additionally needs three operator inputs that are never inferred
from its connection: `WADDLE_DB_MIGRATOR_LOGIN` (the login that ran the owner
phase, checked by the room observation verifier), and
`WADDLE_XMPP_MAM_SCHEMA_OWNER` with `WADDLE_XMPP_MAM_CLAIM_SCHEMA`.

Startup refuses, before binding a listener, when clustering is enabled
together with the attested pool: the owner phase prepares the clustering
stores and each has a verifying attachment, but startup does not select
those attachments. A store whose relation is missing or drifted fails its
attachment; the runtime never creates it.

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
runtime connection. Old-writer drain and physical primary fencing remain
gates, as do the parts of typed transaction authority listed under
[Typed ingress transaction entry](#typed-ingress-transaction-entry). No role
or epoch activation is implied by this configuration or by a successful local
test.

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

### Clustered owner phase

`RestrictedRuntimeOwnerPlan::clustering` carries the configured keypair
pool's PeerIds in pool order, the keypair-pool generation, an activation
delay, the treatment of an unenrolled lease table and whether the phase
requires the database to itself. With it the owner phase, after the MUC
destroy outbox and before the grants:

- creates or completes the claim relations (`clustering_nodes`,
  `clustering_claims` and its epoch sequence, `clustering_steal_intents`,
  `clustering_orphan_reaper_cursors`). Retained rows are kept and the epoch
  sequence only moves forward, past every retained claim;
- enrolls or adopts the keypair pool (`clustering_keypool`,
  `clustering_keypool_slots`, `clustering_keypair_slots` and their guard
  triggers). An existing enrollment must name the plan's generation and
  PeerId order, or the phase fails with `EnrollmentMismatch` before anything
  else changes;
- makes `clustering_peer_allowlist` exactly the plan's PeerIds;
- creates or completes the durable room tables without rewriting a row.

Fenced stream management uses the four relations the portable preparation
creates, including the unique replay de-duplication index
`idx_sm_unacked_dedup`. If retained rows violate that index the phase fails
at the stream management step, before any clustering store or grant.

Every preparation is repeatable: a second owner phase on a prepared database
changes neither the catalog nor a row. Each refuses an attested runtime
handle. A plan without a clustering section prepares none of this and
refuses a database that holds a keypair-pool enrollment
(`KeypoolEnrolledWithoutClusteringPlan`).

The keypair guards are `SECURITY INVOKER`. The slot guard reads the
enrollment with the runtime's own `SELECT`, so room observation's refusal of
owner-privileged triggers on runtime-writable relations holds on an enrolled
database. A runtime without `SELECT` on the enrollment cannot write a lease:
the statement fails with `insufficient_privilege`. The owner phase converts
guards that carry `SECURITY DEFINER` before it runs the room observation
preparation, and verifies the exact guard pin when it adopts the enrollment.

The runtime attaches through verifiers that create nothing:

| Store | Attachment | Verified |
|---|---|---|
| Claims, nodes, steal intents, reaper cursors, allowlist, occupancy | `attach_main_claim_occupancy`, then `PostgresClaimStore::attach_runtime` | exact shape, schema-owner ownership and exact runtime privileges of each relation, the epoch sequence, and an attested control-plane pool on the same physical database |
| Keypair lease | `PostgresKeypairSlotLease::attach_existing` | lease and enrollment tables, guards, exact privileges, active generation, configured PeerId order, on both pools |
| Durable rooms | `PostgresMucRoomStore::attach_runtime` | room tables and both outboxes with exact privileges; the schema ACL admits only the runtime, the attestation definer and owner-granted non-grantable `USAGE` for `pg_monitor` |
| Fenced stream management | `PostgresFencedSmPersistence::attach_runtime` | the portable stream management snapshot with write privileges |

The room and stream management attachments take the claim evidence and
refuse evidence verified on another pool.

#### Upgrading a database a clustered server built under the schema owner

Such a database holds every relation in the shape that server created, with
rows. The owner phase adopts it in place; the claim, node, steal-intent,
reaper-cursor and room rows and the archive are unchanged afterwards. Stale
node and claim rows resolve through lease expiry and stealing, as after any
full restart. `xmpp_occupancy_authority` is not reset: a generation is
replaced by the next fresh bind of its full JID, a resumed session and a
host-owned occupant must keep theirs, and no generation is tied to a node
incarnation.

The exception is `clustering_keypair_slots`. The unattested server creates
it without the enrollment columns and guards, and its rows name no enrolled
generation. The plan decides what happens:

- `LegacyKeypairSlots::Refuse` (the default) fails the phase with
  `LegacyKeypairSlots` and leaves the table alone.
- `LegacyKeypairSlots::Retire { legacy_ttl }` drops and recreates the table
  in one transaction that holds `ACCESS EXCLUSIVE` on it, after checking
  that it has exactly the unattested shape (five columns, primary key, schema
  owner, no trigger, constraint or dependent object) and that no row is
  leased. A row is leased when it has a holder, is not marked expired and its
  heartbeat is newer than `legacy_ttl` on the database clock; a heartbeat
  ahead of the clock counts as leased. A leased row fails the phase with
  `LegacyLeaseLive`. The plan's activation delay must be at least
  `legacy_ttl`.

A holder identity is generated per process start, so a restarted process
cannot renew a surviving row; only a process that is still running can.

A newly enrolled generation is pending until its activation delay has
elapsed and the lease table is empty. While it is pending the phase applies
the grants and then fails with `KeypoolActivationPending`; the runtime
refuses to attach a pending generation. Running the phase again after the
delay activates it.

By default the clustered phase first requires that no other login has a
session on the database and that `pg_prepared_xacts` holds no row for it
(`DatabaseNotExclusive`). Sessions of the migrator login are not counted,
because the phase's own pooled sessions cannot be told apart from them.

The cutover runbook must therefore state:

1. Steps 1–4 of the coordinated cutover (stop the pods, rotate the old
   login's credential, terminate its backends, verify none remain and no
   prepared transaction is pending) are complete before the owner phase.
2. The old server did not connect as the migrator login.
3. `legacy_ttl` is at least the `WADDLE_CLUSTERING_LEASE_TTL_MS` the old
   server ran with (30 s unless configured), and the activation delay is at
   least `legacy_ttl`.
4. Retirement discards the old lease rows, including which node held which
   slot.
5. The owner phase is run again after the activation delay, and the new
   version starts only after that run succeeds.

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
| none | `messages`, `attachments`, `reactions`, `p21_receipt_operation_results`, every other `clustering_*` relation a database can hold, `sm_revision_authority`, `sm_revision_generations`, `sm_revision_operations` |
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

A plan with a clustering section replaces the clustering rows:

| Privileges | Relations |
|---|---|
| `SELECT` | `clustering_peer_allowlist`, `clustering_keypool`, `clustering_keypool_slots` |
| `SELECT`, `INSERT`, `UPDATE` | `clustering_nodes`, `clustering_orphan_reaper_cursors`, `clustering_keypair_slots` |
| `SELECT`, `INSERT`, `DELETE` | `clustering_muc_admin_receipts` |
| `SELECT`, `INSERT`, `UPDATE`, `DELETE` | `clustering_claims`, `clustering_steal_intents`, `clustering_muc_rooms`, `clustering_muc_room_affiliations`, `clustering_muc_room_lifecycles` |
| sequence `SELECT`, `USAGE` | `clustering_claim_epoch_seq` |

These rows follow the same enumeration run over the clustering, durable room
and fenced stream management modules. Node and reaper-cursor rows and
keypair leases are upserted and never deleted. Affiliations are upserted and
deleted. Admin receipts are inserted and deleted, never updated. Lifecycle
rows are inserted, updated and locked; no runtime statement deletes one, and
the row carries `DELETE` only because the room schema verifier requires that
exact set. The revisioned stream management tables and the inter-server
resume tables stay without privileges: no serving path selects the first,
and the registered migrations drop the second. The stream management,
occupancy and outbox rows are the same with and without clustering.

## Typed ingress transaction entry

On the serving pool every ingress transaction that attests its connection
begins in `ingress_substrate::typed_entry::begin_ingress_entry`. The ingress
unit of work, the pooled effect-receipt writer and its post-commit readback
all call it. So do the raw ingress writers when their database handle is an
attested pool: `ingress_uow::begin_raw_ingress_writer`, which the notification
and push outboxes use, and `PostgresIngressSubstrate::begin`. On a handle
without attestation those two keep their own sequence, which for the raw
writer first probes for the descendant foundation so a standalone store
without it stays usable. A request names, as typed values, what the callers
differ in:

- the authority: unattested (SQLite, or PostgreSQL without attestation), the
  registered V1009 guard on an attested pool, or the candidate relation guard;
- how the transaction is acquired, and an optional acquisition deadline;
- optional explicit `lock_timeout` and `statement_timeout` values;
- the clamp an attested transaction applies: 100 ms and 250 ms for writers,
  3 s and 5 s for the relation guard, or none, in which case the explicit
  timeouts are the bounds exactly as given.

The alias GC uses the last form. On an attested pool
`gc_candidate_batch`, which deletes expired `ingress_messages` rows with
their aliases and deliveries, and `gc_orphan_proof_batch`, which deletes
orphaned `sm_ingress_appends` rows, begin through the entry with the GC's own
lock and statement budgets (100 ms and 250 ms in the maintenance
coordinator), so the entry's preparation runs under those budgets. A database
failure during preparation reaches the GC with its driver diagnostics, so it
still reports a lock timeout and a statement timeout separately.
`expired_candidates`, the scan that finds candidates, writes nothing and
opens its own read transaction. A separate test in
`tests/restricted_runtime_startup.rs` records a message on the attested pool,
terminalizes it past retention and runs a GC pass under the runtime role; the
pass deletes the message and draws no error on a runtime connection.

On an attested pool with the registered guard the entry sends, in order:
`BEGIN`; `SET TRANSACTION ISOLATION LEVEL READ COMMITTED`; the explicit
timeouts, if any; the clamp; `LOCK TABLE ONLY <schema>.ingress_protocol_epoch
IN ACCESS SHARE MODE`; the physical attestor; the schema OID and isolation
checks; the transaction-local `search_path`; the epoch read; the
`waddle.protocol_epoch` proof settings; and the epoch row read `FOR SHARE`
that the V1009 guard trigger otherwise requests on each protected write. It
then refuses an epoch above the one the binary implements. Without
attestation it takes the epoch row lock first and, on PostgreSQL, installs
the proof settings; SQLite sends only its write-intent `BEGIN` and the epoch
read.

The entry returns an `IngressEntryTransaction`. On an attested pool it carries
a `PreparedAttestedReceiptWriter`, the witness the ingress repositories use to
address schema-qualified relations. Only the entry constructs either type, and
`IngressUowTransaction` stores the entry's value, so a unit-of-work
transaction cannot be assembled around a transaction that skipped the entry.
A commit error is returned as it is; each caller keeps its own policy for a
commit whose outcome is unknown.

`server/scripts/check-ingress-typed-entry.py` runs in the clippy lanes. In
`ingress/`, `ingress_uow/` and `ingress_substrate/`, outside the entry, it
fails on four kinds of code: a call of `attest_transaction(`; a transaction
opened directly on a database handle; a call that takes the epoch row lock
without the entry's witness; and an autocommit connection taken with
`.guard()`. Test code is exempt from the last three. Every exception is
listed by file, kind and exact
count, with its reason, in
`server/scripts/ingress-typed-entry-allowlist.txt`.

What the entry does not do:

- It attests the physical connection identity and lineage, not the migration
  ledger or the epoch guard manifest; those are checked once by the startup
  preflight.
- The pool type still exposes raw `begin`, `execute` and `query`. A raw
  writer that began through the entry gets back a plain transaction, and its
  later statements are not schema-qualified; they resolve through the
  `search_path` the entry pinned.
- The ingress maintenance scans, the room-observation retention batch and
  three pooled single-statement reads run outside the entry. None touches a
  relation the epoch guard protects, and the allowlist names each.
- The `*_raw` descendant repositories
  (`EffectDescendantRepository::lock_raw`, `attach_raw`, `copy_raw`,
  `settle_all_raw` and their siblings) accept any transaction and install
  the epoch proof on it themselves. The notification and push outboxes call
  them from outside the ingress modules, where the source check does not
  look, so nothing yet proves that every such transaction on an attested
  pool began through the entry.
- Attach-time checks in other stores call the attestor directly on their own
  read-only transactions.
- The clustered stores are refused under the attested pool and have no entry.

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
attaches through `PostgresMucRoomStore::attach_runtime`; its owner
constructor `open` refuses the attested pool.

Disposable PostgreSQL tests use separate owner and restricted logins to check
successful same-pool attachment, DDL denial, malformed shape and grants,
owner schema changes racing with the attachment lock, and ordinary row writes
during attachment. SQLite constructor behavior remains covered. Passing these
tests does not permit clustered serving with the attested pool; clustered
startup does not select the clustering attachments.
