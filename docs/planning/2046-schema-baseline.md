# #2046 M7: schema baseline design and cut procedure

Status: design and tooling. The baseline is not cut. The migration catalogs,
the ledger validation, the owner phase and every store's schema bootstrap are
unchanged. This document defines what the cut is, how it is checked, and the
ordered steps that perform it. Scope and hazards are in
[the completion contracts, section "M7: schema baseline"](2046-completion-contracts.md).

Paths: `S/` is `server/crates/waddle-server/src/`, `X/` is
`server/crates/waddle-xmpp/src/`, `MIG/` is `S/db/migrations/`.

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

**One baseline migration per engine, in the existing ledger.** The catalog
becomes a single entry, version 2000, description `schema baseline`, with
`sql_postgres` and `sql_sqlite` taken from two files:

    server/crates/waddle-server/sql/baseline/postgres.sql
    server/crates/waddle-server/sql/baseline/sqlite.sql

Later schema changes are ordinary migrations numbered from 2001. The two
namespaces collapse into one: version 2000 is above every existing and
in-flight version, so "everything below 2000 is retired history" is a test on
the version alone, and a pre-cut binary that meets a stamped database fails
with `UnknownVersion` instead of serving.

The baseline SQL uses plain `CREATE`, not `CREATE ... IF NOT EXISTS`. It runs
exactly once per database, inside the migration transaction, on a database
whose ledger is empty; an object that already exists there is an error the
runner should surface, not skip.

The baseline holds **structure and the static rows the migrations insert**
(`ingress_protocol_epoch`, `ingress_epoch_guard_manifest`). It names no role.
On PostgreSQL it runs as the schema owner with the application schema first on
the search path, as migrations do now, so objects belong to whoever runs it
and resolve in whichever schema the deployment uses.

### What stays outside the baseline, and why

| Stays outside | Mechanism | Reason |
| --- | --- | --- |
| Roles, memberships, database owner and ACL, schema owner and ACL, `pg_control_system()` ACL, the attestor schema | `sql/restricted_runtime_provisioning.sql` | Needs a superuser; the migrator login must not be able to do any of it. Role names are deployment inputs. |
| Runtime privileges | `grant_runtime` and the grant matrix in `S/server/runtime_owner.rs` | The runtime role's name is a deployment input, and the phase verifies the result (`verify_runtime_role`) against the same matrix. |
| `waddle_attestation.read_physical_identity()` | `install-attestor` | Created by a different login as a different owner; its OID is pinned by the handoff and its comment records the operation ID. |
| The `_migrations` table | `MIG/sql.rs` and `MIG/postgres_ledger.rs` | The runner creates and inspects its own ledger before it applies anything. |
| Deployment-derived rows: `_lineage`, `clustering_keypool`, `clustering_keypool_slots`, `clustering_peer_allowlist`, `push_service_vapid_keys`, the owner-state and schema-marker rows | the owner phase's lineage, clustering, VAPID and store preparations | They are computed from the deployment UUID, the keypair pool, the session key or the database's identity. |

Everything else folds in. After the cut a store's owner preparation creates
nothing: it verifies the relations it attaches to and writes its
deployment-derived rows.

### Six functions carry the schema's name

The generator reports every object whose definition spells out a deployment
name. At `68c8ae9a4` there are six, all naming the schema:

- `clustering_keypair_slot_guard`, `clustering_keypair_truncate_guard`,
  `clustering_keypool_guard`, `clustering_keypool_slots_guard`:
  `SET search_path TO 'pg_catalog', '<schema>', 'pg_temp'`
  (`S/clustering/lease.rs`);
- `pubsub_runtime_attach_lock`: `LOCK TABLE "<schema>"."..."` under
  `search_path = pg_catalog, pg_temp` (`S/pubsub/owner.rs`);
- `waddle_p21_record_effect_receipt`: `<schema>.`-qualified calls and
  statements (`MIG/owner/p21_receipt_journal_postgres.sql`).

