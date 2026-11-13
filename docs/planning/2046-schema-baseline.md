# #2046 M7: schema baseline design and cut procedure

Status: built and unused. The baseline SQL, the migration-runner entry that
applies it and the stamp that records it on an existing database are in the
tree. A process uses them only when `WADDLE_DB_BASELINE_ACTION` is set; with
it unset the migration catalogs, the ledger validation, the owner phase and
every store's schema bootstrap behave as before. The cut, which makes the
baseline the default and deletes the history, is section 8. Scope and hazards
are in
[the completion contracts, section "M7: schema baseline"](2046-completion-contracts.md).

Paths: `S/` is `server/crates/waddle-server/src/`, `X/` is
`server/crates/waddle-xmpp/src/`, `MIG/` is `S/db/migrations/`, `B/` is
`server/crates/waddle-server/sql/baseline/`.

## 1. Where the schema comes from today

One database holds the whole schema: a PostgreSQL database, or a SQLite file.
It is built from five sources.

| Source | What it creates | Recorded in `_migrations` |
| --- | --- | --- |
| Migration catalog: `MIG/global.rs` (V1–V15) and `MIG/waddle.rs` (V1001–V1030), run as one ordered stream by `MigrationRunner::single()` | 55 tables, the ingress epoch guards, their static rows | yes, one row per version |
| Store bootstraps, each with its own `CREATE ... IF NOT EXISTS` and `ALTER` chain | 57 more tables on PostgreSQL, 38 on SQLite (section 4), and columns and indexes on catalog tables | no |
| `sql/restricted_runtime_provisioning.sql`, run by a superuser | the seven roles, database and schema ownership, database and schema ACL, EXECUTE on `pg_catalog.pg_control_system()`, the attestor schema | no |
| `waddle-server owner-phase` (`S/server/runtime_owner.rs`) | runs the catalog and every store's owner preparation, then grants the runtime role from an exhaustive matrix | no |
| `waddle-server install-attestor` | `read_physical_identity()` in the attestor schema, owned by the definer role | no |

The two catalogs share one ledger. `MigrationNamespace` splits versions at
1000 so the lists cannot collide; `MigrationRunner::global()` and `waddle()`
exist for tests only. There is no deployment in which the two namespaces live
in different databases.

A database reaches the served schema in one of four ways. `scripts/schema_history.py`
builds each from an empty database with the real binary:

- **legacy**: PostgreSQL, one login owns everything. The operator applies the
  `waddle-cluster-keypool` enrollment SQL (the server does not create
  `clustering_peer_allowlist` and fails to start without it), then the server
  starts without `WADDLE_DB_RUNTIME_ATTESTATION` and
  `bootstrap_shared_owner_postgres` plus each store's bootstrap build the rest.
- **restricted**: provisioning SQL, `owner-phase` with a clustering plan,
  `install-attestor`.
- **upgraded**: a legacy database taken through provisioning, `owner-phase
  --legacy-keypair-slots retire` and `install-attestor`.
- **sqlite**: the server starts on a SQLite file as its single role.

### What the four paths produce (commit `68c8ae9a4`, PostgreSQL 17.11)

Measured with the tooling in section 7. Counts are catalog objects.

- **restricted and upgraded are the same schema.** Every table, column,
  constraint, index, sequence, trigger, function, owner, ACL, role and
  membership is identical, in the application schema and outside it. The one
  catalog difference is the comment on `read_physical_identity()`, which
  carries the installation's operation ID. The upgraded database additionally
  holds the rows the legacy server wrote while it served (channels, PubSub
  nodes, the test account, clustering rows).
- **legacy is a strict subset of restricted.** Nothing in the legacy schema is
  absent from or different in the restricted one. The restricted schema adds
  128 objects, all created by owner preparations the legacy startup never
  runs:
  - nine tables with their columns, constraints and indexes:
    `clustering_keypool`, `clustering_keypool_slots`,
    `extension_room_observation_owner_state`,
    `notification_activity_owner_state`, `notification_outbox_owner_state`,
    `pubsub_schema_marker`, `push_service_schema_marker`,
    `push_service_vapid_schema_marker`, `webhook_deliveries`;
  - three columns on `clustering_keypair_slots`, which the owner phase
    recreates under the keypair-pool enrollment;
  - the four keypair-pool guard functions and their six triggers, and
    `pubsub_runtime_attach_lock()`;
  - the attestor schema and `read_physical_identity()`.
- At `056ae1a3c` the owner phase also creates `sm_durable_sessions` and
  `sm_durable_operations`, so the restricted schema adds 172 objects.
- **`webhook_deliveries` is created on first use** by a server that owns its
  schema (`S/server/routes/webhook_delivery.rs`, `initialize`), so a freshly
  started legacy or SQLite database does not hold it. The restricted owner
  phase creates it eagerly.
- **PostgreSQL and SQLite agree by name.** The 91 tables both engines hold
  have the same column names and the same explicitly created index names.
  PostgreSQL alone has 21 tables and SQLite alone has one; they are listed
  with their reasons in `server/scripts/schema-engine-differences.txt`.
- **All three PostgreSQL paths apply the same 45 ledger rows.** PostgreSQL
  and SQLite checksums differ for 36 of the 45 versions, because each is the
  hash of that engine's SQL.
- **Source DDL no path executes**: `sm_revision_authority`,
  `sm_revision_generations`, `sm_revision_operations`
  (`S/sm_persistence/revisioned/schema.rs`) and the candidate SQL under
  `MIG/owner/` that is not registered. They are not in any schema and do not
  enter the baseline.

