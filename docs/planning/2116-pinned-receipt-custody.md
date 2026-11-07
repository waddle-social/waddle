# P2.1 dark pinned receipt custody

Status: dark, test-selected implementation for #2116 and #2046. Both
restricted-startup refusals remain active; this work cannot enable serving.

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

The PostgreSQL 17 actual-producer fixture commits and reconstructs an effect
intent under the registered guard before installing the candidate restricted
receipt profile. It then exercises the real executor handoff to the pinned
receipt pool, same-backend/XID observation, caller cancellation beyond five
seconds, exact positive readback after simulated acknowledgement loss, an
absent-row unknown outcome, capacity retention, and refusal to terminalize
through the legacy path. The existing pinned authority fixture separately
covers catalog/subordinate contention, role and catalog drift, actual COMMIT
reply loss, exact operation conflict, and positive-only reconciliation. The
SQLite receipt journal regression and ordinary live-receipt regression protect
their existing paths. These are dark tests, not one shared main-pool serving
proof. The S NOLOGIN/migrator SET ROLE graph remains a provisioning and
quiescence premise, not a runtime guarantee of this selector.

Focused PostgreSQL 17 producer tests passed 3/3; the existing pinned receipt
test passed 1/1, SQLite receipt journal 2/2 and ordinary live-receipt 1/1.
Formatting, diff check and independent exact-tree source review passed. Strict
all-features Clippy with `-D warnings` reports 21 preexisting unused/dead-code
diagnostics outside this diff; the new probe warning was fixed before the
final check. GitHub Actions and workflows remain disabled, so there is no
hosted CI result for this group.

Process-crash restart cannot retire a surviving original backend or release
its retained capacity yet. Other stores, old writers, provider role changes,
full role graph, production SM custody, terminalization, serving activation,
exact-release CI and deployment remain open. GitHub Actions/workflow files
remain disabled until #2046 is complete and `cuenv sync -A` restores them.