Decision: they fold in, and the PostgreSQL baseline is a template with the
placeholders the owner SQL assets already use (`__SCHEMA__` for the quoted
identifier, as rendered by `scripts/generate-restricted-ingress-manifest.py`).
The runner renders the template with the application schema before it applies
it; the checksum is of the template. The generator does not write the
placeholder: step 6 of the cut replaces the reported occurrences by hand, and
the round trip after rendering proves the replacement. If the template is
rejected at the gate, the alternative is that these six functions and the six
keypair-pool triggers stay in their owner preparations; the acceptance check
in section 6 is the same either way.

## 3. Fresh database

PostgreSQL, restricted:

1. `sql/restricted_runtime_provisioning.sql` as a superuser.
2. `owner-phase`: the runner creates `_migrations`, applies the baseline in one
   transaction under the catalog fence and the ledger lock, and inserts row
   2000. Each store preparation then verifies its relations and writes its
   deployment-derived rows. `grant_runtime`, `verify_runtime_role`.
3. `install-attestor`, `emit-handoff`.

SQLite: the server starts, the runner applies the baseline under
`BEGIN IMMEDIATE`, and each store attaches. SQLite keeps single-role startup.

A PostgreSQL server that starts without an attestation applies the same
baseline, so it gets the restricted structure, including the nine tables and
the guards the legacy startup does not create today. That is intended: there
is one PostgreSQL schema.

## 4. Per-store decisions

"Folds" means the DDL moves into the baseline and the store's bootstrap keeps
only attachment, verification and the listed rows.

| Store | Tables | DDL today | Decision |
| --- | --- | --- | --- |
| Lineage | `_lineage` | `S/db/lineage/sql.rs` | Table folds. The row stays with enrollment: it is the deployment's identity. |
| Inbox | `inbox_entries`, `groupchat_notification_recovery` | `S/inbox/schema.rs` | Folds. |
| PubSub | `pubsub_nodes`, `pubsub_items`, `pubsub_subscriptions`, `pubsub_affiliations`, `pubsub_owner_configured`, `pubsub_push_publications`, `pubsub_schema_version`, `dnd_projection`, `dnd_projection_source_version`, `notification_settings_projection`, `notification_settings_projection_source_version`, `pubsub_schema_marker` | `S/pubsub/schema.rs`, `S/pubsub/owner.rs` | Folds, with `pubsub_runtime_attach_lock()` as a template function. `pubsub_schema_version`'s row is static and folds; the marker row stays, it records the owner phase. |
| Stream management, portable and fenced | `sm_sessions`, `sm_unacked`, `sm_acknowledged_windows`, `sm_ingress_appends`, defined by the catalog and again by the store's `CREATE ... IF NOT EXISTS` and `ADD COLUMN` chain | `S/sm_persistence/schema.rs`, `S/sm_persistence/implementation.rs`, `S/sm_persistence_fenced/implementation.rs` | Folds; one definition remains. The V1029 special case in `MIG/runner.rs` goes with the history. |
| Pending delivery | `pending_delivery`, `pending_delivery_sm_promotions`, `pending_delivery_startup_migrations`; SQLite `pending_notification_recovery_counter` | `S/pending_delivery/database/schema.rs`, `recovery_order.rs` | Folds. The two `pending_delivery_startup_migrations` rows mark the store's one-time startup steps as done; they fold as static rows so an attach finds them. |
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
| Clustering claims and nodes | `clustering_claims`, `clustering_nodes`, `clustering_steal_intents`, `clustering_orphan_reaper_cursors`, `clustering_claim_epoch_seq` | `S/clustering/claims.rs` | Folds, PostgreSQL only. The two cursor rows are static and fold. |
| Keypair slots and pool | `clustering_keypair_slots`, `clustering_keypool`, `clustering_keypool_slots`, four guards, six triggers | `S/clustering/lease.rs` | Folds in the enrolled shape, guards as template functions. The enrollment rows stay. The legacy-slot retirement (`--legacy-keypair-slots retire`) is deleted: no database holds the unenrolled table after the cut. |
| Peer allowlist | `clustering_peer_allowlist` | `S/clustering/allowlist.rs`, `S/bin/cluster_keypool.rs` | Table folds. Rows stay with the plan. `waddle-cluster-keypool --sql-output` stops emitting `CREATE TABLE`. |
| MAM | `mam_messages` | `X/mam/storage/sqlx_store/schema.rs`, `MIG/owner/mam.rs` | Folds. The `waddle-xmpp` initializer keeps its DDL for the crate's own tests and stops being called by the server's owner phase. |
| Webhook ledger | `webhook_deliveries` | `S/server/routes/webhook_delivery.rs` | Folds on both engines. This adds the table to SQLite at startup, where it is lazy today. |
| P21 receipt journal | `p21_receipt_operation_results`, `waddle_p21_record_effect_receipt` | `MIG/owner/p21_receipt_journal_{postgres,sqlite}.sql` | Folds, function as a template. |
| Physical attestor | `waddle_attestation.read_physical_identity()` | `install-attestor` | Stays (section 2). |