## 2. What the baseline is

**One baseline migration per engine, in the existing ledger**: version 2000,
description `schema baseline`, defined in `MIG/baseline.rs` with its SQL in
`B/postgres.sql` and `B/sqlite.sql`. Later schema changes are ordinary
migrations numbered from 2001. Version 2000 is above every existing and
in-flight version, so "below 2000" means "retired history" on the version
alone.

The baseline is the catalog of a process that selected it
(`MIG/baseline.rs`, `select_from_env`): `MigrationRunner::single()` then
returns the one baseline entry instead of the history. The selection is
process-wide and made once, before a database is opened, because every ledger
reader in the process builds its runner with `single()`: the runner that
applies, the owner preparations that verify a retired version was applied
(the baseline stands for any of them), and the runtime preflight that pins
the ledger.

`WADDLE_DB_BASELINE_ACTION`:

| Value | Effect |
| --- | --- |
| unset | The migration history is the catalog. Nothing of the baseline runs. |
| `apply` | The baseline is the catalog. An empty database is built from it; a database on it validates; a database that still holds retired versions is refused (`RetiredLedger`). |
| `stamp` | As `apply`, and a database that holds the complete retired history is stamped (section 5). |

Until the cut, every process that touches a baseline database is given
`apply`: `owner-phase`, `install-attestor`, `emit-handoff` and the server. A
process without it meets version 2000 in the ledger and refuses.

The baseline SQL uses plain `CREATE`, not `CREATE ... IF NOT EXISTS`. It runs
once per database, inside the migration transaction, when the ledger does not
hold version 2000; an object that already exists there is an error to
surface, not to skip.

It names no role. On PostgreSQL it runs as the schema owner with the
application schema first on the search path, as migrations do, so objects
belong to whoever runs it.

### The schema's name is a template

`B/postgres.sql` is rendered before it is applied. The retired owner SQL
spells the schema's name three ways inside function definitions, and each has
a placeholder:

| Placeholder | Rendered as | Example in the source schema `public` |
| --- | --- | --- |
| `__SCHEMA_QUOTED__` | the name in double quotes | `LOCK TABLE "public"."pubsub_items"` |
| `__SCHEMA_LITERAL__` | `quote_literal(name)` | `SET search_path TO 'pg_catalog', 'public', 'pg_temp'` |
| `__SCHEMA__` | `quote_ident(name)` | `public.waddle_ingress_definer_prepare()` |

The runner asks the server for `quote_ident` and `quote_literal` of
`current_schema()` and substitutes (`MIG/baseline.rs`, `apply`). The ledger
checksum is of the template, so it is the same in every schema.

The generator writes the placeholders into every function whose body or
`proconfig` names the schema; a schema or role name anywhere else (a trigger,
constraint, default, index or view) has no rendering and makes it refuse. Two
functions in the baseline are templates: `pubsub_runtime_attach_lock` and
`waddle_p21_record_effect_receipt`. The four keypair-pool guards also name the
schema and stay with their owner preparation for now (below).

Proof: the history was built in schema `public` and in schema `tenant_a`. The
templates generated from the two are byte-identical, the template built from
`public` round-trips against the `tenant_a` dump, and an owner phase on the
baseline in `tenant_a` gives the same catalog as the history in `tenant_a`.

### Static rows

The baseline carries the rows of tables whose content is the same in every
deployment and that nothing writes after the migrations:

| Table | Engine | Rows | Written today by |
| --- | --- | --- | --- |
| `ingress_protocol_epoch` | both | 1 | `MIG/waddle.rs` |
| `ingress_epoch_guard_manifest` | PostgreSQL | 13 | `MIG/waddle.rs` |
| `pending_notification_recovery_counter` | SQLite | 1 | `S/pending_delivery/database/recovery_order.rs`, with the table; the store refuses the table without the row |

They are inserted before the triggers are created, because the epoch guards
refuse writes. The list is `STATIC_TABLES` in `scripts/schema_baseline.py`.

Every other table that holds rows on a fresh database gets them after the
baseline, from code that keeps running:

| Rows | Writer | Why not static |
| --- | --- | --- |
| `_lineage` | lineage enrollment | the deployment's identity |
| `_migrations` | the runner | the ledger itself |
| `pending_delivery_startup_migrations` (2) | the pending-delivery bootstrap | each row carries the time the step ran |
| `pubsub_schema_version` (1), `clustering_orphan_reaper_cursors` (2) | the PubSub and claims bootstraps | static content, but the store writes it when it finds it missing, so the baseline would only duplicate a writer that stays |
| `clustering_keypool`, `clustering_keypool_slots`, `clustering_peer_allowlist` | the clustering plan | derived from the keypair pool |
| `push_service_vapid_keys` | the VAPID preparation | sealed under the session key |
| the owner-state and schema-marker rows, `_waddle_space_store_owner` | each store's owner preparation | record the database's identity and the preparation's revision |

The harness compares the contents of the five content-stable tables
(`ROW_TABLES` in `scripts/schema_history.py`) and the row counts of all tables
between the history and the baseline.

### What the baseline does not hold yet

Nine PostgreSQL tables and four functions are created by an owner preparation
together with the row that marks them prepared, and that preparation refuses
the table when it finds it without the row. The baseline leaves them, with
their columns, constraints, indexes and triggers, to the preparation, which
creates them as it does today:

