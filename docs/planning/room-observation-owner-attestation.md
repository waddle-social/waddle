# Room-observation owner and attested schema verification

This plan covers a dark, separately callable owner preparation and read-only
attested verifier for the existing room-observation schema. It advances the
#2046/#2116 storage-evidence work only. It does not connect the verifier to
startup, actors, workers, listeners, or a serving handle, and it makes no
old-writer-drain or role/epoch-activation claim. The existing actor path keeps
using the global database handle.

## Design

1. Add a narrow migration-ledger verification wrapper. In one PostgreSQL
   `READ COMMITTED` transaction, acquire the existing lineage advisory lock
   before the migration advisory lock (the existing owner lock order), and
   call `MigrationRunner::verify_applied_through_in_transaction` against the
   explicitly qualified `_migrations` table. Require registered V1023
   (`Give settled room-observer history its own retention clock`), including
   its registered description and PostgreSQL checksum. Set the target schema
   as the transaction's current schema for the runner's ledger-prefix checks.
   This path is read-only: it must not bootstrap, repair, apply, or backfill
   migrations and must not call `apply_pending` or
   `run_dark_postgres_owner`.
2. Implement owner preparation as a separate API over an operator-selected
   PostgreSQL schema-owner handle. Require the live session role to own the
   target schema, `_migrations` ledger, and all six existing observer tables;
   V1023 does not transfer ownership. Require an explicit trusted
   `designated_migrator` login to equal `session_user` and be the schema owner's
   sole SET-only, non-inheriting, non-admin member, with no inbound role
   memberships. Verify effective `SET` on the schema owner and no effective
   `USAGE` inheritance. The owner role must already
   have effective EXECUTE on `pg_catalog.pg_control_system()` after PUBLIC
   access is revoked; the helper must not change this system-function ACL.
   The externally configured physical-attestation definer must already have
   target-schema USAGE and `_lineage` SELECT; target-schema USAGE must be
   owner-granted. The helper checks these effective privileges and the exact
   target-schema grant, but does not provision them.
   Validate the committed ledger and full schema before writing a versioned
   marker. The marker records the designated owner and migrator and binds the
   expected lineage UUID and deployment UUID to the physical system
   identifier, database OID/name, and schema OID/name. Do not create, repair,
   or reassign the six data tables or their indexes. Grant only the intended
   runtime permissions, read-only marker access, and `pg_monitor` SELECT
   access; reject grants outside that explicit allowlist.
3. Implement the attested verifier over the existing attested main pool. It
   independently checks the supplied `AttestedLineage`, the live physical
   identity and marker binding, the externally supplied designated migrator
   against the sole live schema-owner member, the attested physical definer's
   exact schema grant, and the complete catalog/ACL contract in a read-only
   transaction. Return opaque completion evidence
   only after every check succeeds. It must return no evidence on
   cancellation, timeout, identity mismatch, incomplete marker, or uncertain
   transaction outcome.
4. Keep the preparation and verifier unwired. Do not change actor storage
   handles, startup order, worker scheduling, listener behavior, XMPP wire
   behavior, protocol roles, migration activation, or lineage/epoch
   transitions.

## Catalog and ACL contract

Verify these six tables against the existing schema definitions and V1023:

- `extension_room_observers`
- `extension_room_sources`
- `extension_room_source_revisions`
- `extension_room_observation_work`
- `extension_room_observation_receipts`
- `extension_room_publications`

Require both startup indexes (`extension_room_observation_work_due` and
`extension_room_publications_pending`) and all eight PostgreSQL V1023 indexes:
`extension_room_observation_work_settled`,
`extension_room_observation_work_source`,
`extension_room_publications_settled`,
`extension_room_publications_source`,
`extension_room_observation_receipts_recorded`,
`extension_room_sources_retracted_captured`,
`extension_room_source_revisions_source`, and
`extension_room_observation_work_active_guard`. Compare full index definitions,
including all four partial predicates (`status IN ('completed', 'terminal',
'stale')`, `status IN ('published', 'stale')`, `retracted = 1`, and
`status IN ('pending', 'leased', 'started')`).