## 5. Existing database: the stamp

An existing database already has every object. The baseline must be recorded
as applied without running its DDL. That is a ledger rewrite: the 45-plus
retired rows are replaced by row 2000 in one transaction.

### Retired-ledger manifest

The binary that stamps no longer contains the SQL the old checksums were
computed from. It carries instead a constant table, per engine, of every
retired `(version, description, checksum)`, captured at the cut commit from
`postgres-restricted.ledger.json` and `sqlite-history.ledger.json`, which
`schema_history.py` writes from databases the real binary migrated.

### Preconditions

1. The database last ran the **final pre-cut release** to completion: on
   PostgreSQL its `owner-phase`, on SQLite a start. That release creates every
   folded object, including the ones a first-use bootstrap would create later.
   A PostgreSQL database that still has the legacy one-login shape is cut over
   to the restricted shape with that release first; it is not stamped as it
   is, because it lacks the 128 objects of section 1.
2. Its ledger equals the retired manifest exactly: every version present,
   every description and checksum equal, none extra. A proper prefix means an
   older release was the last to run; the stamp refuses and names the missing
   versions.
3. Its catalog equals the baseline's. The operator proves this with the
   tooling, before the stamp, against the reference document published with
   the cut (step 9):

       schema_catalog.py dump --postgres-url-env URL --schema <schema> \
           --role-alias <owner>=waddle_baseline_owner ... -o deployed.json
       schema_catalog.py diff --only-schema <schema> reference.json deployed.json

   Exit 0 is required. Row counts are not part of it.
4. No server is connected. A clustered owner phase already requires this
   (`ExclusiveDatabase::Required`); an unclustered one and SQLite rely on the
   operator, as today.

### The step

The stamp is part of the migration runner and runs where migrations run: the
first step of `owner-phase` (`bootstrap_global_owner`) on PostgreSQL, startup
on SQLite. It is opt-in per run, like lineage enrollment:
`WADDLE_DB_BASELINE_ACTION=stamp`. Without it, a database whose ledger holds
retired versions is refused with an error that names the action.

Inside the migration transaction (PostgreSQL: catalog fence, then
`MIGRATION_LEDGER_ADVISORY_LOCK_KEY`; SQLite: `BEGIN IMMEDIATE`):

1. Read the ledger. If it holds row 2000 and no retired row, the database is
   already on the baseline: continue as an ordinary run.
2. Otherwise require precondition 2 against the manifest for this engine.
3. Check the name inventory: every table, column and explicitly created index
   the baseline defines exists, and the application schema holds no table the
   baseline does not define. The inventory is a constant generated with the
   baseline. This is a guard against stamping the wrong database; it is not
   the equivalence proof, which is precondition 3.
4. `DELETE` the retired rows, `INSERT` row 2000 with the baseline checksum.
5. Commit. No DDL runs, so no OID changes.

### Failure and rollback