`clustering_keypair_slots`, `clustering_keypool`, `clustering_keypool_slots`,
`extension_room_observation_owner_state`, `notification_activity_owner_state`,
`notification_outbox_owner_state`, `pubsub_schema_marker`,
`push_service_schema_marker`, `push_service_vapid_schema_marker`; and
`clustering_keypair_slot_guard`, `clustering_keypair_truncate_guard`,
`clustering_keypool_guard`, `clustering_keypool_slots_guard`.

The lists are `OWNER_PREPARED_TABLES` and `OWNER_PREPARED_FUNCTIONS` in
`scripts/schema_baseline.py`. An entry is removed when its preparation accepts
the table from the baseline, which is a change to that preparation and not to
the tooling; the harness then shows whether the result is still the history's
schema. SQLite has no such tables.

Everything else an owner preparation or a store bootstrap creates is in the
baseline, and the unchanged preparations and bootstraps run over it without
creating or altering anything: the harness's catalogs are identical.

The baseline also carries the privileges the retired migrations grant that
name no deployment role, because later preparations verify them: `SELECT` for
`pg_monitor` on 19 tables and one column, and the withdrawal of PUBLIC
`EXECUTE` from the two template functions.

### What stays outside the baseline for good, and why

| Stays outside | Mechanism | Reason |
| --- | --- | --- |
| Roles, memberships, database owner and ACL, schema owner and ACL, `pg_control_system()` ACL, the attestor schema | `sql/restricted_runtime_provisioning.sql` | Needs a superuser; the migrator login must not be able to do any of it. Role names are deployment inputs. |
| Runtime privileges | `grant_runtime` and the grant matrix in `S/server/runtime_owner.rs` | The runtime role's name is a deployment input, and the phase verifies the result against the same matrix. |
| `waddle_attestation.read_physical_identity()` | `install-attestor` | Created by a different login as a different owner; its OID is pinned by the handoff and its comment records the operation ID. |
| `_lineage` and `_migrations` | `S/db/lineage/sql.rs`, `MIG/sql.rs`, `MIG/postgres_ledger.rs` | The server creates both before it applies a migration. |
| Deployment-derived rows | the owner phase | See "Static rows". |

## 3. Fresh database

PostgreSQL, restricted, each step with `WADDLE_DB_BASELINE_ACTION=apply`:

1. `sql/restricted_runtime_provisioning.sql` as a superuser.
2. `owner-phase`: lineage is enrolled, the runner creates `_migrations`,
   renders and applies the baseline in one transaction under the catalog
   fence and the ledger lock, and inserts row 2000. The store preparations
   run as they do today and create only the owner-prepared tables.
   `grant_runtime`, `verify_runtime_role`.
3. `install-attestor`, `emit-handoff`.
4. The server starts with the handoff. Its pinned preflight accepts row 2000.

SQLite: the server starts with `apply`, the runner applies the baseline under
`BEGIN IMMEDIATE`, and each store attaches. SQLite keeps single-role startup.

A PostgreSQL server that owns its schema (no attestation) does not take the
baseline and refuses to start when it is selected. Its stores create
`clustering_keypair_slots` and `clustering_peer_allowlist` in a shape or at a
time that differs from the restricted path: the operator creates the
allowlist before the first start, and a plain `CREATE` then fails. The
PostgreSQL baseline is the restricted schema, reached through the owner
phase.

## 4. Per-store decisions

"Folds" means the DDL is in the baseline and, at the cut, the store's
bootstrap keeps only attachment, verification and the listed rows. Until the
cut the bootstrap's DDL still runs and finds everything in place. Tables
named in "What the baseline does not hold yet" fold once their preparation
is converted.

