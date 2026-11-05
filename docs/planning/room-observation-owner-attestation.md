# Room-observation owner and attested schema verification

This plan covers a dark, separately callable owner preparation and read-only
attested verifier for the existing room-observation schema. It advances the
#2046/#2116 storage-evidence work only. It does not connect the verifier to
startup, actors, workers, listeners, or a serving handle, and it makes no
old-writer-drain or role/epoch-activation claim. The existing actor path keeps
using the global database handle.

## Design

1. Add a narrow migration-ledger verification wrapper. In one PostgreSQL
   `READ COMMITTED` transaction, acquire the existing migration advisory lock
   and call `MigrationRunner::verify_applied_through_in_transaction` against
   the explicitly qualified `_migrations` table. Require registered V1023
   (`Give settled room-observer history its own retention clock`), including
   its registered description and PostgreSQL checksum. Set the target schema
   as the transaction's current schema for the runner's ledger-prefix checks.
   This path is read-only: it must not bootstrap, repair, apply, or backfill
   migrations and must not call `apply_pending` or
   `run_dark_postgres_owner`.
2. Implement owner preparation as a separate API over an operator-selected
   PostgreSQL schema-owner handle. Require the live session role to own the
   target schema and all six existing observer tables; V1023 does not transfer
   ownership. Validate the committed ledger and full schema before writing a
   versioned marker. The marker binds the expected lineage UUID and deployment
   UUID to the physical system identifier, database OID/name, and schema
   OID/name. Do not create, repair, or reassign the six data tables or their
   indexes. Grant only the intended runtime permissions, read-only marker
   access, and `pg_monitor` SELECT access; reject grants outside that explicit
   allowlist.
3. Implement the attested verifier over the existing attested main pool. It
   independently checks the supplied `AttestedLineage`, the live physical
   identity and marker binding, and the complete catalog/ACL contract in a
   read-only transaction. Return opaque completion evidence only after every
   check succeeds. It must return no evidence on cancellation, timeout,
   identity mismatch, incomplete marker, or uncertain transaction outcome.
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
own all six data tables. `pg_monitor` must have SELECT on each table. The
runtime role must have only its explicit table allowlist; reject PUBLIC,
column-level, grant-option, or other unexpected ACL entries. Do not infer
ownership from successful DDL or from the V1023 ledger row.

## Focused verification

Add PostgreSQL 17-backed owner/verifier coverage using a one-slot pool. Cover
the positive path, cancellation, incomplete and identity-mismatched markers,
missing/mismatched V1023 ledger version/description/checksum, missing tables,
indexes, predicates, or `pg_monitor` grants, unexpected ACL grants, and a
wrong designated owner. Assert that failed verification performs no DDL and
returns no completion evidence. Repair the existing fixture so the schema
owner's `pg_control_system()` EXECUTE privilege is denied effectively before
the attested-path assertions.

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