Also verify columns, types, nullability, defaults, primary/unique constraints,
and the absence of unexpected triggers or indexes. The designated owner must
own the schema, `_migrations`, and all six data tables; V1023 does not transfer
ownership. The designated migrator is an external verifier input, so rewriting
the marker cannot authorize a replacement membership. Check the designated
migrator's incoming and outgoing memberships and effective `SET`/`USAGE` state.
The shared schema may contain unrelated relations. Reject runtime-readable
owner-rights views or materialized views in any schema when an effective
`SELECT`, `INSERT`, `UPDATE`, or `DELETE` grant exceeds runtime access to any
recursively referenced relation in the target schema, including views whose
schema the runtime cannot use directly. Reject callable
runtime-executable `SECURITY DEFINER` routines owned by the designated schema
owner anywhere in the database, including routines called through views from a
schema the runtime cannot use directly. Trigger functions cannot be invoked
directly, but reject a security-definer trigger attached to a target-schema
relation the runtime can mutate, or a target-owner trigger function attached to
a runtime-writable relation in another schema. These checks let unrelated
relations remain while blocking alternate paths around base-relation
privileges. `pg_monitor` must have SELECT on each table. The runtime role must
have only its explicit table allowlist; reject PUBLIC,
column-level, grant-option, or other unexpected ACL entries. Do not infer
ownership from successful DDL or from the V1023 ledger row.

## Focused verification

Add PostgreSQL 17-backed owner/verifier coverage using a one-slot pool. Cover
the positive path, cancellation, incomplete and identity-mismatched markers,
missing/mismatched V1023 ledger version/description/checksum, missing tables,
indexes, predicates, or `pg_monitor` grants, unexpected ACL grants, and a
wrong designated owner or migrator. Include an owner-member swap where the new
member rewrites the marker and the verifier still rejects it, plus a grant of
the designated migrator role to an outsider. Test an owner-created callable
SECURITY DEFINER routine both in the target schema and another runtime-usable
schema, an unrelated readable table/view, SELECT-only and UPDATE-only views
over a relation the runtime cannot access in the target schema, a SELECT-only
view in another runtime-usable schema, and a column-only SELECT grant on a
target view. Verify that column-only INSERT and UPDATE rights on a partition
parent can fire SECURITY DEFINER triggers attached to a child partition.
Add an owner-controlled `=(oid,oid)` shadow operator beside an
executable owner `SECURITY DEFINER` routine and prove the catalog check still
rejects the routine. Also attach a security-definer trigger to a runtime-writable
unrelated relation and prove it can mutate the owner marker before asserting
the verifier rejects it. Assert that failed verification performs no DDL and
returns no completion evidence.
Repair the existing fixture so the schema owner's `pg_control_system()`
EXECUTE privilege is denied effectively before the attested-path assertions.

Verification completed:

- `rustfmt --edition 2021` passed on every touched Rust file; `git diff --check`
  passed.
- The focused PostgreSQL 17 test passed: `1 passed; 0 failed; 5433 filtered
  out; finished in 0.73s` (the test target build emitted existing vendor and
  unrelated test-code warnings).
- Scoped `cargo clippy -p waddle-server --lib --all-features --locked -- -D warnings -A unused_imports -A dead_code` passed. The two relaxed warning classes are pre-existing unused/dead store-preparation code outside this diff; unrelaxed `-D warnings` identified those alongside two new `map_or` simplifications, which were fixed.
- `cargo check -p waddle-server --no-default-features --locked` passed with existing unused/dead-code warnings.

```sh
CARGO_TARGET_DIR=/private/tmp/waddle-2191-push-store-impl/server/target \
CARGO_INCREMENTAL=0 \
WADDLE_TEST_REQUIRE_POSTGRES=1 \
WADDLE_TEST_POSTGRES_URL=postgresql://rawkode@127.0.0.1:55449/postgres \
cargo test -p waddle-server --lib --all-features --locked -- \
  postgres17_room_observation_owner_and_attested_verifier_contract --nocapture
```

The scoped Rust structure check was also attempted; it reports only existing
wildcard/multi-module imports in untouched `lease_runtime_attach_tests.rs` and
`notification_outbox/runtime_attach_tests.rs`. CI and production startup
checks were not run; this slice remains unwired.

Run only the focused room-observation PostgreSQL tests, with PostgreSQL 17
forced through `WADDLE_TEST_POSTGRES_URL` and
`WADDLE_TEST_REQUIRE_POSTGRES=1`, plus formatting and the scoped Rust checks
needed by touched code. Coordinate Cargo use with the integration owner because
the server target directory is shared and disk space is constrained. CI remains
disabled under the user's direction.

## Delivery gates

- Keep this plan as the first signed Conventional Commit on an isolated branch
  based on the verified `origin/main` commit.
- Have the integration owner review and push the plan, then open a draft PR
  before non-trivial implementation.
- After implementation, update this plan with the exact verification results
  and any remaining evidence limits. Do not push or merge without integration
  owner review.