| Store | Tables | DDL today | Decision |
| --- | --- | --- | --- |
| Lineage | `_lineage` | `S/db/lineage/sql.rs` | Table folds. The row stays with enrollment: it is the deployment's identity. |
| Inbox | `inbox_entries`, `groupchat_notification_recovery` | `S/inbox/schema.rs` | Folds. |
| PubSub | `pubsub_nodes`, `pubsub_items`, `pubsub_subscriptions`, `pubsub_affiliations`, `pubsub_owner_configured`, `pubsub_push_publications`, `pubsub_schema_version`, `dnd_projection`, `dnd_projection_source_version`, `notification_settings_projection`, `notification_settings_projection_source_version`, `pubsub_schema_marker` | `S/pubsub/schema.rs`, `S/pubsub/owner.rs` | Folds, with `pubsub_runtime_attach_lock()` as a template function. `pubsub_schema_version`'s row stays with the store, which writes it when it is missing; the marker row stays, it records the owner phase. |
| Stream management, portable and fenced | `sm_sessions`, `sm_unacked`, `sm_acknowledged_windows`, `sm_ingress_appends`, defined by the catalog and again by the store's `CREATE ... IF NOT EXISTS` and `ADD COLUMN` chain | `S/sm_persistence/schema.rs`, `S/sm_persistence/implementation.rs`, `S/sm_persistence_fenced/implementation.rs` | Folds; one definition remains. The V1029 special case in `MIG/runner.rs` goes with the history. |
| Pending delivery | `pending_delivery`, `pending_delivery_sm_promotions`, `pending_delivery_startup_migrations`; SQLite `pending_notification_recovery_counter` | `S/pending_delivery/database/schema.rs`, `recovery_order.rs` | Folds. The two `pending_delivery_startup_migrations` rows stay with the store: each carries the time its step ran. The SQLite counter row is static. |
| Notification outbox | `notification_outbox`, `notification_candidates`, `notification_outbox_lineage`, `notification_outbox_owner_state` | `S/notification_outbox/schema.rs` | Folds. Owner-state row stays. |
| Notification activity | `notification_activity`, `notification_activity_owner_state` | `S/notification_activity/schema.rs`, `owner.rs` | Folds. Owner-state row stays. |
| Push service | `push_nodes`, `push_devices`, `push_publish_jobs`, `push_delivery_attempts`, `push_node_locks`, `push_owner_locks`, `push_publication_orders`, `push_service_schema_marker` | `S/push_service/store.rs`, `owner.rs` | Folds. Marker row stays. |
| VAPID | `push_service_vapid_keys`, `push_service_vapid_schema_marker` | `S/push_service/vapid_storage.rs`, `S/server/push_notification_owner.rs` | Tables fold. The key row stays: it is sealed under the session key. |
| Push registrations | `push_registrations` | `S/push_registrations.rs` | Folds. |
| Spaces metadata, channel links | `spaces_metadata`, `channel_space_links`, `_waddle_space_store_owner` | `S/spaces_metadata/schema.rs`, `S/channel_space_links/schema.rs` | Folds. The owner row stays. |
| Call teardown outbox | `call_teardown_outbox` | `S/call_teardown_outbox/schema.rs` | Folds. |
| Room effect outbox | `clustering_muc_room_effects` | `S/room_effect_outbox/schema.rs` | Folds. |
| MUC destroy outbox | `clustering_muc_destroy_outbox` | `S/muc_destroy_completion_outbox.rs` | Folds. |
| Durable MUC | `clustering_muc_rooms`, `clustering_muc_room_lifecycles`, `clustering_muc_room_affiliations`, `clustering_muc_admin_receipts` | `S/muc_durable/implementation.rs`, `admin_receipts.rs` | Folds, PostgreSQL only. |
| Room observation | the V10xx observation tables' two scheduling indexes, `extension_room_observation_owner_state` | `S/ingress_uow/room_observation/schema.rs` | Folds. Owner-state row stays. |
| Clustering claims and nodes | `clustering_claims`, `clustering_nodes`, `clustering_steal_intents`, `clustering_orphan_reaper_cursors`, `clustering_claim_epoch_seq` | `S/clustering/claims.rs` | Folds, PostgreSQL only. The two cursor rows stay with the store, which writes them when they are missing. |
| Keypair slots and pool | `clustering_keypair_slots`, `clustering_keypool`, `clustering_keypool_slots`, four guards, six triggers | `S/clustering/lease.rs` | Owner-prepared for now; folds in the enrolled shape, guards as template functions, once the preparation accepts the tables without the enrollment. The enrollment rows stay. The legacy-slot retirement (`--legacy-keypair-slots retire`) is deleted: no database holds the unenrolled table after the cut. |
| Peer allowlist | `clustering_peer_allowlist` | `S/clustering/allowlist.rs`, `S/bin/cluster_keypool.rs` | Table folds. Rows stay with the plan. `waddle-cluster-keypool --sql-output` stops emitting `CREATE TABLE`. |
| MAM | `mam_messages` | `X/mam/storage/sqlx_store/schema.rs`, `MIG/owner/mam.rs` | Folds. The `waddle-xmpp` initializer keeps its DDL for the crate's own tests and stops being called by the server's owner phase. |
| Webhook ledger | `webhook_deliveries` | `S/server/routes/webhook_delivery.rs` | Folds on both engines. This adds the table to SQLite at startup, where it is lazy today. |
| P21 receipt journal | `p21_receipt_operation_results`, `waddle_p21_record_effect_receipt` | `MIG/owner/p21_receipt_journal_{postgres,sqlite}.sql` | Folds, function as a template. |
| Durable stream management and account bindings | `sm_durable_sessions`, `sm_durable_operations`, `account_resource_bindings`, `account_resource_operations` | migration V1034 on PostgreSQL, with their epoch guards; the store initializers on SQLite | Folds. |
| Physical attestor | `waddle_attestation.read_physical_identity()` | `install-attestor` | Stays (section 2). |

## 5. Existing database: the stamp

An existing database already has every object. The baseline is recorded as
applied without running its DDL: the retired ledger rows are replaced by row
2000 in one transaction. `MIG/baseline.rs`, `prepare_ledger`.

### Where it runs

Where migrations run, when the process was started with
`WADDLE_DB_BASELINE_ACTION=stamp`:

- PostgreSQL: the first step of `waddle-server owner-phase`
  (`bootstrap_global_owner`), inside the migration transaction, under the
  catalog fence and `MIGRATION_LEDGER_ADVISORY_LOCK_KEY`. The action is an
  environment variable, not a flag, because `install-attestor`, `emit-handoff`
  and the server need the same selection and one environment block
  configures a job's steps.
- SQLite: server startup, under `BEGIN IMMEDIATE`.

### Preconditions, checked in this order before the first write

1. **The ledger is the retired manifest**, row for row:
   `B/postgres.retired-ledger.json` or `B/sqlite.retired-ledger.json`, which
   hold every retired `(version, description, checksum)` as the real binary
   wrote them. Refusals: `RetiredLedgerIncomplete` (versions missing: an older
   release ran last), `RetiredLedgerUnknownVersion`, `RetiredLedgerMismatch`
   (description or checksum differs, or the checksum is NULL),
   `RetiredLedgerMixed` (retired rows together with row 2000 or later).
