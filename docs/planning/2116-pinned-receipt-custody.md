# P2.1 dark pinned receipt custody

Status: implementation plan for #2116 and #2046. Both restricted-startup
refusals remain active; this work cannot enable serving.

## Scope

Convert the already-proven live executor receipt continuation to a dark,
test-selected exact pinned receipt-pool authority. The fixture's ordinary
executor pool shares lineage but is not a selected shared main pool. Derive a
stable, domain-separated,
nonzero operation UUID from the committed effect-intent receipt identity, or
persist the UUID with that source. A complete immutable receipt tuple and UUID
must be admitted synchronously before any cancellable await. An owned bounded
supervisor retains that operation and its original
  attempt until exact positive journal readback or an explicitly unresolved
  outcome. It must not rerun the external effect or treat a missing row as
  rollback. Process-crash rediscovery is a separate serving gate unless this
  group can prove the durable source of both UUID and tuple and preserve the
  original-attempt fence. The ordinary
  legacy and SQLite paths remain available under their current selection.

Refactor the existing pinned receipt pilot only as needed to reuse a private
non-Clone, exact-backend transaction path. Its first post-BEGIN statement
tries catalog-S; a failed try gets an acknowledged rollback before retry. The
same READ COMMITTED transaction checks pinned lineage, V1030 ledger, epoch,
manifest, restricted receipt role/function/ACL profile and singleton claim,
then calls the guarded receipt function and commits. No raw pool, SQL callback,
transaction, or separable proof escapes the authority. Do not open a second
checkout or nest another transaction for receipt work.

## Architecture gates

- Preserve the existing all-sibling proof, transaction-owned exclusions and
  relay-carbon exclusion at the live executor's synchronous handoff.
- The old five-second worker timeout may not drop a twenty-second pinned
  transaction. The supervisor owns bounded execution and retains uncertainty
  across caller cancellation, checkout, BEGIN, SQL and COMMIT. Original
  primary/backend/XID custody and exact readback must be distinguished from a
  row merely absent on another connection.
- Existing S is a NOLOGIN protected-table owner; the migrator can SET ROLE S
  and therefore has owner DML powers while assumed. Treat that path as a
  privileged mutator requiring the trusted catalog protocol or independently
  proven quiescence. Do not claim the restricted runtime's R/A profile proves
  the entire S/R/A/H/T/I/M/migrator graph.
- Keep frame completion, observers, ordinary UOWs, scheduler/GC,
  terminalization, other stores, old writers, provider role changes, and
  production selection outside this dark group. Do not enable either startup
  path or restore GitHub Actions/workflow files.

## Verification

Use PostgreSQL 17 actual-entry tests for same backend/XID, catalog and lower
gate contention, role/ACL/catalog drift, exact operation reuse/conflict,
positive-only readback and absent=unknown, cancellation before and during
the transaction, bounded supervisor capacity/drain, and no premature effect
terminalization. Cover the S NOLOGIN/migrator SET ROLE fixture or keep the
provisioning premise explicit. Run affected SQLite/legacy regressions,
formatting, strict scoped Clippy and an independent exact-head review. Record
baseline lint limits separately. Exact-release CI and deployment remain open
until #2046 is complete and `cuenv sync -A` restores the workflows.