- Any refusal or error rolls the transaction back; the ledger is unchanged
  and the run can be repeated.
- A lost commit response is resolved by running again: the runner finds
  either the retired ledger (and stamps) or row 2000 (and continues).
- Returning to the pre-cut release after a stamp needs the old rows back,
  because that binary refuses version 2000. Before the stamp the operator
  saves them (`COPY (SELECT * FROM _migrations) TO ...`, or the SQLite
  equivalent); rollback is `DELETE FROM _migrations WHERE version = 2000`
  plus re-inserting the saved rows, in one transaction, with every server
  stopped. It is valid only while no migration above 2000 has been applied.
  No side table is kept in the database for this: a table that exists only on
  stamped databases would make them differ from fresh ones.

### With the coordinated cutover and the owner phase

The cutover is not rolling (decision D1). The order for one PostgreSQL
database is: stop every server; save the ledger; run the cut release's
`owner-phase` with `WADDLE_DB_BASELINE_ACTION=stamp`; `emit-handoff --verify`;
start the servers.

- The stamp recreates nothing, so the attestor, the handoff's database pins
  and the lineage row stay valid. `install-attestor` is not rerun.
- The runtime's read-only preflight (`MIG/owner/ledger_preflight.rs`) pins
  the complete registered ledger. Before the cut that is the V1030 prefix,
  after it row 2000. A pre-cut server therefore refuses a stamped database
  and a cut server refuses an unstamped one, each at preflight, before it
  serves.
- The owner phase after the stamp runs the same store preparations as on a
  fresh database. On a database that passed precondition 3 they find
  everything in place and write nothing new.

## 6. Acceptance criterion

**The schema "baseline only" builds is catalog-identical to the schema "full
history plus every store's bootstrap" builds, on both engines.** Concretely,
with `OLD` the last commit before the cut and `NEW` the cut commit, and
`schema_history.py` run with each commit's binary:

    schema_catalog.py diff OLD/postgres-restricted.json NEW/postgres-restricted.json --row-counts
    schema_catalog.py diff OLD/sqlite-history.json      NEW/sqlite-history.json      --row-counts

Both exit 0, except for:

- the comment on `read_physical_identity()`, which differs between any two
  installations;
- on SQLite, `webhook_deliveries`, present in `NEW` only, unless the final
  pre-cut release made its creation eager (step 1), in which case there is no
  exception.

And on a copy of an `OLD` restricted database, stamped by `NEW`:

    schema_catalog.py diff OLD/postgres-restricted.json STAMPED.json

exits 0 with the same comment exception.

The generator's own round trip (`schema_baseline.py round-trip`) proves the
generated SQL before it is wired in. The criterion above proves the wired
result, including whatever the owner phase still does.

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
- Not covered: rows (only counts, on request); dropped-column slots and other
  physical layout; a sequence's current value; privileges on
  `information_schema` objects, for which `initdb` records no baseline;
  passwords; cluster-level settings.
- Ordering is by code point in Python. Lists whose order is schema (columns,
  enum labels, inheritance) are ordered by their catalog ordinal. The server's
  collation does not enter. Two independent builds at the same commit give
  byte-identical dumps apart from the attestor comment, and a byte-identical
  generated candidate.

## 7. Tooling

All under `server/scripts/`; each has `--help`.

- `schema_catalog.py dump|diff`: the canonical catalog document and its
  comparison.
- `schema_history.py`: builds the four paths and dumps them, with ledgers.
- `schema_baseline.py generate|round-trip|lockstep`: candidate SQL, its
  check, and the cross-engine name comparison.
- `schema-engine-differences.txt`: tables one engine alone has.
- `test_schema_catalog.py`, `test_schema_baseline.py`: contract tests, run by
  `check-ci-helper-contracts.sh`. Their PostgreSQL cases need
  `WADDLE_BASELINE_ADMIN_URL` and `psql` and are skipped without them.