2. **The schema's structure is the one the history builds.** The server
   computes a fingerprint, one SHA-256 per structural object, and compares it
   with `B/postgres.fingerprint.json` or `B/sqlite.fingerprint.json`. Refusal:
   `BaselineCatalogMismatch`, with the number of differing objects and the
   first twelve by name (`missing`, `changed`, `unexpected`).
   - PostgreSQL: the query is `B/postgres.fingerprint.sql`, generated from the
     same catalog sections as the dump, and run by the server and by the
     tooling unchanged. Each digest is of the object's dump row without owner
     and ACL, with the schema's name replaced by a placeholder, so it does not
     depend on roles or on the schema's name. It covers tables, columns,
     constraints, indexes, sequences, triggers, functions, types, policies,
     rules and extended statistics of the application schema, and the counts
     of object classes the dump does not describe. The schema's name must be
     a plain lower-case identifier.
   - SQLite: the digest of each stored `CREATE` statement with layout and
     comments removed, by the same rule in `MIG/baseline.rs` and in
     `scripts/schema_catalog.py`.
   - Owners and privileges are not part of the fingerprint. On PostgreSQL the
     owner phase verifies them after the stamp, as on every run.
3. **PostgreSQL: the database is exclusive.** No other client backend and no
   prepared transaction, apart from sessions of the same login that carry the
   run's own application name. Refusal: `BaselineStampNotExclusive`. A
   clustered owner phase has already required the same before it reaches the
   runner.

The operator can check precondition 2 beforehand, with the tooling:

    schema_catalog.py fingerprint --postgres-url-env URL --schema <schema> \
        --expected crates/waddle-server/sql/baseline/postgres.fingerprint.json

and compare owners and privileges with `schema_catalog.py dump` and `diff`
against a dump of the harness's `restricted` path, using `--role-alias`.

Which databases qualify: one that last ran the final pre-cut release's owner
phase (PostgreSQL, restricted or cut over from one login: their fingerprints
are equal) or was last started by it (SQLite). A PostgreSQL database that one
login still owns is not stamped: the server refuses the baseline there, and
its structure lacks what only the owner phase creates.

### The write

`DELETE FROM _migrations WHERE version < 2000`, then `INSERT` row 2000 with
the baseline's checksum. The step then reads the ledger back and requires
that it deleted as many rows as it read and that row 2000 is the only row
(`BaselineStampUnverified` otherwise). No DDL runs and no application table
is written, so no OID changes.

The runner then continues as on any baseline database: it validates the
ledger, finds nothing to apply and commits. The rest of the owner phase runs
as on every run.

### Repeat, failure and rollback

- A second run with `stamp` finds row 2000 and no retired row and changes
  nothing.
- Every refusal precedes the first write. Any error rolls the migration
  transaction back; on PostgreSQL the runner reports it only after the
  rollback is acknowledged. The ledger and the schema are as they were.
- A lost commit response is resolved by running again.
- A process without the selection refuses a stamped database: the history
  catalog does not know version 2000. Returning to the history therefore
  needs the old rows back. Before the stamp the operator saves them
  (`COPY (SELECT * FROM _migrations) TO ...`, or the SQLite equivalent);
  rollback is `DELETE FROM _migrations WHERE version = 2000` plus
  re-inserting the saved rows, in one transaction, with every server
  stopped. It is valid only while no migration above 2000 has been applied.
  No side table is kept for this: a table that exists only on stamped
  databases would make them differ from fresh ones.

### With the coordinated cutover and the owner phase

The cutover is not rolling (decision D1). For one PostgreSQL database: stop
every server; save the ledger; run `owner-phase` with
`WADDLE_DB_BASELINE_ACTION=stamp`; `emit-handoff --verify` with `apply`; start
the servers with `apply`.

- The stamp recreates nothing, so the attestor, the handoff's database pins
  and the lineage row stay valid. `install-attestor` is not rerun.
- The runtime's read-only preflight (`MIG/owner/ledger_preflight.rs`) verifies
  the ledger against `MigrationRunner::single()`. A server without the
  selection refuses a stamped database and a server with it refuses an
  unstamped one, each at preflight, before it serves.

## 6. Acceptance criterion

**The schema the baseline builds is catalog-identical to the schema the full
history and every store's bootstrap build, on both engines, and a stamped
database is unchanged apart from its ledger.** `scripts/schema_history.py`
runs both sides with one binary and prints one line per check; it exits 1
when a check fails.

| Check | Compares | Allowed differences |
| --- | --- | --- |
| `history-vs-baseline.postgres` | `restricted` and `baseline` dumps, in full, with row counts and stable-table contents | the attestor comment; rows the baseline database's server wrote while it served |
| `history-vs-baseline.tenant` | the same in schema `tenant_a` | the attestor comment; the ledger's row count |
| `history-vs-baseline.sqlite` | `sqlite` and `baseline-sqlite` | the ledger's row count |
| `*.fingerprint` | the fingerprints of the same pairs, and of `public` against `tenant_a` | none |
| `baseline.postgres.restricted-server-starts` | the server under the runtime login on the baseline database reaches `/ready` | |
| `stamp.*.catalog-unchanged` | a history database before and after the stamp | the ledger's row count |
| `stamp.*.ledger-equals-baseline` | the stamped ledger and a baseline-built database's ledger | none |
| `stamp.*.second-is-a-no-op` | the stamped database before and after a second stamp | none |
| `stamp.*.server-starts` | the server on the stamped database reaches `/ready` | |
| `stamp.postgres.history-catalog-refuses-stamped-database` | `owner-phase` without the selection exits 4 | |
| `apply.refuses-a-retired-ledger`, `stamp.refuses-a-changed-schema`, `stamp.refusal-*`, `baseline.refused-by-one-login-postgres` | each refuses, and the database and its ledger are as before | |

