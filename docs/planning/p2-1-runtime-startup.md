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
reconciliation on the original primary. `waddle-server emit-handoff` reads
them on the owner connection; see [Owner entrypoint](#owner-entrypoint). Version 1, version 2, and incomplete
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

A store whose relation is missing or drifted fails its attachment; the
runtime never creates it. With clustering enabled the runtime also attaches
the clustering stores: it opens a second attested pool under the same login
for node and claim liveness, verifies the claim, node, steal-intent,
reaper-cursor, allowlist and occupancy relations once, and attaches the
keypair lease, the claim store, the durable room store, fenced stream
management, claim-fenced MAM and claim-fenced pending delivery with that
evidence. A clustered start on a database prepared without the clustering
section, or on a keypair-pool generation that is still pending, refuses
before it publishes a listener.

The server serves under the runtime role, clustered or not.
`tests/restricted_runtime_startup.rs` starts the real binary on the attested
pool against a database seeded with a dangling roster row, probes `/healthz`
and `/readyz`, and runs WebSocket sessions that cover a direct message, roster
set/get with its push, vCard, private XML storage, the block list, a room
created, joined, written to, read back through MAM and left, a personal MAM
query, an upload slot, push device registration with enable and disable, and
XEP-0198 enable, acknowledgement and same-node resumption. It then checks that
the catalog is unchanged and that a relay in front of PostgreSQL saw no
catalog-changing statement and no `insufficient_privilege` error on any
runtime connection.

`tests/restricted_clustered_startup.rs` starts two real binaries under the
runtime role against a database prepared with a clustering plan, each
through its own relay. It runs direct messages in both directions between
clients on different nodes; a room created on one node and joined and
written to from the other, with the reflection and the owner's archive
read; XEP-0198 enabled on one node and resumed on the other; and a graceful
shutdown of the room's owning node, after which the surviving node delivers
a direct message, takes the room over, accepts a join and a message, serves
the archive, and bans an occupant, which commits an admin receipt. A
replacement node then creates a second room and is killed. The survivor
expires it, steals the room claim and serves the room; once the keypair
lease has run out a further node steals the dead node's slot and joins; and
the owner destroys the room and receives the destroy presence followed by
the IQ result. No relay may record an
`insufficient_privilege` error or a catalog-changing statement, the catalog
must be unchanged, and the owner phase must run again on the database
afterwards.

It does not cover an administrative room owner transfer, or the archive of
an instant room read from a node that does not own the room, which the
archive access gate refuses with or without the restricted role. The relay
classifies each statement by its first keyword after whitespace and
comments and counts a `DO` block as a catalog change; DDL run inside a
function a statement calls is visible only through the catalog comparison.

Old-writer drain and physical primary fencing remain gates, as do the parts
of typed transaction authority listed under
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
such a schema before it runs any DDL. The `public` schema carries PUBLIC
grants by default and is refused in that state; once they are revoked and
the schema belongs to the NOLOGIN schema owner it is accepted like any other
(see [Provisioning a database one login owns](#provisioning-a-database-one-login-owns)).

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
- enrolls the plan's PeerIds in `clustering_peer_allowlist`. An enrolled
  PeerId the plan does not name fails the phase with `UnexpectedPeers`
  before anything is written, so a plan built from the wrong keypair pool
  neither revokes serving peers nor leaves PeerIds outside the pool
  admitted. A plan with `allowlist: Reconcile` revokes them instead;
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
  heartbeat is no older than `legacy_ttl` on the database clock, the
  predicate the old server renews under; a heartbeat ahead of the clock
  counts as leased. A leased row fails the phase with `LegacyLeaseLive`.
  `legacy_ttl` below the plan's `lease_ttl` (the lease lifetime the servers
  are configured with, 30 s by default) is refused with
  `LegacyTtlBelowLeaseTtl`, and the plan's activation delay must be at least
  `legacy_ttl`. Every lock the owner steps wait for is bounded by a lock
  timeout that fails the step with a typed error.

A holder identity is generated per process start, so a restarted process
cannot renew a surviving row; only a process that is still running can.

A newly enrolled generation is pending until its activation delay has
elapsed and the lease table is empty. While it is pending the phase applies
the grants and then fails with `KeypoolActivationPending`; the runtime
refuses to attach a pending generation. Running the phase again after the
delay activates it.

By default the clustered phase requires the database to itself: no other
client session and no row in `pg_prepared_xacts` for it
(`DatabaseNotExclusive`). The caller opens the owner handle with an
`application_name` generated for the run and names it in the plan; every
session without that name counts, including one of the migrator login, and
a handle that does not carry the name is refused (`OwnerSessionUntagged`).
The check runs before the phase changes anything and again inside the
retirement transaction, once the lease table is locked.

A server that starts under the schema owner while other nodes serve reads
the claim schema's gaps from the catalog first and takes no table lock when
the schema is current. Each missing part is created in its own short
transaction under a lock timeout, so a bootstrap never holds a lock on
`clustering_nodes` while it waits for `clustering_claims`.

The cutover runbook must therefore state:

1. Steps 1–4 of the coordinated cutover (stop the pods, rotate the old
   login's credential, terminate its backends, verify none remain and no
   prepared transaction is pending) are complete before the owner phase.
2. The owner handle is opened with an `application_name` unique to the run,
   and the plan names it. `waddle-server owner-phase` generates the name and
   does both.
3. `legacy_ttl` is the `WADDLE_CLUSTERING_LEASE_TTL_MS` the old server ran
   with. The phase only enforces the lifetime the plan says the servers are
   configured with; if the old deployment had raised it, the operator
   supplies that larger value. The activation delay is at least
   `legacy_ttl`.
4. Retirement discards the old lease rows, including which node held which
   slot.
5. The owner phase is run again after the activation delay, and the new
   version starts only after that run succeeds.
6. Retirement is the rollback boundary. The enrolled lease table rejects
   the old server's five-column insert, so an old image cannot lease a
   keypair slot on an enrolled database and cannot start clustered. Rolling
   back to it means restoring the unenrolled table shape first.
7. The retirement shape check is written against PostgreSQL 17. PostgreSQL
   18 records NOT NULL constraints in `pg_constraint`, so the check finds
   constraints besides the primary key there and fails closed with
   `LegacyShapeMismatch`.

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
| `SELECT`, `INSERT`, `UPDATE` | `clustering_nodes`, `clustering_orphan_reaper_cursors`, `clustering_keypair_slots`, `clustering_muc_room_lifecycles` |
| `SELECT`, `INSERT`, `DELETE` | `clustering_muc_admin_receipts` |
| `SELECT`, `INSERT`, `UPDATE`, `DELETE` | `clustering_claims`, `clustering_steal_intents`, `clustering_muc_rooms`, `clustering_muc_room_affiliations` |
| sequence `SELECT`, `USAGE` | `clustering_claim_epoch_seq` |

These rows follow the same enumeration run over the clustering, durable room
and fenced stream management modules. Node and reaper-cursor rows and
keypair leases are upserted and never deleted. Affiliations are upserted and
deleted. Admin receipts are inserted and deleted, never updated. Lifecycle
rows are inserted, updated and locked; a destroyed room's lifecycle is
tombstoned, not deleted. The revisioned stream management tables and the inter-server
resume tables stay without privileges: no serving path selects the first,
and the registered migrations drop the second. The stream management,
occupancy and outbox rows are the same with and without clustering.

## Owner entrypoint

The owner phase, the attestor installation and the handoff are subcommands of
the `waddle-server` binary, so the release image that serves is the image
that prepares its database. `S/main.rs` dispatches on the presence of any
argument before it adopts listeners; without arguments the server starts. A
subcommand builds its own runtime, installs no telemetry exporter, binds
nothing and reads no serving configuration. Results go to standard output,
diagnostics to standard error.

| Subcommand | Login in `WADDLE_DATABASE_URL` | Does |
|---|---|---|
| `owner-phase` | migrator | `prepare_restricted_runtime_owner`: migrations, every store's owner preparation, the runtime grants. Prints the database identity (lineage and physical identity) as JSON |
| `install-attestor` | attestor installer | `physical_attestation::install` for the identity `owner-phase` printed, under an operator-chosen operation ID |
| `emit-handoff` | migrator | Composes the `WADDLE_DB_RUNTIME_ATTESTATION` document, or with `--verify` checks a stored one |

`owner-phase` and `emit-handoff` connect as the migrator and set
`role=<schema owner>`, `search_path=<schema>` and an `application_name`
generated for the run on their own sessions; `install-attestor` sets its
`search_path`. Nothing else may set them: every subcommand refuses, before
it connects, a URL that carries `options`, `options[...]` or
`application_name`, an environment in which `PGOPTIONS` or `PGAPPNAME` is
set, and a URL whose login is not the one it expects.

Connection URLs are taken from the environment only, never as arguments.
`--identity-output` and `--output` write a temporary file in the target's
directory and rename it over the target, so a rerun replaces the file
atomically; a target that is a symbolic link is refused.

### Inputs

Secrets have no flag and are never printed: `WADDLE_DATABASE_URL`,
`WADDLE_SESSION_KEY` (the VAPID root key), `WADDLE_CLUSTERING_KEYPAIR_POOL`
and the optional `WADDLE_RUNTIME_DATABASE_URL`. Role, schema and plan inputs
are flags with an environment fallback. Where the restricted server already
takes the same value, the subcommand reads the same variable, so one
environment block configures the job and the deployment:

| Flag | Environment | Used by |
|---|---|---|
| `--schema` | `WADDLE_XMPP_MAM_CLAIM_SCHEMA` | `owner-phase`, `emit-handoff` |
| `--migrator-login` | `WADDLE_DB_MIGRATOR_LOGIN` | `owner-phase`, `emit-handoff` |
| `--schema-owner` | `WADDLE_XMPP_MAM_SCHEMA_OWNER` | `owner-phase`, `emit-handoff` |
| `--runtime-login` | `WADDLE_DB_RUNTIME_LOGIN` | all three |
| `--attestor-definer-role` | `WADDLE_DB_ATTESTOR_DEFINER_ROLE` | all three |
| `--attestor-installer-login` | `WADDLE_DB_ATTESTOR_INSTALLER_LOGIN` | `install-attestor`, `emit-handoff` |
| `--attestor-schema` | `WADDLE_DB_ATTESTOR_SCHEMA` | `install-attestor`, `emit-handoff` |
| `--release-git-sha` | `WADDLE_GIT_SHA` | `emit-handoff` |
| `--keypool-generation` | `WADDLE_CLUSTERING_KEYPOOL_GENERATION` | `owner-phase` |

The first three and `WADDLE_GIT_SHA` are the variables the restricted server
reads. `owner-phase` also reads `WADDLE_DEPLOYMENT_UUID` and
`WADDLE_DB_LINEAGE_ACTION` through the server's lineage configuration, and
`WADDLE_CLUSTERING_ENABLED`, `WADDLE_CLUSTERING_KEYPAIR_POOL` and
`WADDLE_CLUSTERING_LEASE_TTL_MS` through its clustering configuration. With
clustering enabled the plan's PeerIds are derived from the keypair pool by
the function the runtime's lease attachment uses, so the enrollment cannot
name a pool the servers do not hold. A binary built without the `clustering`
feature refuses `WADDLE_CLUSTERING_ENABLED=true`.

The remaining clustering plan inputs are flags only:

| Flag | Default | Meaning |
|---|---|---|
| `--keypool-activation-delay <n>(ms\|s\|m\|h)` | `--legacy-ttl` when retiring, otherwise `0s` | How long a newly enrolled generation stays pending |
| `--legacy-keypair-slots refuse\|retire` | `refuse` | Treatment of a keypair-slot table an unattested server created |
| `--legacy-ttl <duration>` | none; required with `retire` | Lease lifetime the retired servers ran with |
| `--allowlist refuse\|reconcile` | `refuse` | Treatment of enrolled PeerIds outside the keypair pool |
| `--exclusive-database required\|not-checked` | `required` | Whether another session or a prepared transaction fails the phase |

The exclusivity check exists only with a clustering plan. A non-clustered
`owner-phase` does not check that it has the database to itself, so the
operator must have stopped every server first. `not-checked` disables the
only check that no server or operator session is still connected: a
connected old server can then write while the phase adopts or retires what
it holds. It is never used during a cutover.

`install-attestor` takes `--identity <file>` (the JSON `owner-phase` printed,
also written by `owner-phase --identity-output <file>`) and
`--operation-id <uuid>`. The expected identity is therefore the one attested
on the owner connection; the installer compares the live database with it
and does not read it back through its own login. The installation can be
repeated with the same operation ID.

### Exit codes

| Code | Meaning | Action |
|---|---|---|
| 0 | Completed | Next step |
| 1 | The connection or a statement failed, a lock stayed contended or timed out, or an outcome is unknown. Nothing says a rerun would fail | Read the diagnostic and run again; for `install-attestor`, with the same operation ID |
| 2 | Invalid arguments, environment or input file; nothing was sent to the database | Fix the invocation |
| 3 | `owner-phase` only: every schema is prepared and granted, and the keypair-pool generation is pending | Run `owner-phase` again after the printed activation time; the server refuses to start until that run exits 0 |
| 4 | A verifier refused the database, the plan or the document | A rerun refuses again until one of them changes |

How errors map (`S/owner_cli/error.rs`), searched along the whole chain of
causes:

| Error | Code |
|---|---|
| Missing or invalid environment, wrong login in the URL, smuggled session options, unreadable input file, output path that is a symbolic link | 2 |
| Connection could not be opened; output file could not be written | 1 |
| `sqlx::Error`; `DatabaseError::ConnectionFailed`, `QueryFailed`, `Internal` | 1 |
| `DatabaseError::MigrationCatalogFenceContended`, `MigrationLedgerLockContended`, `MigrationLockTimeout`, `CatalogMutationFenceContended`, `CatalogMutationGateContended`, `CatalogMutationLockTimeout` | 1 |
| `DatabaseError::MigrationLockRollbackUnconfirmed`, `CatalogMutationRollbackUnconfirmed` (reconcile the backend, then rerun) | 1 |
| Any other `DatabaseError` except `MigrationLedger` and `Lineage`, including a pool that cannot be built as configured | 1 |
| `KeypoolOwnerError::LockTimeout`, `AllowlistOwnerPreparationError::LockTimeout` | 1 |
| `PhysicalAttestationError::Database`, `CommitUnknown`, `PostCommitUnverified`, `InstallationIndeterminate` | 1 |
| `RuntimeStartupError::PreflightPoolCloseIndeterminate` | 1 |
| `KeypoolActivationPending` | 3 |
| `DatabaseError::MigrationLedger`, `DatabaseError::Lineage` | 4 |
| Every other owner-phase, attestor, handoff or preflight error: `PublicSchemaPrivilege`, `UnclassifiedRelation`, `ExcessRuntimePrivilege`, `UnsafeRuntimeRole`, `DatabaseNotExclusive`, `OwnerSessionUntagged`, `KeypoolEnrolledWithoutClusteringPlan`, `LegacyKeypairSlots`, `LegacyShapeMismatch`, `LegacyLeaseLive`, `EnrollmentMismatch`, `UnexpectedPeers`, attestor `Mismatch`, `InstallationConflict`, `NotPrimary`, handoff `Drift`, `MissingObject`, `AttestorInstallationMismatch`, `Refused` | 4 |

Two refusals clear without a change to the plan: `LegacyLeaseLive` once the
lease of a stopped server has run out, and `DatabaseNotExclusive` once the
other sessions are gone. Both still exit 4, because a server that is still
running keeps them true and that is for an operator to establish. A store
error that carries a database failure in an opaque variant, without
exposing it as a cause, also exits 4; its text names the failure.

A refusal prints the typed error and each of its causes: the leased slot
with its holder and heartbeat age, the PeerIds the plan does not name, the
unclassified relation, the excess privilege with its relation. When the
database is not exclusive, each other session visible to the migrator is
listed with its pid, login, `application_name`, client address and state,
and each prepared transaction with its identifier. A pending generation
prints its activation time on the database clock.

### The handoff per release

The handoff document has three kinds of field:

- **Database pins**, read by `emit-handoff` on the owner connection:
  `lineage_uuid`, `system_identifier`, the database and schema OID and name,
  and every entry under `pins` except `installation_operation_id`. They
  change only when a pinned object is recreated.
- **Deployment inputs** that are the same for every release:
  `deployment_uuid`, the role and schema names, `installation_operation_id`
  (`--installation-operation-id`) and the `cutover` record
  (`--cutover <file>`, a JSON file holding exactly the handoff's `cutover`
  object: the transition, the primary endpoint the runtime connects to, the
  retired login and the cutoff references).
- **The release input**: `release_git_sha`, which must equal the
  `WADDLE_GIT_SHA` the image is started with.

A deployment therefore runs `emit-handoff` once per prepared database and,
for each new image, replaces `release_git_sha` in the stored document. The
owner phase is not involved. `emit-handoff --verify <file> --release-git-sha
<sha>` recomposes the document from the database, the role flags and the
given release, taking the cutover record and the installation operation ID
from the file, and exits 4 naming every field that differs (for example
`release_git_sha` or `pins.ledger_primary_index_oid`). It emits nothing.

`emit-handoff` refuses, with exit 4:

- a session whose login is the runtime login: the handoff is never composed
  on the connection it will be checked against;
- a pinned object that does not exist yet, naming it;
- an attestor function whose catalog marker was not written by the given
  operation ID for this identity and these roles;
- a document that `RuntimeStartupIdentity::from_json` rejects for the same
  release and deployment;
- when `WADDLE_RUNTIME_DATABASE_URL` is set, a document that fails the
  server's pinned read-only preflight on that runtime connection (endpoint,
  login, transport and the pinned catalog).

### Provisioning a database one login owns

The owner phase expects a NOLOGIN schema owner that a separate migrator
login acts as, a runtime login without membership or ownership, an attestor
definer and installer, an application schema PUBLIC holds nothing on, and
`pg_catalog.pg_control_system()` executable by the schema owner and the
definer only. A database the owner-as-runtime server built has none of this:
one login owns the database and every relation in `public`, and PUBLIC holds
`CONNECT` and `TEMPORARY` on the database, `USAGE` on `public` and `EXECUTE`
on `pg_control_system()`.

The following procedure is the one-time step that precedes `owner-phase` on
such a database. It needs a superuser: only one can create roles, reassign
another role's objects, change the database owner and change the ACL of a
`pg_catalog` function. It is one transaction, every check runs before the
first change, and a second run changes nothing. The tested copy is
`server/crates/waddle-server/sql/restricted_runtime_provisioning.sql`.

Roles. A role that does not exist is created without a password. A role
that already exists is never altered:

- it is refused if it is a superuser, has `REPLICATION` or `BYPASSRLS`, is
  the session's own role, or is named `pg_*`, `postgres`,
  `streaming_replica` or `cnpg_pooler_pgbouncer`;
- otherwise it must already have the shape the procedure creates (`LOGIN`
  or `NOLOGIN` as its parameter requires, `NOINHERIT`, no `CREATEDB` or
  `CREATEROLE`, no `ALTER ROLE … SET` setting, no membership other than the
  two the procedure grants, and for a NOLOGIN role no password), or the
  procedure stops and names each difference;
- a pre-existing login may have a password. The three logins can therefore
  be created with their credentials beforehand, which is how an operator
  that manages roles declaratively supplies them.

With CloudNativePG, logins declared under `managed.roles` default to
`inherit: true`. The manifest must set `inherit: false` for all three, or
the procedure refuses them; the attachment verifiers require `NOINHERIT`.

The seven role parameters must name seven different roles and the two
schemas must differ. The procedure also stops, having changed nothing, when
the old login still has a backend or a prepared transaction in the database,
when the attestor schema exists under another owner, and when a default
privilege exists for all schemas, for the application or attestor schema,
or for the old login. `REASSIGN OWNED` does not move default privileges, so
the old login's must be removed by an operator first.

Run it connected to the application database, after steps 1–4 of the
coordinated cutover, with the parameters set on the same session:

```sql
SET waddle.schema                   = 'public';
SET waddle.old_login                = 'waddle';
SET waddle.database_owner           = 'waddle_database_owner';
SET waddle.schema_owner             = 'waddle_owner';
SET waddle.migrator_login           = 'waddle_migrator';
SET waddle.runtime_login            = 'waddle_runtime';
SET waddle.attestor_installer_login = 'waddle_attestor_installer';
SET waddle.attestor_definer_role    = 'waddle_attestor';
SET waddle.attestor_schema          = 'waddle_attestation';
```

```sql
BEGIN;

DO $provision$
DECLARE
    app_schema      text := pg_catalog.current_setting('waddle.schema');
    old_login       text := pg_catalog.current_setting('waddle.old_login');
    database_owner  text := pg_catalog.current_setting('waddle.database_owner');
    schema_owner    text := pg_catalog.current_setting('waddle.schema_owner');
    migrator        text := pg_catalog.current_setting('waddle.migrator_login');
    runtime         text := pg_catalog.current_setting('waddle.runtime_login');
    installer       text := pg_catalog.current_setting('waddle.attestor_installer_login');
    definer         text := pg_catalog.current_setting('waddle.attestor_definer_role');
    attestor_schema text := pg_catalog.current_setting('waddle.attestor_schema');
    this_database   text := pg_catalog.current_database();
    owner_roles     text[] := ARRAY[database_owner, schema_owner, definer];
    login_roles     text[] := ARRAY[migrator, runtime, installer];
    role_name       text;
    existing        record;
    detail          text;
BEGIN
    ---------------------------------------------------------------------
    -- Checks. Nothing above the "Changes" line below alters anything.
    ---------------------------------------------------------------------
    IF NOT (SELECT rolsuper FROM pg_catalog.pg_roles WHERE rolname = current_user) THEN
        RAISE EXCEPTION 'restricted runtime provisioning must run as a superuser';
    END IF;
    IF (SELECT pg_catalog.count(DISTINCT name)
          FROM pg_catalog.unnest(ARRAY[old_login] || owner_roles || login_roles) AS name) <> 7 THEN
        RAISE EXCEPTION 'the seven role parameters must name seven different roles; nothing was changed';
    END IF;
    IF app_schema = attestor_schema THEN
        RAISE EXCEPTION 'the application schema and the attestor schema must differ; nothing was changed';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_namespace WHERE nspname = app_schema) THEN
        RAISE EXCEPTION 'application schema % does not exist in database %', app_schema, this_database;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = old_login) THEN
        RAISE EXCEPTION 'old login % does not exist', old_login;
    END IF;

    -- The old login must be gone from this database: a live backend or a
    -- prepared transaction of it could still write as the owner it was.
    SELECT pg_catalog.string_agg(activity.pid::text, ', ') INTO detail
      FROM pg_catalog.pg_stat_activity activity
     WHERE activity.datname = this_database AND activity.usename = old_login
       AND activity.pid <> pg_catalog.pg_backend_pid();
    IF detail IS NOT NULL THEN
        RAISE EXCEPTION 'the old login % still has backends in this database (pid %); nothing was changed',
            old_login, detail;
    END IF;
    SELECT pg_catalog.string_agg(prepared.gid, ', ') INTO detail
      FROM pg_catalog.pg_prepared_xacts prepared
     WHERE prepared.database = this_database AND prepared.owner = old_login;
    IF detail IS NOT NULL THEN
        RAISE EXCEPTION 'the old login % still has prepared transactions in this database (%); nothing was changed',
            old_login, detail;
    END IF;

    -- Default privileges would hand future objects to other roles, and
    -- REASSIGN OWNED leaves the old login's default privileges in place.
    -- None is removed here: each needs a decision.
    SELECT pg_catalog.string_agg(
               pg_catalog.format('role %s schema %s type %s: %s',
                   pg_catalog.pg_get_userbyid(da.defaclrole),
                   COALESCE(n.nspname, '(all)'), da.defaclobjtype, da.defaclacl), '; ')
      INTO detail
      FROM pg_catalog.pg_default_acl da
      LEFT JOIN pg_catalog.pg_namespace n ON n.oid = da.defaclnamespace
     WHERE da.defaclnamespace = 0
        OR n.nspname IN (app_schema, attestor_schema)
        OR pg_catalog.pg_get_userbyid(da.defaclrole) = old_login;
    IF detail IS NOT NULL THEN
        RAISE EXCEPTION 'default privileges must be removed first (%); nothing was changed', detail;
    END IF;

    -- An attestor schema that already exists must already be the definer's.
    SELECT pg_catalog.pg_get_userbyid(nspowner) INTO detail
      FROM pg_catalog.pg_namespace WHERE nspname = attestor_schema;
    IF detail IS NOT NULL AND detail <> definer THEN
        RAISE EXCEPTION 'attestor schema % already exists and belongs to %, not to %; nothing was changed',
            attestor_schema, detail, definer;
    END IF;

    -- Roles this procedure would create. An existing one is never altered.
    FOREACH role_name IN ARRAY owner_roles || login_roles LOOP
        IF role_name LIKE 'pg\_%'
           OR role_name IN (current_user, session_user,
                            'postgres', 'streaming_replica', 'cnpg_pooler_pgbouncer') THEN
            RAISE EXCEPTION 'role % is a system role or this session''s own role and cannot be a restricted-runtime role; nothing was changed',
                role_name;
        END IF;
        SELECT * INTO existing FROM pg_catalog.pg_authid WHERE rolname = role_name;
        IF NOT FOUND THEN
            CONTINUE;
        END IF;
        IF existing.rolsuper OR existing.rolreplication OR existing.rolbypassrls THEN
            RAISE EXCEPTION 'role % already exists with SUPERUSER, REPLICATION or BYPASSRLS and will not be altered; nothing was changed',
                role_name;
        END IF;
        detail := pg_catalog.concat_ws(', ',
            CASE WHEN role_name = ANY (login_roles) AND NOT existing.rolcanlogin THEN 'it is NOLOGIN' END,
            CASE WHEN role_name = ANY (owner_roles) AND existing.rolcanlogin THEN 'it is LOGIN' END,
            CASE WHEN role_name = ANY (owner_roles) AND existing.rolpassword IS NOT NULL
                 THEN 'it has a password' END,
            CASE WHEN existing.rolinherit THEN 'it is INHERIT' END,
            CASE WHEN existing.rolcreatedb THEN 'it is CREATEDB' END,
            CASE WHEN existing.rolcreaterole THEN 'it is CREATEROLE' END,
            CASE WHEN EXISTS (SELECT 1 FROM pg_catalog.pg_db_role_setting s
                               WHERE s.setrole = existing.oid)
                 THEN 'it has ALTER ROLE ... SET settings' END,
            (SELECT pg_catalog.string_agg(
                        pg_catalog.format('%s is a member of %s',
                            pg_catalog.pg_get_userbyid(m.member),
                            pg_catalog.pg_get_userbyid(m.roleid)), ', ')
               FROM pg_catalog.pg_auth_members m
              WHERE existing.oid IN (m.roleid, m.member)
                AND NOT (m.set_option AND NOT m.inherit_option AND NOT m.admin_option
                         AND (pg_catalog.pg_get_userbyid(m.roleid),
                              pg_catalog.pg_get_userbyid(m.member))
                             IN ((schema_owner, migrator), (definer, installer)))));
        IF detail <> '' THEN
            RAISE EXCEPTION 'role % already exists and does not have the shape this procedure creates: %; nothing was changed',
                role_name, detail;
        END IF;
    END LOOP;

    ---------------------------------------------------------------------
    -- Changes.
    ---------------------------------------------------------------------

    -- 1. Roles. The three owners cannot log in and nothing inherits from
    --    them; the three logins hold no cluster-wide attribute.
    FOREACH role_name IN ARRAY owner_roles LOOP
        IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = role_name) THEN
            EXECUTE pg_catalog.format(
                'CREATE ROLE %I NOLOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE '
                'NOREPLICATION NOBYPASSRLS', role_name);
        END IF;
    END LOOP;
    FOREACH role_name IN ARRAY login_roles LOOP
        IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = role_name) THEN
            EXECUTE pg_catalog.format(
                'CREATE ROLE %I LOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE '
                'NOREPLICATION NOBYPASSRLS', role_name);
        END IF;
    END LOOP;

    -- 2. The only memberships: the migrator can act as the schema owner and
    --    the installer as the attestor definer, by SET ROLE alone. The
    --    runtime login is a member of nothing.
    IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m
                    WHERE pg_catalog.pg_get_userbyid(m.roleid) = schema_owner
                      AND pg_catalog.pg_get_userbyid(m.member) = migrator) THEN
        EXECUTE pg_catalog.format(
            'GRANT %I TO %I WITH INHERIT FALSE, SET TRUE, ADMIN FALSE', schema_owner, migrator);
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m
                    WHERE pg_catalog.pg_get_userbyid(m.roleid) = definer
                      AND pg_catalog.pg_get_userbyid(m.member) = installer) THEN
        EXECUTE pg_catalog.format(
            'GRANT %I TO %I WITH INHERIT FALSE, SET TRUE, ADMIN FALSE', definer, installer);
    END IF;

    -- 3. Ownership. Every object of the old login moves to the NOLOGIN
    --    schema owner. The database gets an owner no login can act as, and
    --    the application schema (`public` belongs to pg_database_owner, not
    --    to the old login) is given to the schema owner explicitly.
    EXECUTE pg_catalog.format('REASSIGN OWNED BY %I TO %I', old_login, schema_owner);
    EXECUTE pg_catalog.format('ALTER DATABASE %I OWNER TO %I', this_database, database_owner);
    EXECUTE pg_catalog.format('ALTER SCHEMA %I OWNER TO %I', app_schema, schema_owner);

    -- 4. Database access. PUBLIC loses CONNECT and TEMPORARY on this
    --    database, so the old login can no longer connect to it; the three
    --    logins connect and nothing else.
    EXECUTE pg_catalog.format('REVOKE ALL ON DATABASE %I FROM PUBLIC', this_database);
    EXECUTE pg_catalog.format('REVOKE ALL ON DATABASE %I FROM %I', this_database, old_login);
    EXECUTE pg_catalog.format(
        'GRANT CONNECT ON DATABASE %I TO %I, %I, %I', this_database, migrator, runtime, installer);

    -- 5. Schema access. The owner phase refuses an application schema on
    --    which PUBLIC holds anything, which the default `public` schema
    --    does. No other schema is touched.
    EXECUTE pg_catalog.format('REVOKE ALL ON SCHEMA %I FROM PUBLIC', app_schema);
    EXECUTE pg_catalog.format('REVOKE ALL ON SCHEMA %I FROM %I', app_schema, old_login);

    -- 6. The attestor's own schema, owned by the definer role.
    IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_namespace WHERE nspname = attestor_schema) THEN
        EXECUTE pg_catalog.format('CREATE SCHEMA %I AUTHORIZATION %I', attestor_schema, definer);
    END IF;

    -- 7. The physical identity is read through pg_control_system(). Only the
    --    schema owner (lineage) and the attestor definer may call it; the
    --    runtime and installer logins must not, so PUBLIC loses it. Function
    --    ACLs are per database: no other database is affected.
    REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM PUBLIC;
    EXECUTE pg_catalog.format(
        'GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO %I, %I',
        schema_owner, definer);
END
$provision$;

COMMIT;
```

What it changes, and why the verifiers need it:

| Step | Statement | Required by |
|---|---|---|
| 1 | Creates each missing role: three NOLOGIN owners and three logins, all `NOINHERIT` and without cluster-wide attributes | The runtime role check (`UnsafeRuntimeRole`) and the attestor's role prerequisites |
| 2 | `GRANT <owner> TO <migrator>` and `GRANT <definer> TO <installer>`, `SET` only, where not already granted | Room observation's migrator check and the attestor's exact role graph; the runtime is a member of nothing |
| 3 | `REASSIGN OWNED BY <old login> TO <schema owner>`, `ALTER DATABASE … OWNER`, `ALTER SCHEMA … OWNER` | Every store verifies schema-owner ownership; the database owner must be none of the runtime, installer or definer |
| 4 | `REVOKE ALL ON DATABASE … FROM PUBLIC`, `GRANT CONNECT` to the three logins | The runtime must hold neither `TEMP` nor `CREATE` on the database |
| 5 | `REVOKE ALL ON SCHEMA <application schema> FROM PUBLIC` | `PublicSchemaPrivilege`; MAM and the durable room store admit only the owner, the runtime, the definer and `pg_monitor` in the schema ACL |
| 6 | Creates the attestor schema, owned by the definer, if it is missing | The attestor's trusted-schema prerequisites |
| 7 | `REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM PUBLIC`, `GRANT` to the schema owner and the definer | Lineage verification under the schema owner; the attestor refuses a runtime or installer that can call it |

Scope and consequences the runbook must state:

- `REASSIGN OWNED` moves every object the old login owns in this database,
  in any schema, and every database it owns.
- Only the application schema's ACL is changed. With a dedicated
  application schema, `public` keeps its default PUBLIC `USAGE`; the
  restricted-runtime suites pass with it in place.
- Database and function ACLs are per database. The revokes apply to this
  database only: the three logins, like every role, keep PUBLIC's `CONNECT`
  and `TEMPORARY` on other databases such as `postgres` and `template1`,
  and `pg_control_system()` stays callable by PUBLIC there. An operator who
  wants the runtime login confined to the application database revokes
  `CONNECT` from PUBLIC on the others, or restricts the login in
  `pg_hba.conf`; neither is required by a verifier.
- Afterwards the old login owns nothing in this database and cannot connect
  to it, and it can no longer call `pg_control_system()` here, which lineage
  verification reads. The old login is the `old_runtime` of the handoff's
  cutover record. It keeps its role; rotating or dropping its credential is
  step 2 of the cutover.
- The provisioning does not touch relation or function ACLs. A relation that
  grants something to another role is reported by the owner phase or by the
  attachment that verifies it.
- Rollback. Provisioning is the rollback boundary of the cutover: from it
  on, the owner-as-runtime image cannot start on this database. There is no
  reverse script. Rolling back means restoring the PostgreSQL cluster from
  the physical backup taken after the pods stopped and before provisioning
  (a logical dump of the database does not carry roles, memberships or the
  database ACL), restoring the old login's credential, and deploying the
  old image. Writes accepted after the cutover are lost by that restore.
  Once `owner-phase` has retired the keypair-slot table, the old image
  could not lease a slot even with ownership returned.

`owner_entrypoint::provisioning_refuses_what_it_would_rewrite_and_repeats_without_change`
(`tests/restricted_runtime_startup/owner_entrypoint.rs`) applies the
procedure to a database one login owns and compares, around every run, a
superuser's reading of the named roles' attributes, password presence,
memberships and settings, the database and schema owners and ACLs, every
relation and routine with owner and ACL, the ACL of `pg_control_system()`
and all default privileges. A superuser or replication role, a system role
name, repeated parameters, an existing login with `INHERIT`, a setting and
a membership, a default privilege of the old login, and a connected old
login each stop it with that reading unchanged. Logins created beforehand
with passwords and the target shape are accepted and keep their attributes
and passwords, and a second run leaves the reading identical.

`owner_entrypoint::clustered::production_shaped_public_schema_is_provisioned_and_served`
builds the production shape: a login that owns its database, the clustered
owner-as-runtime server started against `public` with its default grants,
and a roster item, an archived direct message and a room with an archived
message written through it. It then applies the procedure twice, with the
same before-and-after comparison, runs `owner-phase` with
`--legacy-keypair-slots retire` (exit 3, then exit 0 after the delay),
`install-attestor` and `emit-handoff` with the pinned preflight, and starts
the clustered server under the runtime login on `public`. The user, roster,
archive and room rows are unchanged by provisioning and the owner phase, the
server returns the roster item and both archived messages, and PostgreSQL
returns no `insufficient_privilege` error to the runtime login.

`owner_entrypoint::subcommands_prepare_a_database_the_restricted_server_serves`
runs the three subcommands on a fresh database with separate roles and a
dedicated schema, starts the server on the emitted handoff, and covers the
repeated owner phase, the handoff for another release that the server
refuses at startup, and `--verify` on a drifted database.
`owner_entrypoint::clustered::clustered_owner_phase_reports_pending_activation_and_refusals`
covers exit 3 with its activation time, the listed foreign session and the
refused unenrolled lease table. Each test checks every subcommand's output
for the database passwords, the connection URLs, the session key and the
keypair seeds.
`owner_entrypoint::subcommands_do_not_echo_credentials_under_password_authentication`
repeats that check where a password is actually verified: it starts a
PostgreSQL server of its own with `scram-sha-256` authentication and runs
the subcommands against a wrong password, an unreachable server and a
runtime URL whose password fails the preflight. It needs `initdb` and
`postgres` on `PATH` and is skipped without them, so it is not in the
required-test list.
`composed_handoff_carries_the_pins_an_administrator_reads` reads every
database-derived field of the handoff with its own administrator query and
requires the composed document to carry exactly those values.

What the entrypoint does not do: create the database, set or rotate a
credential, fence the old writers, or decide the cutover record. It has no
read-only status subcommand; a repeated `owner-phase` on a prepared database
changes nothing and reports the same result.

### Deployment

The chart and the GitOps directory carry the cutover, switched off. With
both chart switches off and neither component listed, the rendered objects
contain nothing of it.

- **Chart** (`server/charts/waddle-server`). `restrictedRuntime.ownerJob.enabled`
  renders a ServiceAccount, a Role limited to the handoff ConfigMap, the
  Job's inputs and a CronJob that is suspended and scheduled for a date
  that does not occur. An operator creates a Job from it. Its pod runs
  `owner-phase`, `install-attestor` and `emit-handoff` from the release
  image as init containers, each with its own login's URL
  (`sslmode=verify-full`, the CA mounted from a Secret) and, for
  `emit-handoff`, the runtime URL so the pinned preflight runs; a last
  container publishes the emitted document as the ConfigMap
  `<release>-runtime-handoff-pins` with `kubectl apply -k`. The Job's
  `podFailurePolicy` fails it at once on exit codes 2 and 4 and counts 1
  and 3 against `backoffLimit`, so a pending keypair-pool generation is
  retried by the Job controller's backoff. It is not a Helm hook: the
  clustered owner phase needs the server at zero replicas.
- `restrictedRuntime.enabled` gives the server `WADDLE_DATABASE_URL` for the
  runtime login (verified TLS, `options=-c search_path=<schema>`, the
  read-write Service), `WADDLE_DB_MIGRATOR_LOGIN`,
  `WADDLE_XMPP_MAM_SCHEMA_OWNER`, `WADDLE_XMPP_MAM_CLAIM_SCHEMA` and
  `WADDLE_DB_RUNTIME_ATTESTATION` from the ConfigMap
  `<release>-runtime-handoff`. The chart renders that ConfigMap from the
  published pins, or from `restrictedRuntime.handoff.document` when the
  document is kept in values, replacing `release_git_sha` with
  `restrictedRuntime.releaseGitSha`. Reading the published ConfigMap uses
  Helm's `lookup`, so an offline render needs the document in values. The
  chart refuses to render when the SHA differs from the `WADDLE_GIT_SHA` in
  `containerExtraEnv`, when a variable it sets is also set there, or when a
  lineage action is configured.
- **Release automation** (`server/env.cue`, `server/deployment.cue`) writes
  `restrictedRuntime.releaseGitSha` in the same step as `WADDLE_GIT_SHA` and
  the published-values schema requires it, so the handoff is composed for
  every release.
- **GitOps**
  (`infrastructure/waddle.cloud/gitops/waddle-server/restricted-runtime/`).
  Two kustomize components that `kustomization.yaml` does not list.
  `prepare` adds the six roles to the CloudNativePG cluster's
  `managed.roles` with `inherit: false`, the two memberships and a
  PostgreSQL 17 image, and sets the chart's connection, role, credential
  and owner Job values. `serve` sets `restrictedRuntime.enabled` and removes
  the old `WADDLE_DATABASE_URL`. The three password Secrets are referenced
  by name and are not in the repository.

The procedure, with its rollback boundary, is
[the cutover runbook](../runbooks/restricted-runtime-cutover.md).

Not established: none of this has been applied to a cluster. The
operator's handling of `inRoles` and `inherit: false` is taken from its
documentation, not observed. A release that changes owner preparation needs
the owner Job again at zero replicas, which is a manual step.

## Typed ingress transaction entry

On the serving pool every ingress transaction that attests its connection
begins in `ingress_substrate::typed_entry::begin_ingress_entry`. The ingress
unit of work, the pooled effect-receipt writer and its post-commit readback
all call it. So do the raw ingress writers when their database handle is an
attested pool: `ingress_uow::begin_raw_ingress_writer`, which the notification
and push outboxes use, and `PostgresIngressSubstrate::begin`. On a handle
without attestation the substrate keeps its own sequence, and the raw writer
begins in the same module's `begin_unattested_raw_writer`, which first probes
for the descendant foundation so a standalone store without it stays usable.
A request names, as typed values, what the callers differ in:

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

Both raw-writer begins return an `EnteredTransaction`, which only the entry
module constructs, and a unit-of-work transaction lends the same type. The
descendant repository methods the notification and push outboxes call
(`EffectDescendantRepository::lock_raw`, `lock_all_raw`,
`lock_all_nowait_raw`, `attach_raw`, `copy_raw` and `settle_all_raw`) take
that type, as does every outbox helper between them and the begin. A
descendant lock or write on a transaction opened directly on a database
handle therefore does not compile. Tests wrap a fixture transaction through
a constructor that exists only in test builds. The source check also fails
if a `*_raw` method takes anything else.

What the entry does not do:

- It attests the physical connection identity and lineage, not the migration
  ledger or the epoch guard manifest; those are checked once by the startup
  preflight.
- The pool type still exposes raw `begin`, `execute` and `query`. A raw
  writer's own statements are not schema-qualified; on an attested pool they
  resolve through the `search_path` the entry pinned.
- The ingress maintenance scans, the room-observation retention batch and
  three pooled single-statement reads run outside the entry. None touches a
  relation the epoch guard protects, and the allowlist names each.
- Attach-time checks in other stores call the attestor directly on their own
  read-only transactions.
- The clustered stores (claims, keypair lease, durable rooms, fenced stream
  management) attach under the attested pool and open their own
  transactions; none goes through the entry.

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
tests does not by itself permit production serving with the attested pool.