The PostgreSQL commands need `psql`, which the dev shell does not provide:
pass `--psql` or set `WADDLE_PSQL`. The harness and the round trip need a
superuser on a disposable cluster and create only `waddle_baseline_*`
databases and roles.

    cd server
    cargo build -p waddle-server --features clustering --locked \
        --bin waddle-server --bin waddle-cluster-keypool
    export WADDLE_BASELINE_ADMIN_URL=postgres://postgres@127.0.0.1:5432/postgres
    export WADDLE_PSQL=/path/to/psql

    python3 scripts/schema_history.py --out /tmp/schema-a \
        --server-bin target/debug/waddle-server \
        --keypool-bin target/debug/waddle-cluster-keypool

    python3 scripts/schema_baseline.py generate /tmp/schema-a/postgres-restricted.json \
        --out crates/waddle-server/sql/baseline-candidate --commit "$(git rev-parse HEAD)"
    python3 scripts/schema_baseline.py generate /tmp/schema-a/sqlite-history.json \
        --out crates/waddle-server/sql/baseline-candidate --commit "$(git rev-parse HEAD)"

    python3 scripts/schema_baseline.py round-trip /tmp/schema-a/postgres-restricted.json \
        --directory crates/waddle-server/sql/baseline-candidate
    python3 scripts/schema_baseline.py round-trip /tmp/schema-a/sqlite-history.json \
        --directory crates/waddle-server/sql/baseline-candidate
    python3 scripts/schema_baseline.py lockstep \
        /tmp/schema-a/postgres-restricted.json /tmp/schema-a/sqlite-history.json

`server/crates/waddle-server/sql/baseline-candidate/` holds the generator's
output for `68c8ae9a4`. Nothing reads it.

### Keeping the engines in lockstep after the cut

Both baseline files come from one harness run at one commit. From then on a
migration carries SQL for both engines, as now. `schema_baseline.py lockstep`
on the two dumps of a harness run fails when a table, column or explicitly
created index exists on one engine only and is not listed in
`schema-engine-differences.txt`, and when a listing has gone stale. It
compares names, not types or constraint bodies: the engines spell those
differently and no rule maps one to the other.

## 8. Cut checklist

Perform in order. Each step names what it touches. Do not start before the
in-flight migration work (mailbox, stream management, account authority) has
merged: the baseline is regenerated from whatever the catalog holds then.

1. **Make the final pre-cut release complete.** Every folded object must exist
   after that release's owner phase or start.
   - `S/server/routes/webhook_delivery.rs`: create the ledger at startup on a
     server that owns its schema, not on first use.
   - Run `schema_history.py` and confirm `legacy-vs-restricted.structure.diff`
     lists nothing but the objects of section 1 and row counts, and that
     `restricted-vs-upgraded.full.diff` lists only the attestor comment and
     row counts.
   - Release it. Every deployment runs its owner phase (or starts, on SQLite)
     on this release before it meets the cut release.
2. **Freeze.** No migration and no store DDL change merges between the
   pre-cut release tag `OLD` and the cut.
3. **Build schema A at `OLD`.** `schema_history.py --out a/` with `OLD`'s
   binaries. Keep `a/` for the whole cut: the dumps, the `.ledger.json` files
   and the diffs.
4. **Generate.** `schema_baseline.py generate` for both engines into
   `server/crates/waddle-server/sql/baseline/`; delete
   `sql/baseline-candidate/`. Run both round trips and `lockstep`; all exit 0.
   Diff the result against the committed candidate to review what the
   in-flight work added.
5. **Add the static rows.** Append to both baseline files the `INSERT`
   statements for rows the retired migrations and bootstraps wrote and no
   owner preparation writes: `ingress_protocol_epoch`,
   `ingress_epoch_guard_manifest`, `pending_delivery_startup_migrations`,
   `pubsub_schema_version`, `clustering_orphan_reaper_cursors`, SQLite
   `pending_notification_recovery_counter`, and any table the round trip's
   "holds rows" note lists that section 2 does not assign to the owner phase.
   Copy the statements from `MIG/waddle.rs` and the store files of section 4.