The attestor comment (`read_physical_identity()`) carries the installation's
operation ID and differs between any two installations.

The generator's round trip (`schema_baseline.py round-trip`) proves the SQL
files alone, without the server: structure, role-free privileges, owners and
ACLs through the privileges file, and the static rows.

What the comparison covers and what it does not:

- Covered: schemas, tables, columns in order with type, default, nullability,
  identity, generated expression, collation, storage and compression where
  set; constraints by name and definition; indexes by definition, predicate
  and flags; sequences with parameters and ownership; triggers with enable
  state; functions with body, `prosecdef`, `proconfig`, volatility, strictness,
  parallel safety, cost; views; enum, domain and range types; policies, rules,
  extended statistics; owners; relation, column, sequence, function, type,
  schema and database ACLs with grantor and grant option; default ACLs; roles
  with attributes and memberships; per-database settings; comments; storage
  parameters; privileges on `pg_catalog` objects that differ from `initdb`'s.
- Counted, not described: operators, operator classes and families,
  collations, conversions, text-search objects, base types, casts, foreign
  data wrappers and servers, publications, subscriptions, languages, access
  methods, transforms, security labels. All are zero today; a non-zero count
  shows in the diff and makes the generator refuse.
- Not covered: rows other than the stable tables' (only counts); dropped-column
  slots and other physical layout; a sequence's current value; privileges on
  `information_schema` objects, for which `initdb` records no baseline;
  passwords; cluster-level settings.
- Ordering is by code point in Python. Lists whose order is schema (columns,
  enum labels, inheritance) are ordered by their catalog ordinal. The server's
  collation does not enter.

## 7. Tooling

All under `server/scripts/`; each has `--help`.

- `schema_catalog.py dump|diff|fingerprint`: the canonical catalog document,
  its comparison, and the structural fingerprint the stamp checks.
- `schema_history.py`: builds the history, baseline and stamp paths and
  compares them.
- `schema_baseline.py generate|round-trip|lockstep`: the baseline files, their
  check, and the cross-engine name comparison.
- `schema-engine-differences.txt`: tables one engine alone has.
- `test_schema_catalog.py`, `test_schema_baseline.py`: contract tests, run by
  `check-ci-helper-contracts.sh`. Their PostgreSQL cases need
  `WADDLE_BASELINE_ADMIN_URL` and `psql` and are skipped without them.

The PostgreSQL commands need `psql`, which the dev shell does not provide:
pass `--psql` or set `WADDLE_PSQL`. The harness and the round trip need a
superuser on a disposable cluster and create only `waddle_baseline_*`
databases and roles.

Regenerating the baseline and proving it, from `server/`:

    export WADDLE_BASELINE_ADMIN_URL=postgres://postgres@127.0.0.1:5432/postgres
    export WADDLE_PSQL=/path/to/psql
    BUILD="cargo build -p waddle-server --features clustering --locked \
        --bin waddle-server --bin waddle-cluster-keypool"
    BINS="--server-bin target/debug/waddle-server \
        --keypool-bin target/debug/waddle-cluster-keypool"
    B=crates/waddle-server/sql/baseline

    # 1. The history, with any binary of this commit.
    $BUILD
    python3 scripts/schema_history.py --paths history --out /tmp/a $BINS

    # 2. The baseline files.
    python3 scripts/schema_baseline.py generate /tmp/a/postgres-restricted.json \
        --out $B --commit "$(git rev-parse HEAD)" \
        --fingerprint /tmp/a/postgres-restricted.fingerprint.json \
        --retired-ledger /tmp/a/postgres-restricted.ledger.json
    python3 scripts/schema_baseline.py generate /tmp/a/sqlite-history.json \
        --out $B --commit "$(git rev-parse HEAD)" \
        --fingerprint /tmp/a/sqlite-history.fingerprint.json \
        --retired-ledger /tmp/a/sqlite-history.ledger.json
    python3 scripts/schema_baseline.py round-trip /tmp/a/postgres-restricted.json --directory $B
    python3 scripts/schema_baseline.py round-trip /tmp/a/postgres-tenant.json \
        --schema tenant_a --directory $B --structure-only
    python3 scripts/schema_baseline.py round-trip /tmp/a/sqlite-history.json --directory $B
    python3 scripts/schema_baseline.py lockstep \
        /tmp/a/postgres-restricted.json /tmp/a/sqlite-history.json

    # 3. The binary that embeds them, and every path.
    $BUILD
    python3 scripts/schema_history.py --out /tmp/b $BINS

`git diff --stat $B` after step 2 shows what changed since the files were
last generated. The files are embedded in the server with `include_str!`, so
step 3 rebuilds.

### Keeping the engines in lockstep after the cut

Both baseline files come from one harness run at one commit. From then on a
migration carries SQL for both engines, as now. `schema_baseline.py lockstep`
on the two dumps of a harness run fails when a table, column or explicitly
created index exists on one engine only and is not listed in
`schema-engine-differences.txt`, and when a listing has gone stale. It
compares names, not types or constraint bodies: the engines spell those
differently and no rule maps one to the other.

## 8. Cut checklist

The baseline, the runner entry and the stamp exist. The cut regenerates the
files, makes the baseline the default and deletes what it replaces. Perform
in order.

### What must be regenerated at the cut commit, and why

Other work adds migrations and owner-prepared tables after the files under
`B/` were generated (their headers name the commit). All of these are stale
as soon as that happens, and the stamp refuses every database until they are
regenerated:

| File | Stale when |
| --- | --- |
| `B/postgres.sql`, `B/sqlite.sql` | a migration is added or a store creates or alters anything |
| `B/postgres.fingerprint.json`, `B/sqlite.fingerprint.json` | the same |
| `B/postgres.retired-ledger.json`, `B/sqlite.retired-ledger.json` | a migration is added, or an existing migration's SQL or description changes |
| `B/postgres.fingerprint.sql` | a catalog section in `scripts/schema_catalog.py` changes |
| `B/postgres.bootstrap.sql`, `B/sqlite.bootstrap.sql`, `B/postgres.privileges.sql` | the ledger or lineage table, a role, or a grant changes (round trip only) |

By hand, when the harness or the generator says so:

- `scripts/schema-engine-differences.txt`: a new table on one engine only
  (`lockstep` names it).
- `STATIC_TABLES` in `scripts/schema_baseline.py`: a new migration inserts
  rows that nothing else writes (the baseline database lacks them and
  `history-vs-baseline.*` fails on the row count).