6. **Template the schema name.** In `sql/baseline/postgres.sql`, replace the
   schema name in each object `generate` reported as "deployment name in the
   structure" with the placeholder. Render with `public` and rerun the
   PostgreSQL round trip; exit 0.
7. **Replace the catalogs.**
   - `MIG/global.rs`, `MIG/waddle.rs`: delete. New `MIG/baseline.rs` with the
     one `Migration` (version 2000) that `include_str!`s the two files, the
     retired-ledger manifest from `a/postgres-restricted.ledger.json` and
     `a/sqlite-history.ledger.json`, and the name inventory.
   - `MIG/namespace.rs`: delete; `MIG/runner.rs`: remove `owned_namespaces`,
     the per-namespace sentinel tables in `validate_schema_has_ledger` (one
     sentinel, `users`), the V1029 branch at line 195 and
     `apply_sm_frame_provenance`; add the template rendering and the stamp of
     section 5 with `WADDLE_DB_BASELINE_ACTION`.
   - `MIG/ledger_error.rs`: errors for a retired ledger without the action,
     an incomplete retired ledger and an inventory mismatch.
   - `S/config.rs`: parse `WADDLE_DB_BASELINE_ACTION`.
     `S/owner_cli/args.rs`: document it under `owner-phase`.
   - `MIG/checksum.rs`: unchanged; the baseline checksum is the hash of the
     template.
8. **Delete the folded DDL and `ALTER` chains.** For each "Folds" row of
   section 4, reduce the store's bootstrap to attachment, verification and its
   remaining rows, in the files that row names. Also:
   - `S/clustering/lease.rs`: remove legacy-slot retirement, and
     `convert_definer_guards_owner` if no supported database can still hold
     definer-owned guards; `S/owner_cli/args.rs`,
     `S/owner_cli/owner_phase.rs`: remove `--legacy-keypair-slots` and
     `--legacy-ttl`.
   - `S/bin/cluster_keypool.rs`: stop emitting `CREATE TABLE`.
   - `MIG/owner/`: delete unregistered candidates and `transition_lab.rs`;
     keep what the owner phase still calls.
   - `S/sm_persistence/revisioned/schema.rs` and other DDL no path executes:
     delete with their callers or leave out of the baseline, per M5's outcome.
9. **Publish the reference documents.** Build schema A again with the cut
   binary (`b/`). Commit `b/postgres-restricted.json` and
   `b/sqlite-history.json` as `sql/baseline/postgres.catalog.json` and
   `sqlite.catalog.json`: they are the reference of precondition 3 and of
   later lockstep checks.
10. **Owner phase and grant matrix.**
    - `S/server/runtime_owner.rs`: `prepare_restricted_runtime_owner` keeps its
      order; each step now verifies. `TABLE_GRANTS`, `CLUSTERED_TABLE_GRANTS`,
      `COLUMN_UPDATE_GRANTS`, `SEQUENCE_GRANTS`, `CLUSTERED_SEQUENCE_GRANTS`
      must classify exactly the relations and sequences of
      `postgres.catalog.json`; the ACLs in `a/` and `b/` must be equal, which
      the acceptance diff checks.
    - `S/db/bootstrap.rs`: `bootstrap_shared_owner_postgres` and
      `bootstrap_global_owner` run the baseline runner.
    - `sql/restricted_runtime_provisioning.sql`: unchanged.