- `OWNER_PREPARED_TABLES` and `OWNER_PREPARED_FUNCTIONS`: a new owner
  preparation that refuses its table without its marker row (the `baseline`
  path's `owner-phase` exits 4 and names the preparation).
- `ROW_TABLES` in `scripts/schema_history.py`: a new table whose rows are the
  same in every deployment.
- The generator refuses an object kind it does not generate (a partitioned
  table, a policy, a domain and so on) and names it; support is added in
  `scripts/schema_baseline.py` with a case in `test_schema_baseline.py`.

### Steps

1. **Freeze.** No migration and no store DDL change merges until the cut
   lands. Call the commit before the cut `OLD`.
2. **Make `webhook_deliveries` eager** on a server that owns its schema
   (`S/server/routes/webhook_delivery.rs`: call `initialize_owner_schema` at
   startup). It is in the PostgreSQL baseline and absent from the SQLite one
   because a fresh SQLite database does not hold it. Add it to neither list
   by hand: after this change the history builds it on both engines. Remove
   its line from `scripts/schema-engine-differences.txt`.
3. **Regenerate and prove**, with the commands of section 7. Required: both
   round trips and the `tenant_a` round trip exit 0, `lockstep` exits 0, and
   `schema_history.py` prints `all N check(s) passed`. Commit `B/`.
4. **Release this as the final pre-cut release.** Every deployment runs its
   owner phase (PostgreSQL) or starts (SQLite) on it. A PostgreSQL deployment
   that one login still owns is cut over to the restricted shape on it.
5. **Convert the owner-prepared stores**, one at a time, each proven by the
   harness: make the preparation accept its table from the baseline without
   its marker row, remove the table from `OWNER_PREPARED_TABLES`, regenerate,
   run the harness.
   - `S/clustering/lease.rs` (`prepare_keypool_owner`): the three keypair
     tables and the four guards.
   - `S/ingress_uow/room_observation/schema.rs` (`prepare_incomplete_marker`).
   - `S/notification_activity/owner.rs`, `S/notification_outbox/schema.rs`.
   - `S/pubsub/owner.rs`, `S/push_service/owner.rs`,
     `S/server/push_notification_owner.rs`.
   A store that is not converted stays owner-prepared; the cut does not wait
   for it.
6. **Switch the default.**
   - `MIG/runner.rs`, `single()`: return the baseline catalog always.
     `MIG/baseline.rs`: `selected()` defaults to `Apply`;
     `WADDLE_DB_BASELINE_ACTION` keeps only `stamp`.
   - `S/main.rs`: remove the refusal for a PostgreSQL server that owns its
     schema if that startup still exists and has been made to accept the
     baseline; otherwise leave it and delete that startup with M6.
7. **Delete the history.**
   - `MIG/global.rs`, `MIG/waddle.rs`, `MIG/namespace.rs` and the
     `owned_namespaces` and per-namespace sentinel logic in `MIG/runner.rs`
     (one sentinel table, `users`); the V1029 branch and
     `apply_sm_frame_provenance`.
   - `MIG/owner.rs` (`verify_applied_through_in_transaction(..., 1023)`),
     `MIG/owner/ledger_preflight.rs` (`_v1023`, `_v1030_*`): name the baseline
     version; the runner's mapping of retired versions to 2000 goes.
   - `MIG/owner/`: unregistered candidates and `transition_lab.rs`.
   - Each folded store's `CREATE` and `ALTER` chain, in the files section 4
     names, leaving attachment, verification and the rows it still writes.
   - `S/clustering/lease.rs`: legacy-slot retirement; `S/owner_cli/args.rs`
     and `S/owner_cli/owner_phase.rs`: `--legacy-keypair-slots`,
     `--legacy-ttl`. `S/bin/cluster_keypool.rs`: stop emitting `CREATE TABLE`.
   - The retired ledgers and fingerprints stay: the stamp needs them for as
     long as an unstamped database may exist.
8. **Pins.**
   - `S/db/physical_attestation/restricted_ingress/manifest.rs`: the
     `include_str!` sources `global.rs`, `waddle.rs`,
     `pending_delivery/database/schema.rs` and `recovery_order.rs` become
     `B/postgres.sql`; `source_sha256` has one entry per remaining source.
   - `server/scripts/generate-restricted-ingress-manifest.py` (`FILES`,
     `render`) and `server/scripts/test-restricted-ingress-manifest.py`: read
     the baseline. Regenerate
     `S/db/physical_attestation/restricted_ingress_manifest_v2.json` as a new
     version; its `functions`, `triggers` and `structure` must equal the old
     file's apart from `source_file` and byte offsets, because no function
     body changed.
   - `S/db/physical_attestation/pinned_entry.rs`, `pinned_receipt.rs`,
     `pinned_selected_entry.rs`, `coverage_gap_gate.rs`,
     `restricted_ingress/receipt_catalog.rs`, `receipt_authority.rs`: the
     V1030 references.
   - `S/server/runtime_owner.rs`: the grant matrices must classify exactly
     the relations and sequences of the baseline database; the harness's full
     comparison includes every ACL.
9. **Tests.**
   - `MIG/tests.rs`: the tests that replay history are replaced by tests of
     a fresh apply, a repeated run, each stamp refusal, and the ordinary
     validation errors for versions above 2000.
   - `MIG/owner/ledger_preflight_tests.rs`, `tests/migration_owner_lab.rs`,
     `tests/migration_owner_lab/`, `tests/postgres_migration_ledger_catalog.rs`,
     `tests/physical_attestation/{pinned_entry,pinned_catalog,pinned_receipt_caller,runtime_startup,restricted_receipt_journal,combined_restricted_profile}.rs`,
     `tests/ingress_relation_guard_lab.rs`, `S/test_support/relation_guard_lab.rs`,
     `S/ingress_uow/room_observation/tests*`, `tests/xep0198_promotion_inventory.rs`:
     version literals and fixtures that build a ledger row by row.
   - `tests/restricted_runtime_startup/`, `tests/restricted_clustered_startup.rs`,
     `tests/owner_bootstrap.rs`: remove legacy-slot retirement cases.
   - `scripts/schema_history.py`: the history paths need the `OLD` binary
     from here on. Keep `OLD`'s output directory; run the baseline and stamp
     paths with the cut binary against copies of `OLD`'s databases
     (`--keep`), and compare with `schema_catalog.py diff`.
10. **Prove the cut.** The workspace suite (`cargo nextest run --workspace
    --all-targets --locked --profile ci`) with the PostgreSQL group; both
    contract tests against PostgreSQL; a stamp of a kept `OLD` restricted
    database and of an `OLD` SQLite file, each followed by a server start and
    a dump compared with `OLD`'s.
11. **Operator documentation** in `server/docs/operations/`: saving the
    ledger, the fingerprint check, `WADDLE_DB_BASELINE_ACTION=stamp`, the
    refusals and the rollback SQL; the stamp's place in
    `server/docs/operations/db-lineage.md`'s cutover order.
12. **Record** in `2046-completion-contracts.md` that M7 is cut, with `OLD`,
    the cut commit and the harness output.

## 9. Decisions

Each was open when the design was written. The more conservative option was
taken every time.

1. **One baseline row, version 2000; the two namespaces are left alone until
   the cut.** Two rows (global and waddle) would need the generated SQL split
   along a boundary the schema does not have: foreign keys cross it. Version
   2000 falls in the waddle namespace, so the existing validation, sentinel
   check and ownership rules apply to it unchanged.
2. **The schema name is a template, and only for objects already in the
   baseline.** The alternative, leaving every schema-naming function to an
   owner preparation, would keep DDL outside the ledger for good. The four
   keypair-pool guards stay owner-prepared for now for a different reason:
   their preparation refuses tables it did not create.
3. **A PostgreSQL server that owns its schema does not take the baseline.**
   Making it accept the baseline would change the one-login stores' DDL, and
   that startup path is being retired by the coordinated cutover. It keeps
   the history until it is deleted.
4. **The baseline is frozen once a release carries it as the default.** A
   changed checksum fails every database that holds it, so after that a
   change is a migration above 2000. Before the cut the files are
   regenerated freely: nothing holds row 2000 except the harness's
   databases.
5. **Tables an owner preparation creates with its marker row stay with that
   preparation.** The alternative was to change nine preparations in stores
   other work is editing. The baseline plus the unchanged owner phase already
   gives the history's catalog; folding them is step 5 of the cut and can be
   done one store at a time.
6. **Rows are static only when nothing writes them after the migrations**, or
   when their store refuses the table without them. Rows a store writes when
   it finds them missing stay with the store, so there is one writer.
7. **The stamp checks structure with a fingerprint the server computes, and
   leaves owners and privileges to the owner phase.** Reimplementing the full
   dump comparison in the server would duplicate the tooling; shipping only
   a name inventory would accept a column of the wrong type. The fingerprint
   query is one generated file both sides run.

Not decided here, because it needs information this work does not have:
whether any deployed database was created by a release whose store
bootstraps produced a different column order or constraint set than today's
do. The fingerprint is of the schema a fresh history build produces at the
generating commit. A database that drifted is refused, by name, and needs
either a migration in the final pre-cut release that brings it to this
shape or an operator decision to rebuild it.