11. **Update the pins.**
    - `MIG/owner.rs` (`verify_applied_through_in_transaction(..., 1023)`),
      `MIG/owner/ledger_preflight.rs` (`_v1023`, `_v1030_*` entry points and
      the literal versions): pin row 2000; rename the entry points.
    - `S/db/physical_attestation/restricted_ingress/manifest.rs`: the
      `include_str!` sources `global.rs`, `waddle.rs`,
      `pending_delivery/database/schema.rs` and `recovery_order.rs` become the
      baseline file; `source_sha256` has one entry per remaining source.
    - `server/scripts/generate-restricted-ingress-manifest.py` (`FILES`,
      `render`) and `server/scripts/test-restricted-ingress-manifest.py`:
      read the baseline. Regenerate
      `S/db/physical_attestation/restricted_ingress_manifest_v2.json` as a new
      version. Its `functions`, `triggers` and `structure` sections must equal
      the old file's apart from `source_file` and byte offsets, because no
      function body changed; diff them.
    - `S/db/physical_attestation/pinned_entry.rs`, `pinned_receipt.rs`,
      `pinned_selected_entry.rs`, `coverage_gap_gate.rs`,
      `restricted_ingress/receipt_catalog.rs`, `receipt_authority.rs`: replace
      the V1030 references.
12. **Tests.**
    - `MIG/tests.rs`: the runner tests that replay history (roughly 85) are
      replaced by tests of a fresh apply, a repeated run, the stamp (complete
      ledger, proper prefix, wrong checksum, extra version, inventory
      mismatch, missing action, repeat after success) and the ordinary
      validation errors for versions above 2000.
    - `MIG/owner/ledger_preflight_tests.rs`, `tests/migration_owner_lab.rs`,
      `tests/migration_owner_lab/`, `tests/postgres_migration_ledger_catalog.rs`,
      `tests/physical_attestation/{pinned_entry,pinned_catalog,pinned_receipt_caller,runtime_startup,restricted_receipt_journal,combined_restricted_profile}.rs`,
      `tests/ingress_relation_guard_lab.rs`, `S/test_support/relation_guard_lab.rs`,
      `S/ingress_uow/room_observation/tests*`, `tests/xep0198_promotion_inventory.rs`:
      update version literals and fixtures that build a ledger row by row.
    - `tests/restricted_runtime_startup/`, `tests/restricted_clustered_startup.rs`,
      `tests/owner_bootstrap.rs`: remove legacy-slot retirement cases; add an
      owner phase that stamps.
    - `server/scripts/check-ci-helper-contracts.sh` keeps the two contract
      tests; add `python3 scripts/schema_baseline.py round-trip
      crates/waddle-server/sql/baseline/sqlite.catalog.json --directory
      crates/waddle-server/sql/baseline`-style check for SQLite, which needs
      no server.
13. **Prove it.** With the cut binary:
    - the acceptance diffs of section 6 between `a/` and `b/`;
    - stamp a kept copy of an `OLD` restricted database
      (`schema_history.py --keep` at `OLD`, then the cut `owner-phase` with
      the action) and diff its dump against `a/postgres-restricted.json`;
      repeat for an `OLD` SQLite file;
    - the workspace suite, `cargo nextest run --workspace --all-targets
      --locked --profile ci`, with the PostgreSQL group;
    - `python3 scripts/test_schema_catalog.py` and `test_schema_baseline.py`
      against PostgreSQL.
14. **Document the operator steps** in `server/docs/operations/`: saving the
    ledger, precondition 3's two commands, the action, the rollback SQL. Add
    the stamp to `server/docs/operations/db-lineage.md`'s cutover order.
15. **Record** in `2046-completion-contracts.md` that M7 is cut, with `OLD`,
    the cut commit and the acceptance output.

## 9. Open at the gate

- Version 2000 and the single namespace, against keeping two baseline rows
  (global and waddle). Two rows would need the generated SQL split by a
  boundary the schema does not have: foreign keys cross it.
- The schema-name template, against leaving the six functions in owner
  preparations.
- Whether the legacy one-login PostgreSQL startup still exists at the cut. If
  it does, it gains nine tables and the guards (section 3). If M6 removed it,
  `bootstrap_shared_owner_postgres` goes with it and step 10 shrinks.
- Whether to freeze the baseline after the first release that carries it. A
  checksum change fails every existing database (`ChecksumMismatch`), so
  after release a change is a migration above 2000. While no deployment holds
  data worth keeping, cutting again by this procedure is the cheaper way to
  fold later migrations.
